"""Measure one real project music seed change, including authorization and archive reopen.

This diagnostic produces unapproved resource evidence, never a production profile.
Run --self-check without acquiring the model lock or executing a native process.
"""

import argparse
from contextlib import ExitStack, closing
import hashlib
import json
import os
from pathlib import Path, PureWindowsPath
import re
import sqlite3
import sys
import time

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[1]
BASE = ROOT / "target/lifecycle-validation"
TOOLS = ROOT / "crates/lumina-local-inference/src/lifecycle/task_resources"
PARENT = BASE / "m4-project-music-20261115-r1"
ASSET = "0024354d-3a4a-83a4-9900-0ab7c1f96d1c"
PROJECT = "project_c2c2019b-c741-4db8-9e5f-10e3d91b039b"
CONTENT = "f69a343dfa9a14a69b2063d5100bb566d52cd6acfbf78741cc582f14c3d1c9ec"
PARENT_WAV = "afa7d6699180ed7c1bd7fdd4524af6b4faf15b9e077f25d4b70dc5787693f2e5"
HOST_TEST = "media::task_output::tests::project_music_reuse_probe::measure_project_music_reuse"
REQUIRED_HOST_SOURCES = {
    "crates/lumina/tests/support/project_music_reuse_probe.rs",
    "crates/lumina/src/media/task_publication.rs",
    "crates/lumina/src/media/task_service.rs",
    "crates/lumina/src/media/project_task_reuse.rs",
    "crates/lumina/src/media/task_inputs.rs",
    "crates/lumina/src/media/task_configuration.rs",
    "crates/lumina/src/media/task_output.rs",
    "crates/lumina/src/media/project_assets.rs",
    "crates/lumina/src/project/assets.rs", "Cargo.lock",
}


def require(condition, message):
    if not condition:
        raise ValueError(message)


