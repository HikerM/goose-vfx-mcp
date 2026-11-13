"""Measure one fixed PocketTTS invocation and actual isolated MediaResultStore publication.

Requires two already-built ignored Rust test helpers. Never builds, downloads,
installs, changes a runtime allowlist, or marks capability READY. A completed
measurement still needs review and a source-pinned record before admission.
"""

import argparse
import contextlib
import ctypes
import hashlib
import json
import os
from pathlib import Path
import sys
import time
import wave

sys.dont_write_bytecode = True


ROOT = Path(__file__).resolve().parents[5]
VALIDATION = ROOT / "target/lifecycle-validation"
REVISION = "c0cd77fa80285f8113883e0de06ffc781cf502f6"
BINARY_SHA = "b319916421e54024ba4229462e1a577f368627176fc538d3be6d4a8a5398394f"
ENGINE = ROOT / "target/runtime-engines/audio-cpp" / REVISION
BINARY = ENGINE / "validated-binaries" / BINARY_SHA / "audiocpp_cli.exe"
MAX_OUTPUT = 32 * 1024 * 1024


def require(value, message):
    if not value:
        raise RuntimeError(message)


def sha(path):
    result = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            result.update(chunk)
    return result.hexdigest()


def write_json(path, value):
    temporary = path.with_suffix(path.suffix + ".tmp")
    temporary.write_text(
        json.dumps(value, indent=2, ensure_ascii=False, allow_nan=False) + "\n",
        encoding="utf-8",
    )
    os.replace(temporary, path)


def read_json(path):
    return json.loads(path.read_text(encoding="utf-8"))


def tree_bytes(path):
    if not path.exists():
        return 0
    total = 0
    for parent, directories, files in os.walk(path, followlinks=False):
        for name in directories + files:
            item = Path(parent) / name
            try:
                metadata = item.lstat()
            except FileNotFoundError:
                continue  # SQLite may remove a WAL between sampler snapshots.
            require(
                not item.is_symlink() and not (metadata.st_file_attributes & 0x400),
                "Probe output contains a reparse point",
            )
            if name in files:
                total += metadata.st_size
    return total


def active_engines(api, module):
    snapshot = api.kernel.CreateToolhelp32Snapshot(2, 0)
    require(
        snapshot != ctypes.c_void_p(-1).value,
        "Cannot enumerate active native processes",
    )
    names = {"sd-cli.exe", "audiocpp_cli.exe", "llama-server.exe", "test_whisper.exe"}
    found = []
    try:
        entry = module.ProcessEntry()
        entry.dwSize = ctypes.sizeof(entry)
        available = api.kernel.Process32FirstW(snapshot, ctypes.byref(entry))
        while available:
            if entry.szExeFile.lower() in names:
                found.append(
                    {"pid": int(entry.th32ProcessID), "executable": entry.szExeFile}
                )
            available = api.kernel.Process32NextW(snapshot, ctypes.byref(entry))
        require(ctypes.get_last_error() == 18, "Incomplete process inventory")
    finally:
        api.kernel.CloseHandle(snapshot)
    return found


