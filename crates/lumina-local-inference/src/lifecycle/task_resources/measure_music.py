"""Measure a real Stable Audio run, session publication and reopen in one Windows Job."""
import argparse
from contextlib import ExitStack
from pathlib import Path
import sys
sys.dont_write_bytecode = True

import measure_sd15 as monitor_tools
import windows_job_measurement as jobs
from measure_pocket_tts import read_json, require, sha, write_json

ROOT = Path(__file__).resolve().parents[5]
BASE = ROOT / "target/lifecycle-validation"
HELPER_SHA = "5e53e8441457c990d11955873e513c29030e04c3d19229c62eeb03b0e118ad8b"
ENGINE = ROOT / "target/runtime-engines/audio-cpp/c0cd77fa80285f8113883e0de06ffc781cf502f6/validated-binaries" / HELPER_SHA / "audiocpp_cli.exe"
WAV_SHA = "afa7d6699180ed7c1bd7fdd4524af6b4faf15b9e077f25d4b70dc5787693f2e5"
HOST_TEST = "media::task_output::tests::music_probe::measure_installed_music_publication"


class MusicMonitor(monitor_tools.TreeMonitor):
    def __init__(self, run, api, stack):
        super().__init__(run, api, stack)
        self.output_bytes = 0

    def sample(self, process):
        root = self.run / "output-root/native"
        for directory in root.glob(".lumina-native-task-*"):
            try:
                # GuardedTaskDirectory accepts only regular direct-child output files.
                sizes = sum(p.stat().st_size for p in directory.iterdir() if p.is_file())
            except FileNotFoundError:
                continue
            self.output_bytes = max(self.output_bytes, sizes)
        return super().sample(process)