def sha(path):
    digest = hashlib.sha256()
    with Path(path).open("rb") as stream:
        for block in iter(lambda: stream.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


def json_digest(value):
    return hashlib.sha256(json.dumps(value, ensure_ascii=False, separators=(",", ":")).encode()).hexdigest()


def read(path):
    return json.loads(Path(path).read_text(encoding="utf-8"))


def windows_ancestry_path(path):
    value = str(path)
    if re.match(r"^\\\\\?\\[A-Za-z]:\\", value):
        value = value[4:]
    return PureWindowsPath(value)


def resolved_within(path, directory):
    candidate = Path(path).resolve(strict=True)
    parent = Path(directory).resolve(strict=True)
    # Only normalize comparison spelling after resolving junctions/symlinks.
    # Keep the original resolved path for IO, including verbatim path semantics.
    if os.name == "nt":
        return windows_ancestry_path(candidate).is_relative_to(windows_ancestry_path(parent))
    return candidate.is_relative_to(parent)


def new_json(path, value):
    with Path(path).open("x", encoding="utf-8", newline="\n") as stream:
        json.dump(value, stream, ensure_ascii=False, indent=2)
        stream.write("\n")
        stream.flush()
        os.fsync(stream.fileno())


def validate_parent_report(report):
    require(report.get("status") == "PASS", "Parent acceptance did not pass")
    for name in ("realBudgetAdmission", "sourceSessionDeleted", "sqliteReopened",
                 "archiveConfigurationVerified", "originalMediaSessionIsolationPreserved"):
        require(report.get(name) is True, "Missing parent acceptance: " + name)
    result = report["actualResult"]
    require(result["resultId"] == ASSET and report["projectId"] == PROJECT
            and report["archiveContentSha256"] == CONTENT
            and report["verifiedWavSha256"] == result["sha256"] == PARENT_WAV
            and result["bytes"] == 705644, "Parent identity differs from retained acceptance")
    require(report["sourceSessionId"] == result["sessionId"] and report["jobId"] == result["jobId"],
            "Parent report scope is inconsistent")


def parent_row(connection, report):
    row = connection.execute(
        "SELECT project_id,source_session_id,job_id,content_json,content_sha256 "
        "FROM project_assets_v1 WHERE asset_id=?", (ASSET,)).fetchone()
    require(row is not None, "Parent project asset is missing")
    project, session, job, content, digest = row
    raw = content.encode("utf-8")
    require(project == PROJECT and session == report["sourceSessionId"] and job == report["jobId"]
            and digest == CONTENT == hashlib.sha256(raw).hexdigest(), "Parent archive identity changed")
    require(connection.execute("SELECT 1 FROM sessions WHERE id=?", (session,)).fetchone() is None,
            "Parent source session must remain deleted")
    archive = json.loads(content)
    require(archive["kind"] == "media" and archive["result"] == report["actualResult"],
            "Archive output differs from the accepted parent")
    configuration = archive["configuration"]
    require(hashlib.sha256(configuration["snapshotJson"].encode()).hexdigest()
            == configuration["snapshotSha256"] == report["snapshotSha256"], "Parent snapshot hash mismatch")
    require(json.loads(configuration["snapshotJson"])["parameters"]["seed"] == 42,
            "Expected independently accepted seed 42 parent")
    return len(raw)


def readonly_database(path):
    # immutable=1 would silently omit committed pages still held in the source WAL.
    connection = sqlite3.connect(path.resolve(strict=True).as_uri() + "?mode=ro", uri=True)
    connection.execute("PRAGMA query_only=ON")
    return connection


def source_evidence():
    report_path = PARENT / "result.json"
    report = read(report_path)
    validate_parent_report(report)
    wav = Path(report["publishedPath"]).resolve(strict=True)
    require(resolved_within(wav, PARENT) and sha(wav) == PARENT_WAV,
            "Original WAV location or bytes changed")
    database = PARENT / "session-storage/sessions/sessions.db"
    paths = [report_path, database, wav]
    wal = database.with_name(database.name + "-wal")
    if wal.exists():
        paths.append(wal)
    return report, database, {str(path): sha(path) for path in paths}


def unchanged(evidence):
    for path, expected in evidence.items():
        require(Path(path).is_file() and sha(path) == expected, "Original evidence changed: " + path)


def prepare_backup(run, report, database):
    target = run / "output-root/publication/sessions/sessions.db"
    target.parent.mkdir(parents=True)
    with closing(readonly_database(database)) as source:
        source.execute("BEGIN")
        archive_bytes = parent_row(source, report)
        with closing(sqlite3.connect(target)) as destination:
            source.backup(destination)
            require(destination.execute("PRAGMA integrity_check").fetchone() == ("ok",), "Backup integrity failed")
            require(parent_row(destination, report) == archive_bytes, "Backup parent differs")
        source.rollback()
    case = dict(schemaVersion=1, parentReportPath=str(PARENT / "result.json"),
                parentReportSha256=sha(PARENT / "result.json"), parentAssetId=ASSET,
                parentAssetContentSha256=CONTENT, parentProjectId=PROJECT,
                parentSessionId=report["sourceSessionId"], parentJobId=report["jobId"],
                parentOutputSha256=PARENT_WAV, backupDatabaseSha256=sha(target), patch={"seed": 1})
    new_json(run / "project-reuse-case.json", case)
    return target, archive_bytes


def publication_context():
    path = ROOT / "crates/lumina/src/media/task_publication.rs"
    source = path.read_text(encoding="utf-8")
    match = re.search(r"let sources = sources!\((.*?)\);", source, re.S)
    require(match is not None, "Cannot identify authoritative publication source list")
    names = re.findall(r'"([^"\r\n]+)"', match.group(1))
    require(names and len(set(names)) == len(names), "Invalid publication source list")
    sources = []
    for name in names:
        target = (path.parent / name).resolve(strict=True)
        require(target.is_relative_to(ROOT), "Publication source leaves repository")
        normalized = target.read_bytes().decode("utf-8").replace("\r\n", "\n").encode("utf-8")
        sources.append([name, hashlib.sha256(normalized).hexdigest()])
    return {"kind": "project_parameter_reuse_v1", "implementationSourcesDigest": json_digest(sources)}


def validate_sources(document, label):
    sources = document.get("sources")
    require(isinstance(sources, dict) and sources, label + " lacks source fingerprints")
    for relative, expected in sources.items():
        path = (ROOT / relative).resolve(strict=True)
        require(path.is_relative_to(ROOT) and sha(path) == expected, label + " source mismatch: " + relative)
    return sources


def validate_reports(run, expected_context, case_sha, archive_bytes):
    fixture, driver, native, pub = [read(run / name) for name in
                                   ("fixture.json", "driver.json", "owner.json", "publication.json")]
    request_path = run / "project-reuse-owner.json"
    require(0 < request_path.stat().st_size <= 1 << 20, "Owner request exceeds bound")
    request, request_sha = read(request_path), sha(request_path)
    require(request["patch"] == {"seed": 1} and request["parentAssetId"] == ASSET
            and request["parentAssetContentSha256"] == CONTENT
            and request["implementationSourcesDigest"] == expected_context["implementationSourcesDigest"],
            "Owner request changes parent, patch or publication identity")
    for document in (fixture, driver, native, pub):
        require(document.get("projectReuseRequestSha256") == request_sha
                and document.get("publicationContext") == expected_context, "Probe request/context mismatch")
    for document in (driver, pub):
        require(document["caseSha256"] == case_sha and document["parentAssetBytes"] == archive_bytes
                and type(document["authorizationBytes"]) is int and 0 < document["authorizationBytes"] <= 1 << 20
                and type(document["preparationMillis"]) is int and document["preparationMillis"] >= 0,
                "Host case or authorization footprint mismatch")
    require(driver["authorizationBytes"] == pub["authorizationBytes"]
            and driver["preparationMillis"] == pub["preparationMillis"], "Host preparation reports disagree")
    host_sources = validate_sources(driver, "host")
    require(REQUIRED_HOST_SOURCES.issubset(host_sources), "Missing project publication source fingerprints")
    require(validate_sources(pub, "publication") == host_sources, "Host source maps disagree")
    validate_sources(native, "owner")
    require(pub["implementationSourcesSha256"] == json_digest(dict(sorted(host_sources.items()))),
            "Publication implementation source digest mismatch")
    require(fixture["snapshot"] == read(run / "snapshot.json") and fixture["key"]["kind"] == "music"
            and fixture["key"]["requiresGpu"] is False and fixture["snapshot"]["parameters"]["seed"] == 1,
            "Expected unchanged-shape CPU music child snapshot")
    require(fixture["snapshotDigest"] == native["snapshotDigest"] == pub["snapshotDigest"]
            == request["expectedChildSnapshotDigest"], "Child snapshot digest mismatch")
    require(pub["reopenedVerified"] is True and driver["implementation"] == pub["implementation"]
            == "media::task_output::import_readers", "Missing production publication/reopen")
    for flag in ("verified", "reopenedVerified", "parentUnchanged", "lineageVerified", "sourceSessionAbsent"):
        require(pub["projectArchive"].get(flag) is True, "Project archive check failed: " + flag)
    published = Path(pub["publishedPath"]).resolve(strict=True)
    require(resolved_within(published, run / "output-root/publication"),
            "Child output leaves measurement publication root")
    wav = sha(run / "output-root/native/output.wav")
    require(wav == native["outputSha256"] == pub["inputSha256"] == pub["publishedSha256"]
            == pub["result"]["sha256"] == sha(published) and wav != PARENT_WAV,
            "Generated, handed-off, published and reopened audio differ or unchanged seed output")
    require(native["outputBytes"] == pub["publishedBytes"] == pub["result"]["bytes"]
            == published.stat().st_size == (run / "output-root/native/output.wav").stat().st_size == 705644
            and pub["result"]["mime"] == "audio/wav", "Invalid four-second output")
    spec = pub["result"]["spec"]
    require(spec == dict(kind="audio", durationMicros=4000000, sampleRate=44100, channels=2, sampleFrames=176400),
            "Published output shape changed")
    require(pub["result"]["sessionId"] == request["childSessionId"]
            and pub["result"]["jobId"] == request["childJobId"], "Child result scope mismatch")
    return fixture, native, pub, request_sha, wav


def execute(args):
    require(os.name == "nt", "Windows Job measurement requires Windows")
    run = args.run_dir.resolve()
    require(run.parent == BASE.resolve(strict=True) and run.drive.upper() in ("D:", "E:")
            and not run.exists(), "Run directory must be a new direct lifecycle-validation child on D/E")
    deps = (ROOT / "target/debug/deps").resolve(strict=True)
    host, owner = (path.resolve(strict=True) for path in (args.publication_probe, args.owner_probe))
    for path, prefix, digest in ((host, "lumina-", args.publication_probe_sha256),
                                 (owner, "lumina_local_inference-", args.owner_probe_sha256)):
        require(path.parent == deps and path.name.startswith(prefix) and path.suffix == ".exe"
                and re.fullmatch(r"[0-9a-fA-F]{64}", digest), "Unexpected test executable or SHA")
    require("LUMINA_TASK_RESOURCE_VERIFY" not in os.environ,
            "Remove unrelated frozen-budget measurement environment before starting")
    run.mkdir()
    sys.path.insert(0, str(TOOLS))
    import measure_music as music
    import measure_sd15 as monitors
    import windows_job_measurement as jobs
    from measure_pocket_tts import tree_bytes, write_json

    class ProjectMonitor(music.MusicMonitor):
        def __init__(self, *arguments):
            super().__init__(*arguments)
            self.application_bytes = 0

        def sample(self, process):
            self.application_bytes = max(self.application_bytes, tree_bytes(run / "application"))
            return super().sample(process)

        def finish(self, end, host_binary, owner_binary):
            # Retained handles expose OS peaks reached between sampling and exit.
            for row in self.members.values():
                final = self.read_memory(row["proc"])
                row["ws"] = max(row["ws"], final["PeakWorkingSetSize"])
                row["commit"] = max(row["commit"], final["PeakPagefileUsage"])
            return super().finish(end, host_binary, owner_binary)

    report = dict(status="PREPARING", scope="One real project music seed 42 to 1 reuse, authorization, publication and reopen; not prompt range, natural-language or desktop acceptance",
                  runnerSha256=sha(__file__), processes=[], reviewedBudget=None,
                  publicationProbeSha256=args.publication_probe_sha256.lower(), ownerProbeSha256=args.owner_probe_sha256.lower(),
                  limitations=["Job assignment follows spawn; no atomic spawn/assignment claim",
                               "OS per-process peak working sets are summed conservatively, not sampled simultaneous physical RAM",
                               "Directory high-water samples may miss files created and removed between samples",
                               "Source database backup is outside measured host work; actual authorization/preparation/save/reopen is inside"])
    evidence = None
    monitor = None
    started = time.monotonic()
    def persist():
        write_json(run / "result.json", report)
    persist()
    try:
        jobs.RUN_ROOT = BASE / "m4-managed-audio-install-20261113-r1"
        os.environ.pop("LUMINA_MUSIC_DOMAIN_CASE_FILE", None)
        jobs.ENVIRONMENT.pop("LUMINA_MUSIC_DOMAIN_CASE_FILE", None)
        jobs.ENVIRONMENT.update(LUMINA_TASK_RESOURCE_OWNER=str(owner), LUMINA_TASK_RESOURCE_RUN_DIR=str(run),
                                LUMINA_AUDIO_MUSIC_CPU_CLI=str(music.ENGINE), LUMINA_PATH_ROOT=str(run / "application"),
                                LUMINA_PROJECT_MUSIC_REUSE="measurement", RUST_BACKTRACE="1", TMP=str(run), TEMP=str(run))
        monitors.ENGINE = music.ENGINE.resolve(strict=True)
        report["measurementHelpers"] = {str(Path(module.__file__).relative_to(ROOT)): sha(module.__file__)
                                         for module in (music, monitors, jobs, sys.modules["measure_pocket_tts"])}
        api = jobs.WindowsApi()
        with ExitStack() as stack:
            stack.enter_context(jobs.exclusive_run())
            for path, expected in ((host, args.publication_probe_sha256), (owner, args.owner_probe_sha256),
                                   (music.ENGINE, music.HELPER_SHA)):
                stack.enter_context(api.read_guard(path))
                require(sha(path) == expected.lower(), "Executable identity mismatch: " + str(path))
            parent_report, database, evidence = source_evidence()
            report["originalEvidence"] = evidence
            backup, archive_bytes = prepare_backup(run, parent_report, database)
            unchanged(evidence)
            context = publication_context()
            case_sha = sha(run / "project-reuse-case.json")
            (run / "output-root/native").mkdir(parents=True)
            report.update(publicationContext=context, caseSha256=case_sha, parentAssetBytes=archive_bytes,
                          publicationBaselineBytes=tree_bytes(run / "output-root/publication"))
            monitor = ProjectMonitor(run, api, stack)
            monitor.install()
            report["status"] = "RUNNING"
            persist()
            call = jobs.run_process(api, [str(host), "--exact", HOST_TEST, "--ignored", "--nocapture"],
                                    run, "lifecycle", 1200, report, persist,
                                    [(run / "output-root/native/output.wav", 32 << 20)])
            require(call["processTreeExitConfirmed"] and not call.get("terminatedOwnedJob") and call["exitCode"] == 0
                    and call["memoryQueryFailures"] == 0
                    and f"test {HOST_TEST} ... ok" in Path(call["stdout"]).read_text(encoding="utf-8").splitlines(),
                    "Project music host did not execute and pass the exact test")
            end = call["jobAfterCleanup"]
            main, engine = monitor.finish(end, host, owner)
            fixture, native, pub, request_sha, wav = validate_reports(run, context, case_sha, archive_bytes)
            require(context == publication_context() and sha(run / "project-reuse-case.json") == case_sha,
                    "Context or immutable case changed during measurement")
            with closing(readonly_database(backup)) as connection:
                require(parent_row(connection, parent_report) == archive_bytes, "Backup parent changed after reuse")
            unchanged(evidence)
            require(sha(__file__) == report["runnerSha256"], "Controller source changed during measurement")
            for relative, expected in report["measurementHelpers"].items():
                require(sha(ROOT / relative) == expected, "Measurement helper source changed: " + relative)
            samples = sha(run / "tree.jsonl")
            native_peak = max(native["outputBytes"], monitor.output_bytes)
            publication_peak = max(monitor.publication_bytes, *(item["directoryLogicalBytes"] for item in pub["checkpoints"]))
            def metrics(entry, disk):
                pid, row = entry
                return dict(pid=pid, peakWorkingSetBytes=row["ws"], peakCommitBytes=row["commit"],
                            elapsedMillis=max(1, (row["finish"] - row["start"] + 9999) // 10000), exitCode=0,
                            processTreeExited=True, memorySamplesSha256=samples, observedOutputPeakBytes=disk)
            artifacts = ("fixture.json", "driver.json", "owner.json", "publication.json", "tree.jsonl",
                         "snapshot.json", "project-reuse-case.json", "project-reuse-owner.json")
            report.update(status="MEASURED_UNREVIEWED", job=end, projectReuseRequestSha256=request_sha,
                          nativeDirectoryHighWaterBytes=monitor.native_bytes, nativeOutputPeakBytes=native_peak,
                          publicationDirectoryHighWaterBytes=publication_peak, applicationDirectoryHighWaterBytes=monitor.application_bytes,
                          controllerPeakCommitBytes=monitor.controller_peak, authorizationBytes=pub["authorizationBytes"],
                          preparationMillis=pub["preparationMillis"], originalEvidenceUnchanged=True,
                          members={pid: {key: str(value) if key == "path" else value for key, value in row.items() if key != "proc"}
                                   for pid, row in monitor.members.items()}, artifacts={name: sha(run / name) for name in artifacts})
            new_json(run / "execution-evidence.json", report)
            measurement = dict(schemaVersion=2, rulesVersion="native-task-resources-v1", key=fixture["key"],
                               provenance=dict(runnerSha256=report["runnerSha256"], fixtureSha256=sha(run / "fixture.json"),
                                               executionEvidenceSha256=sha(run / "execution-evidence.json")),
                               snapshotDigest=fixture["snapshotDigest"], hardware=fixture["hardware"],
                               controllerPeakCommitBytes=monitor.controller_peak,
                               lifecycleTree=dict(processIds=sorted(monitor.members), peakJobCommitBytes=end["peakJobCommitBytes"],
                                                  summedPeakWorkingSetBytes=sum(row["ws"] for row in monitor.members.values()), samplesSha256=samples),
                               native=metrics(engine, native_peak), publication=dict(kind="media", process=metrics(main, publication_peak),
                                   implementation=pub["implementation"], implementationSourcesSha256=pub["implementationSourcesSha256"],
                                   helperBinarySha256=sha(host), inputSha256=wav, publishedSha256=wav, publishedBytes=705644,
                                   peakDirectoryBytes=publication_peak, reopenedVerified=True), gpu={"status": "cpu_only"},
                               nativeExitedBeforePublication=True, reviewedBudget=None)
            new_json(run / "measurement.json", measurement)
            report.update(status="MEASURED_REVIEW_REQUIRED", measurementSha256=sha(run / "measurement.json"))
    except BaseException as error:
        report.update(status="FAILED", error=repr(error))
        raise
    finally:
        if monitor is not None:
            report["observedHighWater"] = dict(controllerPeakCommitBytes=monitor.controller_peak,
                                              nativeDirectoryBytes=monitor.native_bytes,
                                              nativeOutputBytes=monitor.output_bytes,
                                              publicationDirectoryBytes=monitor.publication_bytes,
                                              applicationDirectoryBytes=monitor.application_bytes)
        if evidence is not None:
            try:
                unchanged(evidence)
                report["originalEvidenceUnchanged"] = True
            except Exception as error:
                report.update(status="FAILED", originalEvidenceUnchanged=False, originalEvidenceError=str(error))
        report["controllerElapsedMillis"] = round((time.monotonic() - started) * 1000)
        persist()
    require(report["status"] == "MEASURED_REVIEW_REQUIRED", "Measurement did not complete cleanly")


def self_check():
    report = dict(status="PASS", projectId=PROJECT, archiveContentSha256=CONTENT, verifiedWavSha256=PARENT_WAV,
                  sourceSessionId="source", jobId="job", realBudgetAdmission=True, sourceSessionDeleted=True,
                  sqliteReopened=True, archiveConfigurationVerified=True, originalMediaSessionIsolationPreserved=True,
                  actualResult=dict(resultId=ASSET, sha256=PARENT_WAV, bytes=705644, sessionId="source", jobId="job"))
    validate_parent_report(report)
    for key, value in (("status", "FAILED"), ("sourceSessionDeleted", False), ("projectId", "other")):
        changed = dict(report, **{key: value})
        try:
            validate_parent_report(changed)
        except ValueError:
            continue
        raise AssertionError("Accepted invalid parent " + key)
    require(json_digest(["x", 1]) == hashlib.sha256(b'["x",1]').hexdigest(), "Canonical digest mismatch")
    context = publication_context()
    require(re.fullmatch(r"[0-9a-f]{64}", context["implementationSourcesDigest"]), "Invalid context digest")
    root = "D:\\Project\\goose\\target\\lifecycle-validation\\parent"
    for candidate, directory, expected in (
        ("\\\\?\\" + root + "\\output.wav", root, True),
        (root + "\\output.wav", "\\\\?\\" + root, True),
        ("\\\\?\\" + root + "-sibling\\output.wav", root, False),
        ("\\\\?\\D:\\outside\\output.wav", root, False),
    ):
        require(windows_ancestry_path(candidate).is_relative_to(windows_ancestry_path(directory)) is expected,
                "Normal/verbatim path ancestry check failed")
    print(json.dumps(dict(status="PASS", checks=10, scope="Pure parent rejection, serialization, path ancestry and local publication source checks; no database backup, Job lock or model execution", publicationContext=context)))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--self-check", action="store_true")
    for key in ("publication-probe", "owner-probe", "run-dir"):
        parser.add_argument("--" + key, type=Path)
    for key in ("publication-probe-sha256", "owner-probe-sha256"):
        parser.add_argument("--" + key)
    args = parser.parse_args()
    if args.self_check:
        require(not any(value for name, value in vars(args).items() if name != "self_check"), "Self-check cannot execute a run")
        self_check()
    else:
        require(all(value for name, value in vars(args).items() if name != "self_check"), "All probe paths, SHA256 values and run directory are required")
        execute(args)


if __name__ == "__main__":
    main()