def measure(args):
    from ctypes import wintypes
    import subprocess

    texts = [
        "The red apple is on the wooden table.",
        "Please read the short note aloud.",
        "The morning train leaves at nine. We can walk to the station after breakfast.",
    ]
    seed_sweep = [0, 2**31, 2**32 - 1]
    bounded_cases = [
        ("I.", 0),
        ("Hello there.", 2**31),
        ("Fresh blue rain falls now.", 2**32 - 1),
        ("QQKWKWKKWQJXWZWJXKWWJXQZQZWZWK.", 17),
    ]
    if args.case <= 3:
        text, seed = texts[args.case - 1], 42
    elif args.case <= 6:
        text, seed = texts[1], seed_sweep[args.case - 4]
    else:
        text, seed = bounded_cases[args.case - 7]
    max_frames = 0 if args.case <= 3 else 32
    root = VALIDATION.resolve(strict=True)
    require(
        args.run_dir.parent.resolve(strict=True) == root,
        "Expected a direct child of lifecycle-validation",
    )
    args.run_dir.mkdir()
    run = args.run_dir.resolve(strict=True)
    (run / "output-root/native").mkdir(parents=True)
    host_test = "media::task_output::tests::resource_probe::measure_installed_pocket_publication"
    owner_test = (
        "lifecycle::task_resources::tests::pocket_probe::measure_installed_pocket_owner"
    )
    deps = (ROOT / "target/debug/deps").resolve(strict=True)
    host, owner = (
        args.publication_probe.resolve(strict=True),
        args.owner_probe.resolve(strict=True),
    )
    require(
        host.parent == deps == owner.parent, "Expected test binaries from this checkout"
    )
    helper_hashes = {
        str(p): digest.lower()
        for p, digest in (
            (host, args.publication_probe_sha256),
            (owner, args.owner_probe_sha256),
            (BINARY, BINARY_SHA),
        )
    }
    import windows_job_measurement as module

    module.RUN_ROOT = root
    module.ENVIRONMENT.update(
        LUMINA_TASK_RESOURCE_VERSION=args.version_id,
        LUMINA_TASK_RESOURCE_RUN_DIR=str(run),
        LUMINA_TASK_RESOURCE_OWNER=str(owner),
        LUMINA_TASK_RESOURCE_TEXT=text,
        LUMINA_TASK_RESOURCE_SEED=str(seed),
        LUMINA_TASK_RESOURCE_MAX_FRAMES=str(max_frames),
        LUMINA_AUDIO_CPU_CLI=str(BINARY),
        LUMINA_PATH_ROOT=str(run / "application"),
    )
    api = module.WindowsApi()
    api.kernel.GetCurrentProcess.restype = wintypes.HANDLE
    api.kernel.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
    api.kernel.OpenProcess.restype = wintypes.HANDLE
    api.kernel.QueryFullProcessImageNameW.argtypes = [
        wintypes.HANDLE,
        wintypes.DWORD,
        wintypes.LPWSTR,
        ctypes.POINTER(wintypes.DWORD),
    ]
    api.kernel.QueryFullProcessImageNameW.restype = wintypes.BOOL
    api.kernel.GetProcessTimes.argtypes = [wintypes.HANDLE] + [
        ctypes.POINTER(wintypes.FILETIME)
    ] * 4
    api.kernel.GetProcessTimes.restype = wintypes.BOOL
    controller = type(
        "ControllerHandle", (), {"_handle": api.kernel.GetCurrentProcess()}
    )()
    report = {
        "status": "PREPARING",
        "case": args.case,
        "text": text,
        "seed": seed,
        "maxFrames": max_frames,
        "processes": [],
        "scriptSha256": sha(Path(__file__)),
        "helpers": helper_hashes,
        "controllerSha256": sha(Path(module.__file__)),
        "scope": "One registered CPU model, one exact request and reference voice on the observed machine",
    }

    def persist():
        write_json(run / "result.json", report)

    persist()
    try:
        with contextlib.ExitStack() as stack:
            stack.enter_context(module.exclusive_run())
            for p, digest in helper_hashes.items():
                path = Path(p)
                stack.enter_context(api.read_guard(path))
                require(sha(path) == digest, f"Executable identity changed: {p}")
            env = {**os.environ, **module.ENVIRONMENT}
            for probe, test in ((host, host_test), (owner, owner_test)):
                listed = subprocess.run(
                    [str(probe), "--list", test],
                    env=env,
                    capture_output=True,
                    text=True,
                    check=True,
                    timeout=180,
                )
                require(
                    f"{test}: test" in listed.stdout.splitlines(),
                    f"Current binary lacks exact probe {test}",
                )
            report["otherInferenceBeforeLaunch"] = active_engines(api, module)
            original_job, original_memory = module.OwnedJob, api.memory
            members, peaks, lifetime_job = (
                {},
                {"controller": 0, "nativeDisk": 0, "publicationDisk": 0},
                {},
            )
            sample_file = stack.enter_context(
                (run / "tree.memory.jsonl").open("x", encoding="utf-8")
            )

            class MeasuredJob(original_job):
                def assign(self, process):
                    super().assign(process)
                    lifetime_job["job"] = self

                def accounting(self):
                    observed = super().accounting()
                    limits = module.JobLimits()
                    api.check(
                        api.kernel.QueryInformationJobObject(
                            self.handle,
                            9,
                            ctypes.byref(limits),
                            ctypes.sizeof(limits),
                            None,
                        )
                    )
                    observed["peakJobCommitBytes"] = int(limits.PeakJobMemoryUsed)
                    return observed

            module.OwnedJob = MeasuredJob

            def sample(process):
                now = time.monotonic()
                job = lifetime_job["job"].accounting()
                own = original_memory(controller)
                peaks["controller"] = max(
                    peaks["controller"],
                    own["PeakPagefileUsage"],
                    own["PeakWorkingSetSize"],
                )
                peaks["nativeDisk"] = max(
                    peaks["nativeDisk"], tree_bytes(run / "output-root/native")
                )
                peaks["publicationDisk"] = max(
                    peaks["publicationDisk"],
                    tree_bytes(run / "output-root/publication"),
                )
                samples = {}
                for pid in job["ProcessIds"]:
                    if pid not in members:
                        handle = api.kernel.OpenProcess(0x0410, False, pid)
                        if not handle:
                            continue
                        stack.callback(api.kernel.CloseHandle, handle)
                        name, length = (
                            ctypes.create_unicode_buffer(32768),
                            wintypes.DWORD(32768),
                        )
                        api.check(
                            api.kernel.QueryFullProcessImageNameW(
                                handle, 0, name, ctypes.byref(length)
                            )
                        )
                        members[pid] = {
                            "handle": type("Proc", (), {"_handle": handle})(),
                            "path": Path(name.value).resolve(),
                            "peakWs": 0,
                            "peakCommit": 0,
                            "first": now,
                            "last": now,
                        }
                    row = members[pid]
                    observed = original_memory(row["handle"])
                    row["last"] = now
                    row["peakWs"] = max(row["peakWs"], observed["PeakWorkingSetSize"])
                    row["peakCommit"] = max(
                        row["peakCommit"], observed["PeakPagefileUsage"]
                    )
                    samples[pid] = observed
                sample_file.write(
                    json.dumps({"time": now, "job": job, "members": samples}) + "\n"
                )
                sample_file.flush()
                return original_memory(process)

            api.memory = sample
            report["status"] = "MEASURING"
            entry = module.run_process(
                api,
                [str(host), "--exact", host_test, "--ignored", "--nocapture"],
                run,
                "lifecycle",
                360,
                report,
                persist,
                [(run / "output-root/native/output.wav", MAX_OUTPUT)],
            )
            require(
                entry["processTreeExitConfirmed"]
                and not entry.get("terminatedOwnedJob"),
                "Job cleanup was incomplete",
            )
            require(
                f"test {host_test} ... ok"
                in Path(entry["stdout"]).read_text(encoding="utf-8"),
                "Host test did not execute",
            )
            end = entry["jobAfterCleanup"]
            report["otherInferenceAfterExit"] = active_engines(api, module)
            require(
                end["TotalProcesses"] == len(members) and len(members) >= 3,
                "One or more members of the measured job were not observed",
            )

            def member(path):
                found = [
                    (pid, row)
                    for pid, row in members.items()
                    if row["path"] == path.resolve(strict=True)
                ]
                require(len(found) == 1, f"Missing unique measured process {path}")
                return found[0]

            native_pid, native = member(BINARY)
            main_pid, main = member(host)
            owner_pid, _ = member(owner)
            for row in members.values():
                times = [wintypes.FILETIME() for _ in range(4)]
                api.check(
                    api.kernel.GetProcessTimes(
                        row["handle"]._handle, *(ctypes.byref(t) for t in times)
                    )
                )
                start, finish = (
                    (t.dwHighDateTime << 32) | t.dwLowDateTime for t in times[:2]
                )
                require(finish >= start > 0, "Missing lifetime of an exited job member")
                row.update(startTicks=start, finishTicks=finish)
            require(
                native["finishTicks"]
                <= members[owner_pid]["finishTicks"]
                <= main["finishTicks"],
                "Native owner was still running when the publisher exited",
            )
            console = (Path(os.environ["SystemRoot"]) / "System32/conhost.exe").resolve(
                strict=True
            )
            for pid, row in members.items():
                if pid not in {native_pid, main_pid, owner_pid}:
                    require(row["path"] == console, "Unexpected child in measured job")
            require(
                all(
                    row["peakCommit"] > 0 and row["peakWs"] > 0
                    for row in members.values()
                ),
                "Missing process peak",
            )
            fixture = read_json(run / "fixture.json")
            driver = read_json(run / "driver.json")
            owner_record = read_json(run / "owner.json")
            pub = read_json(run / "publication.json")
            require(
                fixture["snapshot"]["parameters"]["text"] == text
                and fixture["snapshot"]["parameters"]["seed"] == seed
                and fixture["snapshot"]["parameters"]["maxFrames"] == max_frames
                and fixture["snapshot"]["parameters"]["framesAfterEos"]
                == (max_frames or -1)
                and fixture["snapshotDigest"]
                == pub["snapshotDigest"]
                == owner_record["snapshotDigest"],
                "Snapshot binding mismatch",
            )
            require(
                pub["reopenedVerified"]
                and pub["inputSha256"]
                == pub["publishedSha256"]
                == owner_record["outputSha256"]
                == sha(run / "output-root/native/output.wav"),
                "Publication content identity mismatch",
            )
            if max_frames:
                with wave.open(str(run / "output-root/native/output.wav"), "rb") as wav:
                    require(
                        (
                            wav.getframerate(),
                            wav.getnchannels(),
                            wav.getsampwidth(),
                            wav.getnframes(),
                        )
                        == (24_000, 1, 2, 1920 * max_frames)
                        and any(wav.readframes(wav.getnframes())),
                        "Measured fixed-frame seed workload changed PCM layout or produced silence",
                    )
            require(
                driver["implementation"]
                == pub["implementation"]
                == "media::task_output::import_readers",
                "Unexpected publisher",
            )
            report["checkoutDifferences"] = {
                relative: {"built": digest, "checkout": current}
                for relative, digest in driver["sources"].items()
                if (current := sha(ROOT / relative)) != digest
            }
            if args.case >= 7:
                owner_sources = owner_record["implementationSources"]
                require(
                    "src/lifecycle/task_resources.rs" in owner_sources
                    and "src/lifecycle/native_task_runtime.rs" in owner_sources,
                    "Bounded workload lacks compiled native owner sources",
                )
                report["ownerCheckoutDifferences"] = {
                    relative: {"built": digest, "checkout": current}
                    for relative, digest in owner_sources.items()
                    if (current := sha(ROOT / "crates/lumina-local-inference" / relative))
                    != digest
                }
            require(
                read_json(run / "snapshot.json") == fixture["snapshot"]
                and not fixture["key"]["requiresGpu"],
                "Invalid CPU workload fixture",
            )
            native_bytes = max(peaks["nativeDisk"], owner_record["outputBytes"])
            publication_bytes = max(
                peaks["publicationDisk"],
                *(c["directoryLogicalBytes"] for c in pub["checkpoints"]),
            )
            sample_hash = sha(run / "tree.memory.jsonl")
            report.update(
                job=end,
                memberPaths={pid: str(row["path"]) for pid, row in members.items()},
                memberLifetimeTicks={
                    pid: [row["startTicks"], row["finishTicks"]]
                    for pid, row in members.items()
                },
                sampleSha256=sample_hash,
                artifacts={
                    name: sha(run / name)
                    for name in (
                        "fixture.json",
                        "publication.json",
                        "owner.json",
                        "driver.json",
                    )
                },
            )
            write_json(run / "execution-evidence.json", report)

            def proc(pid, row, disk):
                return {
                    "pid": pid,
                    "peakWorkingSetBytes": row["peakWs"],
                    "peakCommitBytes": row["peakCommit"],
                    "elapsedMillis": max(
                        1, (row["finishTicks"] - row["startTicks"] + 9999) // 10000
                    ),
                    "exitCode": 0,
                    "processTreeExited": True,
                    "memorySamplesSha256": sample_hash,
                    "observedOutputPeakBytes": disk,
                }

            record = {
                "schemaVersion": 2,
                "rulesVersion": "native-task-resources-v1",
                "key": fixture["key"],
                "provenance": {
                    "runnerSha256": report["scriptSha256"],
                    "fixtureSha256": sha(run / "fixture.json"),
                    "executionEvidenceSha256": sha(run / "execution-evidence.json"),
                },
                "snapshotDigest": fixture["snapshotDigest"],
                "hardware": fixture["hardware"],
                "controllerPeakCommitBytes": peaks["controller"],
                "lifecycleTree": {
                    "processIds": sorted(members),
                    "peakJobCommitBytes": end["peakJobCommitBytes"],
                    "summedPeakWorkingSetBytes": sum(
                        row["peakWs"] for row in members.values()
                    ),
                    "samplesSha256": sample_hash,
                },
                "native": proc(native_pid, native, native_bytes),
                "publication": {
                    "kind": "media",
                    "process": proc(main_pid, main, publication_bytes),
                    "implementation": pub["implementation"],
                    "implementationSourcesSha256": pub["implementationSourcesSha256"],
                    "helperBinarySha256": helper_hashes[str(host)],
                    "inputSha256": pub["inputSha256"],
                    "publishedSha256": pub["publishedSha256"],
                    "publishedBytes": pub["publishedBytes"],
                    "peakDirectoryBytes": publication_bytes,
                    "reopenedVerified": True,
                },
                "gpu": {"status": "cpu_only"},
                "nativeExitedBeforePublication": True,
                "reviewedBudget": None,
            }
            write_json(run / "measurement.json", record)
            report.update(
                status=(
                    "MEASURED_PREVIOUS_BUILD"
                    if report["checkoutDifferences"] or report.get("ownerCheckoutDifferences")
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
    parser.add_argument("--owner-probe", type=Path, required=True)
    parser.add_argument("--owner-probe-sha256", required=True)
    parser.add_argument("--publication-probe", type=Path, required=True)
    parser.add_argument("--publication-probe-sha256", required=True)
    parser.add_argument("--case", choices=range(1, 11), type=int, required=True)
    parser.add_argument(
        "--version-id",
        required=True,
        help="Installed PocketTTS v2 version in the dedicated validation database",
    )
    parser.add_argument("--run-dir", type=Path, required=True)
    require(os.name == "nt", "Windows only")
    measure(parser.parse_args())