def measure(args):
    run = args.run_dir.resolve()
    deps = (ROOT / "target/debug/deps").resolve(strict=True)
    host, owner = (p.resolve(strict=True) for p in (args.publication_probe, args.owner_probe))
    require(run.parent == BASE and host.parent == deps == owner.parent, "Unexpected measurement directory")
    run.mkdir()
    (run / "output-root/native").mkdir(parents=True)
    jobs.RUN_ROOT = BASE / "m4-managed-audio-install-20261113-r1"
    jobs.ENVIRONMENT.update(LUMINA_TASK_RESOURCE_OWNER=str(owner), LUMINA_TASK_RESOURCE_RUN_DIR=str(run),
        LUMINA_AUDIO_MUSIC_CPU_CLI=str(ENGINE), LUMINA_PATH_ROOT=str(run / "application"), LUMINA_TASK_RESOURCE_VERIFY="measurement",
        TMP=str(run), TEMP=str(run))
    # This standalone controller monitors one fixed native engine.
    monitor_tools.ENGINE = ENGINE
    report = {"status":"PREPARING", "processes":[], "runnerSha256":sha(Path(__file__)),
        "jobControllerSha256":sha(Path(jobs.__file__)), "treeMonitorSha256":sha(Path(monitor_tools.__file__)),
        "ioUtilitiesSha256":sha(Path(sys.modules["measure_pocket_tts"].__file__))}
    def persist():
        write_json(run / "result.json", report)
    api = jobs.WindowsApi()
    try:
        with ExitStack() as stack:
            stack.enter_context(jobs.exclusive_run())
            for path, expected in [(ENGINE,HELPER_SHA),(host,args.publication_probe_sha256),(owner,args.owner_probe_sha256)]:
                stack.enter_context(api.read_guard(path))
                require(sha(path) == expected.lower(), f"Executable identity changed: {path}")
            monitor = MusicMonitor(run, api, stack)
            monitor.install()
            report["status"] = "RUNNING"
            persist()
            call = jobs.run_process(api, [str(host),"--exact",HOST_TEST,"--ignored","--nocapture"],run,"lifecycle",1200,
                report,persist,[(run/"output-root/native/output.wav",32<<20)])
            require(call["processTreeExitConfirmed"] and not call.get("terminatedOwnedJob") and call["exitCode"] == 0
                    and f"test {HOST_TEST} ... ok" in Path(call["stdout"]).read_text(encoding="utf-8").splitlines(), "Music host failed")
            end = call["jobAfterCleanup"]
            main, engine = monitor.finish(end, host, owner)
            fixture, driver, native, pub = [read_json(run / name) for name in ("fixture.json","driver.json","owner.json","publication.json")]
            for label, doc in (("owner",native),("host",driver)):
                require(doc.get("sources"), f"No {label} source fingerprints")
                differences = {p:expected for p,expected in doc["sources"].items() if sha(ROOT/p)!=expected}
                require(not differences, f"{label} build differs from checkout: {differences}")
            require(fixture["snapshot"] == read_json(run/"snapshot.json") and fixture["key"]["kind"] == "music"
                    and not fixture["key"]["requiresGpu"], "Expected bound CPU music snapshot")
            require(pub["reopenedVerified"] and pub["implementation"]==driver["implementation"]=="media::task_output::import_readers"
                    and pub["snapshotDigest"]==native["snapshotDigest"]==fixture["snapshotDigest"], "Invalid publication identity")
            require(native["outputSha256"] == WAV_SHA == pub["inputSha256"] == pub["publishedSha256"]
                    == sha(run/"output-root/native/output.wav") == sha(Path(pub["publishedPath"]))
                    and pub["publishedBytes"]==705644 and pub["result"]["mime"]=="audio/wav","Published audio differs from independent replay")
            samples = sha(run/"tree.jsonl")
            native_peak = max(native["outputBytes"], monitor.output_bytes)
            require(monitor.native_bytes > 40_176_400, "Missing cold engine scratch footprint")
            pub_peak = max(monitor.publication_bytes,*(p["directoryLogicalBytes"] for p in pub["checkpoints"]))
            def metrics(entry, disk):
                pid,row=entry
                return dict(pid=pid,peakWorkingSetBytes=row["ws"],peakCommitBytes=row["commit"],
                    elapsedMillis=max(1,(row["finish"]-row["start"]+9999)//10000),exitCode=0,
                    processTreeExited=True,memorySamplesSha256=samples,observedOutputPeakBytes=disk)
            report.update(job=end, nativeDirectoryHighWaterBytes=monitor.native_bytes, members={pid:{k:str(v) if k=="path" else v for k,v in row.items() if k!="proc"}
                for pid,row in monitor.members.items()}, artifacts={f:sha(run/f) for f in ["fixture.json","driver.json","owner.json","publication.json","tree.jsonl"]})
            write_json(run/"execution-evidence.json",report)
            record = dict(schemaVersion=2,rulesVersion="native-task-resources-v1",key=fixture["key"],
                provenance=dict(runnerSha256=report["runnerSha256"],fixtureSha256=sha(run/"fixture.json"),
                    executionEvidenceSha256=sha(run/"execution-evidence.json")),snapshotDigest=fixture["snapshotDigest"],
                hardware=fixture["hardware"],controllerPeakCommitBytes=monitor.controller_peak,
                lifecycleTree=dict(processIds=sorted(monitor.members),peakJobCommitBytes=end["peakJobCommitBytes"],
                    summedPeakWorkingSetBytes=sum(r["ws"] for r in monitor.members.values()),samplesSha256=samples),
                native=metrics(engine,native_peak),publication=dict(kind="media",process=metrics(main,pub_peak),
                    implementation=pub["implementation"],implementationSourcesSha256=pub["implementationSourcesSha256"],
                    helperBinarySha256=sha(host),inputSha256=WAV_SHA,publishedSha256=WAV_SHA,publishedBytes=pub["publishedBytes"],
                    peakDirectoryBytes=pub_peak,reopenedVerified=True),gpu={"status":"cpu_only"},
                nativeExitedBeforePublication=True,reviewedBudget=None)
            write_json(run/"measurement.json",record)
            report.update(status="MEASURED_REVIEW_REQUIRED",measurementSha256=sha(run/"measurement.json"))
    except BaseException as error:
        report.update(status="FAILED",error=str(error))
        raise
    finally:
        persist()


if __name__ == "__main__":
    parser=argparse.ArgumentParser(description=__doc__)
    for key in ("publication-probe","owner-probe","run-dir"):
        parser.add_argument("--"+key,type=Path,required=True)
    for key in ("publication-probe-sha256","owner-probe-sha256"):
        parser.add_argument("--"+key,required=True)
    measure(parser.parse_args())
