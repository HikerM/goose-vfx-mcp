"""Measure the installed SD1.5 CPU owner and actual session image publication on Windows."""

import argparse
import contextlib
import ctypes
from ctypes import wintypes
import json
import os
from pathlib import Path
import sys
import time

sys.dont_write_bytecode = True

import psutil
import windows_job_measurement as jobs
from measure_pocket_tts import read_json, require, sha, tree_bytes, write_json

ROOT = Path(__file__).resolve().parents[5]
BASE = ROOT / "target/lifecycle-validation"
ENGINE = (
    ROOT
    / "target/runtime-engines/diffusion-cpp/c6dfa9def12fd0cb36ac7149ba30a75792bedbf0/build-cpu/bin/Release/sd-cli.exe"
)
ENGINE_HASH = "4f0090622e57c198c9fb6470f0981d5d5434830770c1aa0cf50d9ab8c8e2f043"
HOST_TEST = "media::task_output::tests::sd15_probe::measure_installed_sd15_publication"
LIMIT = 32 << 20


class TreeMonitor:
    def __init__(self, run, api, stack):
        self.api = api
        self.run = run
        self.stack = stack
        self.members = {}
        self.controller_peak = 0
        self.native_bytes = self.publication_bytes = 0
        self.read_memory = api.memory
        self.log = stack.enter_context((run / "tree.jsonl").open("x", encoding="utf-8"))
        kernel = api.kernel
        kernel.OpenProcess.argtypes = (wintypes.DWORD, wintypes.BOOL, wintypes.DWORD)
        kernel.OpenProcess.restype = wintypes.HANDLE
        kernel.GetCurrentProcess.restype = wintypes.HANDLE
        kernel.GetProcessTimes.argtypes = [wintypes.HANDLE] + [
            ctypes.POINTER(wintypes.FILETIME)
        ] * 4
        kernel.GetProcessTimes.restype = wintypes.BOOL
        kernel.GetExitCodeProcess.argtypes = [
            wintypes.HANDLE,
            ctypes.POINTER(wintypes.DWORD),
        ]
        kernel.GetExitCodeProcess.restype = wintypes.BOOL
        self.controller = type("Proc", (), {"_handle": kernel.GetCurrentProcess()})()

    def install(self):
        monitor = self

        class MeasuredJob(jobs.OwnedJob):
            def assign(self, process):
                super().assign(process)
                monitor.job = self

            def accounting(self):
                value = super().accounting()
                limits = jobs.JobLimits()
                monitor.api.check(
                    monitor.api.kernel.QueryInformationJobObject(
                        self.handle,
                        9,
                        ctypes.byref(limits),
                        ctypes.sizeof(limits),
                        None,
                    )
                )
                value["peakJobCommitBytes"] = int(limits.PeakJobMemoryUsed)
                return value

        self.stack.callback(setattr, jobs, "OwnedJob", jobs.OwnedJob)
        self.stack.callback(setattr, self.api, "memory", self.read_memory)
        jobs.OwnedJob = MeasuredJob
        self.api.memory = self.sample

    def sample(self, process):
        own = self.read_memory(self.controller)
        self.controller_peak = max(
            self.controller_peak, own["PeakWorkingSetSize"], own["PeakPagefileUsage"]
        )
        info = self.job.accounting()
        samples = {}
        for pid in info["ProcessIds"]:
            if pid not in self.members:
                try:
                    path = Path(psutil.Process(pid).exe()).resolve(strict=True)
                except psutil.NoSuchProcess:
                    continue
                handle = self.api.kernel.OpenProcess(0x0410, False, pid)
                if not handle:
                    continue
                self.stack.callback(self.api.kernel.CloseHandle, handle)
                self.members[pid] = dict(
                    path=path,
                    proc=type("Proc", (), {"_handle": handle})(),
                    ws=0,
                    commit=0,
                )
            row = self.members[pid]
            sample = self.read_memory(row["proc"])
            row["ws"] = max(row["ws"], sample["PeakWorkingSetSize"])
            row["commit"] = max(row["commit"], sample["PeakPagefileUsage"])
            samples[pid] = sample
        self.native_bytes = max(
            self.native_bytes, tree_bytes(self.run / "output-root/native")
        )
        self.publication_bytes = max(
            self.publication_bytes, tree_bytes(self.run / "output-root/publication")
        )
        self.log.write(
            json.dumps(dict(time=time.monotonic(), job=info, members=samples)) + "\n"
        )
        self.log.flush()
        return self.read_memory(process)

    def finish(self, end, host, owner):
        require(
            end["TotalProcesses"] == len(self.members),
            "Missing process from monitored job",
        )
        console = (Path(os.environ["SystemRoot"]) / "System32/conhost.exe").resolve(
            strict=True
        )
        for row in self.members.values():
            require(
                row["path"] in {host, owner, ENGINE, console},
                "Unexpected child in measured job",
            )
            times = [wintypes.FILETIME() for _ in range(4)]
            handle = row["proc"]._handle
            self.api.check(
                self.api.kernel.GetProcessTimes(
                    handle, *(ctypes.byref(t) for t in times)
                )
            )
            start, finish = [
                (t.dwHighDateTime << 32) | t.dwLowDateTime for t in times[:2]
            ]
            code = wintypes.DWORD()
            self.api.check(
                self.api.kernel.GetExitCodeProcess(handle, ctypes.byref(code))
            )
            require(
                finish >= start > 0 and (row["path"] == console or code.value == 0),
                "Missing exited process",
            )
            require(row["ws"] and row["commit"], "Missing process memory")
            row.update(start=start, finish=finish)
        for path in (host, owner, ENGINE):
            require(
                sum(row["path"] == path for row in self.members.values()) == 1,
                f"Invalid process count for {path}",
            )

        def entry(path):
            return next(
                (pid, row) for pid, row in self.members.items() if row["path"] == path
            )

        host, native_owner, engine = entry(host), entry(owner), entry(ENGINE)
        require(
            engine[1]["finish"] <= native_owner[1]["finish"] <= host[1]["finish"],
            "Invalid owner/publication lifetime",
        )
        return host, engine


