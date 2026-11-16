"""Measure project music parameter reuse, including authorization and archive reopen.

This diagnostic produces unapproved resource evidence, never a production profile.
Run --self-check without acquiring the model lock or executing a native process.
"""

import argparse
from contextlib import ExitStack, closing
import ctypes
from ctypes import wintypes
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
METADATA_TEST = "media::task_output::tests::project_music_reuse_probe::metadata_fixture::prepare_project_music_metadata_fixture"
PRODUCTION_TEST = "media::task_output::tests::project_music_reuse_probe::production::verify_project_music_reuse_production"
# These are enforced candidate payload ceilings, not measured resource budgets.
SMALL_SOURCE_ASSETS_LIMIT = 256 * 1024
SMALL_RECORD_LIMIT = 256 * 1024
SMALL_OUTPUT_ARCHIVE_LIMIT = 1024 * 1024
REQUIRED_HOST_SOURCES = {
    "crates/lumina/tests/support/project_music_reuse_probe.rs",
    "crates/lumina/src/media/task_publication.rs",
    "crates/lumina/src/media/task_service.rs",
    "crates/lumina/src/media/project_task_reuse.rs",
    "crates/lumina/src/media/task_inputs.rs",
    "crates/lumina/src/media/task_configuration.rs",
    "crates/lumina/src/media/task_output.rs",
    "crates/lumina/src/media/project_assets.rs",
    "crates/lumina/src/media/project_payload.rs",
    "crates/lumina/tests/support/project_music_metadata_fixture.rs",
    "crates/lumina/tests/support/project_music_reuse_production.rs",
    "crates/lumina-local-inference/src/lifecycle/task_resources.rs",
    "crates/lumina-local-inference/src/lifecycle/task_resources/music_conditions.rs",
    "crates/lumina-local-inference/src/lifecycle/task_resources/music_probe.rs",
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


def same_resolved_path(left, right):
    left, right = Path(left).resolve(strict=True), Path(right).resolve(strict=True)
    return (windows_ancestry_path(left) == windows_ancestry_path(right)) if os.name == "nt" else left == right


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


def parent_row(connection, report, limit=SMALL_SOURCE_ASSETS_LIMIT):
    asset_id = report["actualResult"]["resultId"]
    row = connection.execute(
        "SELECT project_id,source_session_id,job_id,content_json,content_sha256 "
        "FROM project_assets_v1 WHERE asset_id=?", (asset_id,)).fetchone()
    require(row is not None, "Parent project asset is missing")
    project, session, job, content, digest = row
    raw = content.encode("utf-8")
    require(0 < len(raw) <= limit, "Parent archive exceeds its source payload ceiling")
    require(project == report["projectId"] and session == report["sourceSessionId"] and job == report["jobId"]
            and digest == report["archiveContentSha256"] == hashlib.sha256(raw).hexdigest(), "Parent archive identity changed")
    require(connection.execute("SELECT 1 FROM sessions WHERE id=?", (session,)).fetchone() is None,
            "Parent source session must remain deleted")
    archive = json.loads(content)
    require(archive["kind"] == "media" and archive["result"] == report["actualResult"],
            "Archive output differs from the accepted parent")
    configuration = archive["configuration"]
    require(hashlib.sha256(configuration["snapshotJson"].encode()).hexdigest()
            == configuration["snapshotSha256"] == report["snapshotSha256"], "Parent snapshot hash mismatch")
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


def validate_domain_case(case):
    require(isinstance(case, dict) and set(case).issubset({"schemaVersion", "caseId", "patch", "metadataPressure"})
            and type(case.get("schemaVersion")) is int and case["schemaVersion"] == 1
            and isinstance(case.get("caseId"), str) and re.fullmatch(r"[a-z0-9][a-z0-9-]{0,63}", case["caseId"]),
            "Invalid domain case identity or schema")
    patch = case.get("patch")
    require(isinstance(patch, dict) and len(patch) == 1, "Domain case changes exactly one parameter")
    if "seed" in patch:
        require(type(patch["seed"]) is int and 0 <= patch["seed"] <= 0xffffffff, "Seed is not a u32")
    else:
        prompt = patch.get("prompt")
        require(isinstance(prompt, str) and prompt.strip() and 0 < len(prompt.encode("utf-8")) <= 16384
                and "\0" not in prompt and "|" not in prompt
                and not (prompt.startswith("--") and not any(c.isspace() for c in prompt)),
                "Prompt is outside the single-prompt contract")
    if "metadataPressure" in case:
        pressure = case["metadataPressure"]
        require(isinstance(pressure, dict) and set(pressure) == {"referenceCount", "messageBytes"}
                and type(pressure["referenceCount"]) is int and 1 <= pressure["referenceCount"] <= 128
                and type(pressure["messageBytes"]) is int and 8 <= pressure["messageBytes"] <= 1024,
                "Metadata fixture exceeds the supported API reference limits")
    return case


def exported_parent_report(source):
    require(source.get("schemaVersion") == 1 and source.get("kind") == "project_music_reuse_source"
            and source.get("provenanceKind") in ("project_measurement", "api_metadata_fixture", "project_production")
            and source.get("sourceSessionDeleted") is True and source.get("projectArchiveReopenedVerified") is True,
            "Source has no verified retained-project provenance")
    result = source["result"]
    require(result["resultId"] == source["assetId"] and result["sessionId"] == source["sourceSessionId"]
            and result["jobId"] == source["jobId"] and result["sha256"] == source["outputSha256"]
            and result["bytes"] == source["outputBytes"] == 705644 and result["mime"] == "audio/wav"
            and result["spec"] == dict(kind="audio", durationMicros=4000000, sampleRate=44100, channels=2, sampleFrames=176400),
            "Source result differs from exported identity")
    return dict(actualResult=result, projectId=source["projectId"], sourceSessionId=source["sourceSessionId"],
                jobId=source["jobId"], archiveContentSha256=source["assetContentSha256"],
                verifiedWavSha256=source["outputSha256"], snapshotSha256=source["snapshotSha256"])


def validate_export(source, directory):
    report = exported_parent_report(source)
    database = Path(source["databasePath"]).resolve(strict=True)
    wav = Path(source["publishedPath"]).resolve(strict=True)
    expected_database = directory / "output-root/publication/sessions/sessions.db"
    require(same_resolved_path(database, expected_database)
            and resolved_within(wav, directory / "output-root/publication")
            and sha(wav) == source["outputSha256"] and wav.stat().st_size == 705644,
            "Exported database or audio differs from its run")
    database = expected_database.resolve(strict=True)
    with closing(readonly_database(database)) as connection:
        parent_row(connection, report, 32 << 20)
    return report, database, wav


def exported_source_evidence(path):
    path = path.resolve(strict=True)
    directory = path.parent
    require(path.name == "source.json" and directory.parent == BASE.resolve(strict=True)
            and path.stat().st_size <= 2 << 20, "Source must be a bounded export from a validation run")
    source = read(path)
    proof = Path(source["proofPath"]).resolve(strict=True)
    require(same_resolved_path(proof.parent, directory) and proof.stat().st_size <= 8 << 20
            and sha(proof) == source["proofSha256"], "Source proof identity changed")
    document = read(proof)
    evidence = {str(path): sha(path), str(proof): sha(proof)}
    if source["provenanceKind"] == "project_measurement":
        require(proof.name == "result.json" and document["status"] == "MEASURED_REVIEW_REQUIRED"
                and document["originalEvidenceUnchanged"] is True, "Source project execution did not pass")
        publication = directory / "publication.json"
        measurement = directory / "measurement.json"
        require(sha(publication) == document["artifacts"]["publication.json"]
                and sha(measurement) == document["measurementSha256"], "Source execution artifacts changed")
        exported = read(publication)["exportSource"]
        measured = read(measurement)
        require(measured["key"]["kind"] == "music" and measured["publication"]["reopenedVerified"] is True
                and measured["publication"]["publishedSha256"] == source["outputSha256"], "Source measurement output mismatch")
        evidence.update({str(publication): sha(publication), str(measurement): sha(measurement)})
    else:
        require(source["provenanceKind"] == "api_metadata_fixture" and proof.name == "fixture-result.json"
                and document["status"] == "API_METADATA_FIXTURE_READY" and document["generatedNewAudio"] is False,
                "Metadata setup must not claim a new native execution")
        exported = document["exportSource"]
    require({key: value for key, value in source.items() if key not in ("proofPath", "proofSha256", "provenanceKind")}
            == exported, "Source export differs from its proof")
    report, database, wav = validate_export(source, directory)
    evidence.update({str(database): sha(database), str(wav): sha(wav)})
    wal = database.with_name(database.name + "-wal")
    if wal.exists():
        evidence[str(wal)] = sha(wal)
    return report, database, evidence, source["provenanceKind"]


def export_source(run, exported, kind, proof):
    source = dict(exported, schemaVersion=1, kind="project_music_reuse_source", provenanceKind=kind,
                  proofPath=str(proof), proofSha256=sha(proof))
    validate_export(source, run)
    new_json(run / "source.json", source)


def prepare_backup(run, report, database, domain_case=None, parent_report_path=None, proof_kind="normal_tool"):
    target = run / "output-root/publication/sessions/sessions.db"
    target.parent.mkdir(parents=True)
    with closing(readonly_database(database)) as source:
        source.execute("BEGIN")
        source_limit = 32 << 20 if domain_case is not None else SMALL_SOURCE_ASSETS_LIMIT
        archive_bytes = parent_row(source, report, source_limit)
        with closing(sqlite3.connect(target)) as destination:
            source.backup(destination)
            require(destination.execute("PRAGMA integrity_check").fetchone() == ("ok",), "Backup integrity failed")
            require(parent_row(destination, report, source_limit) == archive_bytes, "Backup parent differs")
        source.rollback()
    parent_report_path = parent_report_path or PARENT / "result.json"
    case = dict(schemaVersion=1, parentReportPath=str(parent_report_path),
                parentReportSha256=sha(parent_report_path), parentAssetId=report["actualResult"]["resultId"],
                parentAssetContentSha256=report["archiveContentSha256"], parentProjectId=report["projectId"],
                parentSessionId=report["sourceSessionId"], parentJobId=report["jobId"],
                parentOutputSha256=report["verifiedWavSha256"], backupDatabaseSha256=sha(target), patch={"seed": 1})
    if domain_case is not None:
        case.update(schemaVersion=2, parentProofKind=proof_kind, patch=domain_case["patch"], caseId=domain_case["caseId"])
        if "metadataPressure" in domain_case:
            case["metadataPressure"] = domain_case["metadataPressure"]
    new_json(run / "project-reuse-case.json", case)
    return target, archive_bytes


def payload_limits(tier):
    require(tier in ("small", "standard"), "Unknown server-selected payload tier")
    return ([SMALL_SOURCE_ASSETS_LIMIT, SMALL_RECORD_LIMIT, SMALL_OUTPUT_ARCHIVE_LIMIT]
            if tier == "small" else [64 << 20, 1 << 20, 32 << 20])


def publication_context(tier="small"):
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
    limits = payload_limits(tier)
    return {"kind": "project_parameter_reuse_v2", "implementationSourcesDigest": json_digest([sources, limits]),
            "payloadTier": tier}


def validate_sources(document, label):
    sources = document.get("sources")
    require(isinstance(sources, dict) and sources, label + " lacks source fingerprints")
    for relative, expected in sources.items():
        path = (ROOT / relative).resolve(strict=True)
        require(path.is_relative_to(ROOT) and sha(path) == expected, label + " source mismatch: " + relative)
    return sources


def validate_reports(run, expected_context, case_sha, archive_bytes, domain=False):
    fixture, driver, native, pub = [read(run / name) for name in
                                   ("fixture.json", "driver.json", "owner.json", "publication.json")]
    request_path = run / "project-reuse-owner.json"
    require(0 < request_path.stat().st_size <= 1 << 20, "Owner request exceeds bound")
    request, request_sha = read(request_path), sha(request_path)
    case = read(run / "project-reuse-case.json")
    source_limit, record_limit, output_limit = payload_limits(expected_context["payloadTier"])
    require(request["payloadTier"] == expected_context["payloadTier"] and request["patch"] == case["patch"]
            and request["parentAssetId"] == case["parentAssetId"]
            and request["parentAssetContentSha256"] == case["parentAssetContentSha256"]
            and request["implementationSourcesDigest"] == expected_context["implementationSourcesDigest"],
            "Owner request changes parent, patch or publication identity")
    if domain:
        require(request.get("caseSha256") == case_sha and fixture.get("caseSha256") == case_sha
                and native.get("caseSha256") == case_sha, "Owner did not bind the domain case")
    else:
        require(case["patch"] == {"seed": 1} and case["parentAssetId"] == ASSET
                and case["parentAssetContentSha256"] == CONTENT
                and request["parentSnapshot"]["parameters"]["seed"] == 42, "Legacy parent/patch changed")
    for document in (fixture, driver, native, pub):
        require(document.get("projectReuseRequestSha256") == request_sha
                and document.get("publicationContext") == expected_context, "Probe request/context mismatch")
    for document in (driver, pub):
        require(document["caseSha256"] == case_sha and document["parentAssetBytes"] == archive_bytes
                and type(document["parentAssetBytes"]) is int and 0 < document["parentAssetBytes"] <= source_limit
                and type(document["authorizationBytes"]) is int and 0 < document["authorizationBytes"] <= record_limit
                and type(document["preparationMillis"]) is int and document["preparationMillis"] >= 0,
                "Host case or authorization footprint mismatch")
    require(type(pub["outputArchiveBytes"]) is int and 0 < pub["outputArchiveBytes"] <= output_limit,
            "Output archive exceeds the selected payload tier ceiling")
    require(driver["authorizationBytes"] == pub["authorizationBytes"]
            and driver["preparationMillis"] == pub["preparationMillis"], "Host preparation reports disagree")
    host_sources = validate_sources(driver, "host")
    require(REQUIRED_HOST_SOURCES.issubset(host_sources), "Missing project publication source fingerprints")
    require(validate_sources(pub, "publication") == host_sources, "Host source maps disagree")
    owner_sources = validate_sources(native, "owner")
    require({"crates/lumina-local-inference/src/lifecycle/task_resources.rs",
             "crates/lumina-local-inference/src/lifecycle/task_resources/music_conditions.rs"}.issubset(owner_sources),
            "Owner omits resource-condition implementation fingerprints")
    parameters = dict(request["parentSnapshot"]["parameters"], **case["patch"])
    require(pub["implementationSourcesSha256"] == json_digest(dict(sorted(host_sources.items()))),
            "Publication implementation source digest mismatch")
    require(fixture["snapshot"] == read(run / "snapshot.json") and fixture["key"]["kind"] == "music"
            and fixture["key"]["requiresGpu"] is False and fixture["snapshot"]["parameters"] == parameters,
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
            == pub["result"]["sha256"] == sha(published) and (domain or wav != PARENT_WAV),
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


def validate_production(run, contexts, case_sha, profile_sha, monitor, end, host, engine):
    document = read(run / "production.json")
    case = read(run / "project-reuse-case.json")
    require(document["status"] == "PASS_NORMAL_TOOL_ENTRY" and document["caseSha256"] == case_sha
            and document["resourceEvidenceSha256"] == profile_sha,
            "Normal entry did not use the expected case and registered profile")
    for flag in ("parentUnchanged", "lineageVerified", "reopenedVerified", "sourceSessionAbsent",
                 "sameCallReplayed", "nativeCallCompleted"):
        require(document.get(flag) is True, "Normal entry acceptance is missing: " + flag)
    context = document["publicationContext"]
    require(context == contexts.get(context["payloadTier"]), "Normal entry publication context changed")
    require(document["patch"] == case["patch"]
            and document["childParameters"] == dict(document["parentParameters"], **case["patch"]),
            "Normal tool changed parameters outside the requested patch")
    sources = validate_sources(document, "normal tool")
    require(REQUIRED_HOST_SOURCES.issubset(sources)
            and "crates/lumina/tests/support/project_music_reuse_production.rs" in sources,
            "Normal tool omitted compiled source fingerprints")
    require(all(not (run / name).exists() for name in
                ("owner.json", "fixture.json", "measurement.json", "project-reuse-owner.json")),
            "Normal tool entry unexpectedly used the measurement-owner path")
    exported = dict(document["exportSource"], provenanceKind="project_production")
    validate_export(exported, run)
    require(exported["result"] == document["result"]
            and same_resolved_path(exported["publishedPath"], document["publishedPath"]),
            "Normal result differs from the reopened retained output")
    require(end["TotalProcesses"] == len(monitor.members), "Normal entry process observation is incomplete")
    console = (Path(os.environ["SystemRoot"]) / "System32/conhost.exe").resolve(strict=True)
    for row in monitor.members.values():
        require(row["path"] in (host, engine, console), "Unexpected process in normal entry")
        code = wintypes.DWORD()
        monitor.api.check(monitor.api.kernel.GetExitCodeProcess(row["proc"]._handle, ctypes.byref(code)))
        require(code.value == 0 or (row["path"] == console and code.value != 259),
                "Normal entry child did not exit successfully")
    for path in (host, engine):
        require(sum(row["path"] == path for row in monitor.members.values()) == 1,
                "Normal entry did not execute exactly one host and one engine")
    return document


def execute(args):
    require(os.name == "nt", "Windows Job measurement requires Windows")
    domain_case = None
    if args.case_file is not None:
        args.case_file = args.case_file.resolve(strict=True)
        require(args.case_file.is_relative_to(BASE.resolve(strict=True)) and args.case_file.stat().st_size <= 1 << 20,
                "Case must be a bounded local validation fixture")
        domain_case = validate_domain_case(read(args.case_file))
    require(not args.parent_source or domain_case is not None, "A chained parent needs an explicit domain case")
    require(not args.production or (domain_case is not None and not args.prepare_metadata_fixture
                                   and re.fullmatch(r"[0-9a-f]{64}", args.expected_profile_sha256 or "")),
            "Normal entry requires an explicit case and pinned registered profile SHA")
    require(args.production or args.expected_profile_sha256 is None, "Profile SHA is only used for normal entry")
    require(bool(domain_case is not None and "metadataPressure" in domain_case) == args.prepare_metadata_fixture,
            "Metadata pressure is only supported by explicit fixture preparation")
    require(not args.prepare_metadata_fixture or (not args.parent_source and domain_case["patch"] == {"seed": 1}),
            "Metadata fixture preparation uses the independently accepted original source and seed-1 placeholder")
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

    scope = ("Normal native_tasks.project_reuse tool, registered resource admission, actual engine, durable result and idempotent replay; not natural-language or desktop acceptance"
             if args.production else
             "Supported API metadata pressure fixture from an existing real WAV; no new native inference"
             if args.prepare_metadata_fixture else
             "One explicit project music parameter case with real execution and retained cross-session provenance; not a registered budget, quality or desktop acceptance"
             if domain_case is not None else
             "One real project music seed 42 to 1 reuse, authorization, publication and reopen; not prompt range, natural-language or desktop acceptance")
    report = dict(status="PREPARING", scope=scope,
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
                                LUMINA_PROJECT_MUSIC_REUSE=("production" if args.production else
                                                           "metadata_fixture" if args.prepare_metadata_fixture else
                                                           "domain_measurement" if domain_case is not None else "measurement"),
                                RUST_BACKTRACE="1", TMP=str(run), TEMP=str(run))
        if args.production:
            os.environ.pop("LUMINA_TASK_RESOURCE_OWNER", None)
            jobs.ENVIRONMENT.pop("LUMINA_TASK_RESOURCE_OWNER", None)
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
            parent_path, proof_kind = PARENT / "result.json", "normal_tool"
            if args.parent_source:
                parent_path = args.parent_source.resolve(strict=True)
                parent_report, database, evidence, proof_kind = exported_source_evidence(parent_path)
            else:
                parent_report, database, evidence = source_evidence()
            if domain_case is not None:
                evidence[str(args.case_file)] = sha(args.case_file)
                report["domainCase"] = domain_case
            report["originalEvidence"] = evidence
            backup, archive_bytes = prepare_backup(run, parent_report, database, domain_case, parent_path, proof_kind)
            unchanged(evidence)
            contexts = {tier: publication_context(tier) for tier in ("small", "standard")}
            context = contexts["small"] if domain_case is None else None
            case_sha = sha(run / "project-reuse-case.json")
            (run / "output-root/native").mkdir(parents=True)
            report.update(publicationContext=context, caseSha256=case_sha, parentAssetBytes=archive_bytes,
                          publicationBaselineBytes=tree_bytes(run / "output-root/publication"))
            if args.production:
                monitor = ProjectMonitor(run, api, stack)
                monitor.install()
                report["status"] = "RUNNING_NORMAL_TOOL"
                persist()
                call = jobs.run_process(api, [str(host), "--exact", PRODUCTION_TEST, "--ignored", "--nocapture"],
                                        run, "normal-tool", 1200, report, persist, [])
                require(call["processTreeExitConfirmed"] and not call.get("terminatedOwnedJob")
                        and call["exitCode"] == 0 and call["memoryQueryFailures"] == 0
                        and f"test {PRODUCTION_TEST} ... ok" in Path(call["stdout"]).read_text(encoding="utf-8").splitlines(),
                        "Normal project reuse did not pass the exact production-entry test")
                document = validate_production(run, contexts, case_sha, args.expected_profile_sha256,
                                               monitor, call["jobAfterCleanup"], host, monitors.ENGINE)
                require(sha(__file__) == report["runnerSha256"] and sha(run / "project-reuse-case.json") == case_sha,
                        "Normal entry controller or case changed")
                for relative, expected in report["measurementHelpers"].items():
                    require(sha(ROOT / relative) == expected, "Normal entry helper changed: " + relative)
                with closing(readonly_database(backup)) as connection:
                    require(parent_row(connection, parent_report, 32 << 20) == archive_bytes,
                            "Normal entry changed its source archive")
                unchanged(evidence)
                report.update(status="PASS_NORMAL_TOOL_ENTRY", publicationContext=document["publicationContext"],
                              resourceEvidenceSha256=document["resourceEvidenceSha256"],
                              originalEvidenceUnchanged=True, normalTool=document,
                              normalToolEvidenceSha256=sha(run / "production.json"),
                              toolResponseSha256=sha(run / "tool-response.json"),
                              engineExecuted=True, measurementOwnerExecuted=False,
                              processPaths=[str(row["path"]) for row in monitor.members.values()])
            elif args.prepare_metadata_fixture:
                call = jobs.run_process(api, [str(host), "--exact", METADATA_TEST, "--ignored", "--nocapture"],
                                        run, "metadata-fixture", 180, report, persist, [])
                require(call["processTreeExitConfirmed"] and not call.get("terminatedOwnedJob") and call["exitCode"] == 0
                        and call["memoryQueryFailures"] == 0
                        and f"test {METADATA_TEST} ... ok" in Path(call["stdout"]).read_text(encoding="utf-8").splitlines(),
                        "Metadata fixture host did not execute and pass the exact setup test")
                fixture_result = read(run / "fixture-result.json")
                require(fixture_result["status"] == "API_METADATA_FIXTURE_READY" and fixture_result["generatedNewAudio"] is False
                        and fixture_result["caseSha256"] == case_sha
                        and fixture_result["metadataPressure"] == domain_case["metadataPressure"]
                        and fixture_result["originalSourceAssetId"] == ASSET
                        and fixture_result["originalSourceContentSha256"] == CONTENT
                        and fixture_result["originalSourceOutputSha256"] == PARENT_WAV,
                        "Metadata setup differs from its case or original real audio")
                require(REQUIRED_HOST_SOURCES.issubset(validate_sources(fixture_result, "metadata fixture")),
                        "Metadata fixture omitted source fingerprints")
                require(sha(__file__) == report["runnerSha256"] and sha(run / "project-reuse-case.json") == case_sha,
                        "Fixture controller or case changed")
                for relative, expected in report["measurementHelpers"].items():
                    require(sha(ROOT / relative) == expected, "Fixture helper changed: " + relative)
                with closing(readonly_database(backup)) as connection:
                    require(parent_row(connection, parent_report) == archive_bytes, "Fixture changed its original parent")
                unchanged(evidence)
                report.update(status="API_METADATA_FIXTURE_READY", generatedNewAudio=False,
                              fixtureResultSha256=sha(run / "fixture-result.json"),
                              metadataPressure=domain_case["metadataPressure"],
                              fixtureParentAssetBytes=fixture_result["parentAssetBytes"], originalEvidenceUnchanged=True)
            else:
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
                if domain_case is not None:
                    tier = read(run / "publication.json")["publicationContext"]["payloadTier"]
                    require(tier in contexts, "Unexpected publication tier")
                    context = contexts[tier]
                    report["publicationContext"] = context
                fixture, native, pub, request_sha, wav = validate_reports(run, context, case_sha, archive_bytes, domain_case is not None)
                require(context == publication_context(context["payloadTier"]) and sha(run / "project-reuse-case.json") == case_sha,
                        "Context or immutable case changed during measurement")
                with closing(readonly_database(backup)) as connection:
                    require(parent_row(connection, parent_report, 32 << 20 if domain_case is not None else SMALL_SOURCE_ASSETS_LIMIT)
                            == archive_bytes, "Backup parent changed after reuse")
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
                              preparationMillis=pub["preparationMillis"], outputArchiveBytes=pub["outputArchiveBytes"], originalEvidenceUnchanged=True,
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
    require(report["status"] in ("MEASURED_REVIEW_REQUIRED", "API_METADATA_FIXTURE_READY", "PASS_NORMAL_TOOL_ENTRY"), "Execution did not complete cleanly")
    if domain_case is not None and not args.production:
        proof = run / ("fixture-result.json" if args.prepare_metadata_fixture else "result.json")
        document = read(run / ("fixture-result.json" if args.prepare_metadata_fixture else "publication.json"))
        try:
            export_source(run, document["exportSource"],
                          "api_metadata_fixture" if args.prepare_metadata_fixture else "project_measurement", proof)
        except Exception as error:
            report.update(status="FAILED_SOURCE_EXPORT", exportError=repr(error))
            persist()
            raise


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
    good_cases = [dict(schemaVersion=1, caseId="seed-max", patch={"seed": 0xffffffff}),
                  dict(schemaVersion=1, caseId="utf8-limit", patch={"prompt": "猫" * 341 + "a"}),
                  dict(schemaVersion=1, caseId="long-parent", patch={"prompt": "x" * 16384}),
                  dict(schemaVersion=1, caseId="metadata", patch={"seed": 1},
                       metadataPressure=dict(referenceCount=118, messageBytes=1024))]
    for case in good_cases:
        validate_domain_case(case)
    bad_cases = [dict(schemaVersion=1, caseId="bad", patch=patch) for patch in
                 ({"seed": True}, {"seed": -1}, {"seed": 0x100000000}, {"seed": 1, "prompt": "piano"},
                  {"prompt": "x" * 16385}, {"prompt": "a|b"}, {"prompt": "a\0"}, {"steps": 8})]
    bad_cases.extend([dict(schemaVersion=True, caseId="bad", patch={"seed": 1}),
                      dict(schemaVersion=1, caseId="bad/path", patch={"seed": 1}),
                      dict(schemaVersion=1, caseId="bad", patch={"seed": 1},
                           metadataPressure=dict(referenceCount=129, messageBytes=1024))])
    for case in bad_cases:
        try:
            validate_domain_case(case)
        except ValueError:
            continue
        raise AssertionError("Accepted invalid domain case")
    print(json.dumps(dict(status="PASS", checks=10 + len(good_cases) + len(bad_cases), scope="Pure case/parent rejection, serialization, path ancestry and publication source checks; no database backup, Job lock or model execution", publicationContext=context)))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--self-check", action="store_true")
    parser.add_argument("--case-file", type=Path)
    parser.add_argument("--parent-source", type=Path)
    parser.add_argument("--prepare-metadata-fixture", action="store_true")
    parser.add_argument("--production", action="store_true")
    parser.add_argument("--expected-profile-sha256")
    for key in ("publication-probe", "owner-probe", "run-dir"):
        parser.add_argument("--" + key, type=Path)
    for key in ("publication-probe-sha256", "owner-probe-sha256"):
        parser.add_argument("--" + key)
    args = parser.parse_args()
    if args.self_check:
        require(not any(value for name, value in vars(args).items() if name != "self_check"), "Self-check cannot execute a run")
        self_check()
    else:
        require(all(getattr(args, key) for key in ("publication_probe", "owner_probe", "run_dir",
                                                   "publication_probe_sha256", "owner_probe_sha256")),
                "All probe paths, SHA256 values and run directory are required")
        execute(args)


if __name__ == "__main__":
    main()