def measure(args):
    cases = read_json(Path(__file__).with_name("sd15-cases.json"))
    require(1 <= args.case <= len(cases), "Unknown SD image case")
    case = cases[args.case - 1]
    run = args.run_dir.resolve()
    deps = (ROOT / "target/debug/deps").resolve(strict=True)
    host, owner = (
        p.resolve(strict=True) for p in (args.publication_probe, args.owner_probe)
    )
    require(
        run.parent == BASE and host.parent == deps == owner.parent,
        "Unexpected run or executable directory",
    )
    run.mkdir()
    (run / "output-root/native").mkdir(parents=True)
    # Independent Windows jobs attribute only their own process trees. Serialize
    # SD trials against each other while another adapter measures its own store.
    jobs.RUN_ROOT = BASE / "m4-managed-generation-20261113-v2-r1"
    jobs.ENVIRONMENT.update(
        LUMINA_TASK_RESOURCE_OWNER=str(owner),
        LUMINA_TASK_RESOURCE_RUN_DIR=str(run),
        LUMINA_DIFFUSION_CPU_CLI=str(ENGINE),
        LUMINA_PATH_ROOT=str(run / "application"),
        LUMINA_TASK_RESOURCE_VERIFY="measurement",
        LUMINA_SD15_ACCEPTANCE_CASE=str(args.case),
    )
    report = {
        "status": "PREPARING",
        "processes": [],
        "runnerSha256": sha(Path(__file__)),
        "jobControllerSha256": sha(Path(jobs.__file__)),
        "ioUtilitiesSha256": sha(Path(sys.modules["measure_pocket_tts"].__file__)),
    }

    def persist():
        write_json(run / "result.json", report)

    api = jobs.WindowsApi()
    try:
        with contextlib.ExitStack() as stack:
            stack.enter_context(jobs.exclusive_run())
            for path, expected in (
                (ENGINE, ENGINE_HASH),
                (host, args.publication_probe_sha256),
                (owner, args.owner_probe_sha256),
            ):
                stack.enter_context(api.read_guard(path))
                require(
                    sha(path) == expected.lower(), f"Binary identity mismatch {path}"
                )
            report["helpers"] = {str(path): sha(path) for path in (ENGINE, host, owner)}
            monitor = TreeMonitor(run, api, stack)
            monitor.install()
            report["status"] = "RUNNING"
            persist()
            call = jobs.run_process(
                api,
                [str(host), "--exact", HOST_TEST, "--ignored", "--nocapture"],
                run,
                "lifecycle",
                1200,
                report,
                persist,
                [(run / "output-root/native/output.png", LIMIT)],
            )
            require(
                call["processTreeExitConfirmed"]
                and not call.get("terminatedOwnedJob")
                and call["exitCode"] == 0,
                "Native or publication job failed",
            )
            require(
                f"test {HOST_TEST} ... ok"
                in Path(call["stdout"]).read_text(encoding="utf-8").splitlines(),
                "Host test did not run",
            )
            end = call["jobAfterCleanup"]
            main, engine = monitor.finish(end, host, owner)
            fixture, driver, native, publication = [
                read_json(run / name)
                for name in (
                    "fixture.json",
                    "driver.json",
                    "owner.json",
                    "publication.json",
                )
            ]
            require(native.get("sources"), "Missing native owner implementation digests")
            for name, sources in (
                ("hostCheckoutDifferences", driver["sources"]),
                ("ownerCheckoutDifferences", native["sources"]),
            ):
                report[name] = {
                    relative: {"built": expected, "checkout": current}
                    for relative, expected in sources.items()
                    if (current := sha(ROOT / relative)) != expected
                }
            require(
                fixture["snapshot"] == read_json(run / "snapshot.json")
                and fixture["key"]["kind"] == "image"
                and not fixture["key"]["requiresGpu"]
                and all(
                    fixture["snapshot"]["parameters"]["parameters"][k] == v
                    for k, v in case.items()
                ),
                "Wrong installed image workload",
            )
            require(
                fixture["snapshotDigest"]
                == native["snapshotDigest"]
                == publication["snapshotDigest"],
                "Snapshot binding mismatch",
            )
            require(
                publication["reopenedVerified"]
                and publication["implementation"]
                == driver["implementation"]
                == "media::task_output::import_readers"
                and native["outputSha256"]
                == publication["inputSha256"]
                == publication["publishedSha256"]
                == sha(run / "output-root/native/output.png")
                == sha(Path(publication["publishedPath"])),
                "Published content changed",
            )
            require(
                publication["result"]["mime"] == "image/png"
                and publication["result"]["spec"]["width"]
                == publication["result"]["spec"]["height"]
                == 512,
                "Published geometry changed",
            )
            sample_hash = sha(run / "tree.jsonl")
            native_bytes = max(monitor.native_bytes, native["outputBytes"])
            published_bytes = max(
                monitor.publication_bytes,
                *(
                    point["directoryLogicalBytes"]
                    for point in publication["checkpoints"]
                ),
            )

            def proc(entry, disk):
                pid, row = entry
                return dict(
                    pid=pid,
                    peakWorkingSetBytes=row["ws"],
                    peakCommitBytes=row["commit"],
                    elapsedMillis=max(
                        1, (row["finish"] - row["start"] + 9999) // 10000
                    ),
                    exitCode=0,
                    processTreeExited=True,
                    memorySamplesSha256=sample_hash,
                    observedOutputPeakBytes=disk,
                )

            report.update(
                job=end,
                members={
                    pid: dict(
                        path=str(row["path"]),
                        ws=row["ws"],
                        commit=row["commit"],
                        start=row["start"],
                        finish=row["finish"],
                    )
                    for pid, row in monitor.members.items()
                },
                artifacts={
                    name: sha(run / name)
                    for name in (
                        "fixture.json",
                        "driver.json",
                        "publication.json",
                        "owner.json",
                        "tree.jsonl",
                    )
                },
            )
            write_json(run / "execution-evidence.json", report)
            record = dict(
                schemaVersion=2,
                rulesVersion="native-task-resources-v1",
                key=fixture["key"],
                provenance=dict(
                    runnerSha256=report["runnerSha256"],
                    fixtureSha256=sha(run / "fixture.json"),
                    executionEvidenceSha256=sha(run / "execution-evidence.json"),
                ),
                snapshotDigest=fixture["snapshotDigest"],
                hardware=fixture["hardware"],
                controllerPeakCommitBytes=monitor.controller_peak,
                lifecycleTree=dict(
                    processIds=sorted(monitor.members),
                    peakJobCommitBytes=end["peakJobCommitBytes"],
                    summedPeakWorkingSetBytes=sum(
                        m["ws"] for m in monitor.members.values()
                    ),
                    samplesSha256=sample_hash,
                ),
                native=proc(engine, native_bytes),
                publication=dict(
                    kind="media",
                    process=proc(main, published_bytes),
                    implementation=publication["implementation"],
                    implementationSourcesSha256=publication[
                        "implementationSourcesSha256"
                    ],
                    helperBinarySha256=args.publication_probe_sha256.lower(),
                    inputSha256=publication["inputSha256"],
                    publishedSha256=publication["publishedSha256"],
                    publishedBytes=publication["publishedBytes"],
                    peakDirectoryBytes=published_bytes,
                    reopenedVerified=True,
                ),
                gpu={"status": "cpu_only"},
                nativeExitedBeforePublication=True,
                reviewedBudget=None,
            )
            write_json(run / "measurement.json", record)
            report.update(
                status=(
                    "MEASURED_PREVIOUS_BUILD"
                    if report["hostCheckoutDifferences"] or report["ownerCheckoutDifferences"]
                    else "MEASURED_REVIEW_REQUIRED"
                ),
                measurementSha256=sha(run / "measurement.json"),
            )
    except BaseException as error:
        report.update(status="FAILED", error=str(error))
        raise
    finally:
        persist()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--case", type=int, default=1)
    for arg in ("publication-probe", "owner-probe", "run-dir"):
        parser.add_argument("--" + arg, type=Path, required=True)
    for arg in ("publication-probe-sha256", "owner-probe-sha256"):
        parser.add_argument("--" + arg, required=True)
    measure(parser.parse_args())
