"""Standalone Windows Job accounting for the bounded PocketTTS measurement."""

import contextlib
import ctypes
from ctypes import wintypes
import datetime
import hashlib
import json
import os
import subprocess
import time

SAMPLE_INTERVAL = 0.02
ENVIRONMENT = {}
RUN_ROOT = None


def utc_now():
    return datetime.datetime.now(datetime.timezone.utc).isoformat()


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def artifact(path):
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return {
        "path": str(path),
        "bytes": path.stat().st_size,
        "sha256": digest.hexdigest(),
    }


class MemoryCounters(ctypes.Structure):
    _fields_ = [("cb", wintypes.DWORD), ("PageFaultCount", wintypes.DWORD)] + [
        (name, ctypes.c_size_t)
        for name in (
            "PeakWorkingSetSize",
            "WorkingSetSize",
            "QuotaPeakPagedPoolUsage",
            "QuotaPagedPoolUsage",
            "QuotaPeakNonPagedPoolUsage",
            "QuotaNonPagedPoolUsage",
            "PagefileUsage",
            "PeakPagefileUsage",
            "PrivateUsage",
        )
    ]


class JobBasicLimits(ctypes.Structure):
    _fields_ = [
        ("PerProcessUserTimeLimit", ctypes.c_int64),
        ("PerJobUserTimeLimit", ctypes.c_int64),
        ("LimitFlags", wintypes.DWORD),
        ("MinimumWorkingSetSize", ctypes.c_size_t),
        ("MaximumWorkingSetSize", ctypes.c_size_t),
        ("ActiveProcessLimit", wintypes.DWORD),
        ("Affinity", ctypes.c_size_t),
        ("PriorityClass", wintypes.DWORD),
        ("SchedulingClass", wintypes.DWORD),
    ]


class IoCounters(ctypes.Structure):
    _fields_ = [
        (name, ctypes.c_uint64)
        for name in (
            "ReadOperationCount",
            "WriteOperationCount",
            "OtherOperationCount",
            "ReadTransferCount",
            "WriteTransferCount",
            "OtherTransferCount",
        )
    ]


class JobLimits(ctypes.Structure):
    _fields_ = [
        ("BasicLimitInformation", JobBasicLimits),
        ("IoInfo", IoCounters),
        ("ProcessMemoryLimit", ctypes.c_size_t),
        ("JobMemoryLimit", ctypes.c_size_t),
        ("PeakProcessMemoryUsed", ctypes.c_size_t),
        ("PeakJobMemoryUsed", ctypes.c_size_t),
    ]


class JobAccounting(ctypes.Structure):
    _fields_ = [
        (name, ctypes.c_int64)
        for name in (
            "TotalUserTime",
            "TotalKernelTime",
            "ThisPeriodTotalUserTime",
            "ThisPeriodTotalKernelTime",
        )
    ] + [
        (name, wintypes.DWORD)
        for name in (
            "TotalPageFaultCount",
            "TotalProcesses",
            "ActiveProcesses",
            "TotalTerminatedProcesses",
        )
    ]


class ProcessEntry(ctypes.Structure):
    _fields_ = [
        ("dwSize", wintypes.DWORD),
        ("cntUsage", wintypes.DWORD),
        ("th32ProcessID", wintypes.DWORD),
        ("th32DefaultHeapID", ctypes.c_size_t),
        ("th32ModuleID", wintypes.DWORD),
        ("cntThreads", wintypes.DWORD),
        ("th32ParentProcessID", wintypes.DWORD),
        ("pcPriClassBase", wintypes.LONG),
        ("dwFlags", wintypes.DWORD),
        ("szExeFile", wintypes.WCHAR * 260),
    ]


class WindowsApi:
    def __init__(self):
        require(os.name == "nt", "This evidence runner requires Windows")
        self.kernel = ctypes.WinDLL("kernel32", use_last_error=True)
        self.psapi = ctypes.WinDLL("psapi", use_last_error=True)
        signatures = {
            "CreateJobObjectW": ([ctypes.c_void_p, wintypes.LPCWSTR], wintypes.HANDLE),
            "SetInformationJobObject": (
                [wintypes.HANDLE, ctypes.c_int, ctypes.c_void_p, wintypes.DWORD],
                wintypes.BOOL,
            ),
            "AssignProcessToJobObject": (
                [wintypes.HANDLE, wintypes.HANDLE],
                wintypes.BOOL,
            ),
            "QueryInformationJobObject": (
                [
                    wintypes.HANDLE,
                    ctypes.c_int,
                    ctypes.c_void_p,
                    wintypes.DWORD,
                    ctypes.c_void_p,
                ],
                wintypes.BOOL,
            ),
            "TerminateJobObject": ([wintypes.HANDLE, wintypes.UINT], wintypes.BOOL),
            "CloseHandle": ([wintypes.HANDLE], wintypes.BOOL),
            "CreateFileW": (
                [
                    wintypes.LPCWSTR,
                    wintypes.DWORD,
                    wintypes.DWORD,
                    ctypes.c_void_p,
                    wintypes.DWORD,
                    wintypes.DWORD,
                    wintypes.HANDLE,
                ],
                wintypes.HANDLE,
            ),
            "CreateToolhelp32Snapshot": (
                [wintypes.DWORD, wintypes.DWORD],
                wintypes.HANDLE,
            ),
            "Process32FirstW": (
                [wintypes.HANDLE, ctypes.POINTER(ProcessEntry)],
                wintypes.BOOL,
            ),
            "Process32NextW": (
                [wintypes.HANDLE, ctypes.POINTER(ProcessEntry)],
                wintypes.BOOL,
            ),
        }
        for name, (arguments, result) in signatures.items():
            function = getattr(self.kernel, name)
            function.argtypes, function.restype = arguments, result
        self.psapi.GetProcessMemoryInfo.argtypes = [
            wintypes.HANDLE,
            ctypes.POINTER(MemoryCounters),
            wintypes.DWORD,
        ]
        self.psapi.GetProcessMemoryInfo.restype = wintypes.BOOL

    @staticmethod
    def check(value):
        if not value:
            raise ctypes.WinError(ctypes.get_last_error())
        return value

    @contextlib.contextmanager
    def read_guard(self, path):
        handle = self.kernel.CreateFileW(str(path), 0x80000000, 1, None, 3, 0x80, None)
        if handle == ctypes.c_void_p(-1).value:
            raise ctypes.WinError(ctypes.get_last_error())
        try:
            yield
        finally:
            self.kernel.CloseHandle(handle)

    def memory(self, process):
        counters = MemoryCounters()
        counters.cb = ctypes.sizeof(counters)
        self.check(
            self.psapi.GetProcessMemoryInfo(
                wintypes.HANDLE(int(process._handle)),
                ctypes.byref(counters),
                counters.cb,
            )
        )
        return {
            name: int(getattr(counters, name))
            for name in (
                "WorkingSetSize",
                "PeakWorkingSetSize",
                "PrivateUsage",
                "PagefileUsage",
                "PeakPagefileUsage",
            )
        }


class OwnedJob:
    def __init__(self, api):
        self.api = api
        self.handle = api.check(api.kernel.CreateJobObjectW(None, None))
        limits = JobLimits()
        limits.BasicLimitInformation.LimitFlags = (
            0x2000  # JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
        )
        try:
            api.check(
                api.kernel.SetInformationJobObject(
                    self.handle, 9, ctypes.byref(limits), ctypes.sizeof(limits)
                )
            )
        except BaseException:
            self.close()
            raise

    def assign(self, process):
        self.api.check(
            self.api.kernel.AssignProcessToJobObject(
                self.handle, wintypes.HANDLE(int(process._handle))
            )
        )

    def accounting(self):
        info = JobAccounting()
        self.api.check(
            self.api.kernel.QueryInformationJobObject(
                self.handle, 1, ctypes.byref(info), ctypes.sizeof(info), None
            )
        )
        result = {
            name: int(getattr(info, name))
            for name in (
                "TotalProcesses",
                "ActiveProcesses",
                "TotalTerminatedProcesses",
            )
        }
        capacity = max(8, result["ActiveProcesses"])
        while capacity <= 4096:
            storage = ctypes.create_string_buffer(
                8 + ctypes.sizeof(ctypes.c_size_t) * capacity
            )
            if self.api.kernel.QueryInformationJobObject(
                self.handle, 3, storage, ctypes.sizeof(storage), None
            ):
                count = ctypes.c_uint32.from_buffer(storage, 4).value
                require(count <= capacity, "JobObject returned an invalid member count")
                result["ProcessIds"] = list(
                    (ctypes.c_size_t * count).from_buffer(storage, 8)
                )
                return result
            require(
                ctypes.get_last_error() == 234,
                "Could not enumerate the owned JobObject process IDs",
            )
            capacity *= 2
        raise RuntimeError("Owned JobObject process count exceeds the diagnostic bound")

    def terminate(self):
        self.api.check(self.api.kernel.TerminateJobObject(self.handle, 1))

    def close(self):
        if self.handle:
            self.api.kernel.CloseHandle(self.handle)
            self.handle = None


@contextlib.contextmanager
def exclusive_run():
    import msvcrt

    lock_path = RUN_ROOT / "single-model.lock"
    with lock_path.open("a+b") as lock:
        if lock.seek(0, os.SEEK_END) == 0:
            lock.write(b"0")
            lock.flush()
        lock.seek(0)
        try:
            msvcrt.locking(lock.fileno(), msvcrt.LK_NBLCK, 1)
        except OSError as error:
            raise RuntimeError(
                "Another validation invocation owns the measurement lock"
            ) from error
        try:
            yield
        finally:
            lock.seek(0)
            msvcrt.locking(lock.fileno(), msvcrt.LK_UNLCK, 1)


def run_process(
    api, argv, directory, phase, timeout, report, persist, output_limits=()
):
    stdout_path, stderr_path = (
        directory / f"{phase}.stdout.log",
        directory / f"{phase}.stderr.log",
    )
    entry = {
        "phase": phase,
        "argv": argv,
        "timeoutSeconds": timeout,
        "startedAt": utc_now(),
        "memoryIntervalSeconds": SAMPLE_INTERVAL,
        "memoryScope": "Same native PID: sampled working set/private bytes and OS-reported process peaks; not VRAM or a system-wide peak",
        "jobScope": "Private Windows kill-on-close JobObject; assigned immediately after spawn. The spawn-to-assignment crash window is not claimed atomic.",
        "stdout": str(stdout_path),
        "stderr": str(stderr_path),
        "memorySamples": 0,
        "memoryQueryFailures": 0,
    }
    report["processes"].append(entry)
    persist()
    job, process, assigned = OwnedJob(api), None, False
    started = time.monotonic()
    sample_path = directory / f"{phase}.memory.jsonl"
    try:
        with (
            stdout_path.open("xb") as stdout,
            stderr_path.open("xb") as stderr,
            sample_path.open("x", encoding="utf-8") as samples,
        ):
            process = subprocess.Popen(
                argv,
                cwd=directory,
                stdin=subprocess.DEVNULL,
                stdout=stdout,
                stderr=stderr,
                env={**os.environ, **ENVIRONMENT},
                creationflags=subprocess.CREATE_NO_WINDOW,
            )
            entry["pid"] = process.pid
            job.assign(process)
            assigned = True
            entry["jobAssignedAt"] = utc_now()
            persist()
            peaks = {}
            while True:
                try:
                    sample = api.memory(process)
                    for name, value in sample.items():
                        peaks[name] = max(peaks.get(name, 0), value)
                    samples.write(
                        json.dumps(
                            {
                                "elapsedSeconds": time.monotonic() - started,
                                "pid": process.pid,
                                **sample,
                            }
                        )
                        + "\n"
                    )
                    samples.flush()
                    entry["memorySamples"] += 1
                except OSError as error:
                    entry["memoryQueryFailures"] += 1
                    entry["lastMemoryQueryError"] = str(error)
                entry["memoryPeakBytes"] = peaks
                if process.poll() is not None:
                    break
                if time.monotonic() - started >= timeout:
                    entry["timedOut"] = True
                    raise TimeoutError(f"{phase} exceeded {timeout}s")
                for path, maximum in output_limits:
                    require(
                        not path.exists() or path.stat().st_size <= maximum,
                        f"Output exceeded validation bound: {path.name}",
                    )
                time.sleep(SAMPLE_INTERVAL)
            entry["exitCode"] = process.wait(timeout=30)
            require(
                entry["exitCode"] == 0,
                f"{phase} exited {entry['exitCode']}; retained stdout/stderr",
            )
    finally:
        # Only this newly created job can be terminated; no image-name or foreign PID kill.
        try:
            if process is not None:
                if assigned:
                    before = job.accounting()
                    entry["jobBeforeCleanup"] = before
                    if entry.get("exitCode") == 0 and before["ActiveProcesses"]:
                        # A private console host can exit just after its console client.
                        grace = time.monotonic() + 3
                        while before["ActiveProcesses"] and time.monotonic() < grace:
                            time.sleep(SAMPLE_INTERVAL)
                            before = job.accounting()
                    if before["ActiveProcesses"]:
                        entry["terminatedOwnedJob"] = True
                        job.terminate()
                    entry["exitCode"] = process.wait(timeout=30)
                    deadline = time.monotonic() + 30
                    while (
                        job.accounting()["ActiveProcesses"]
                        and time.monotonic() < deadline
                    ):
                        time.sleep(SAMPLE_INTERVAL)
                    entry["jobAfterCleanup"] = job.accounting()
                    entry["processTreeExitConfirmed"] = (
                        entry["jobAfterCleanup"]["ActiveProcesses"] == 0
                    )
                    require(
                        entry["processTreeExitConfirmed"],
                        "Owned process tree exit could not be confirmed",
                    )
                else:
                    if process.poll() is None:
                        process.kill()
                    entry["exitCode"] = process.wait(timeout=30)
                    entry["processTreeExitConfirmed"] = False
                    entry["cleanupBoundary"] = (
                        "Job assignment failed; direct child exited, descendant containment was not established"
                    )
            entry["finishedAt"] = utc_now()
            entry["elapsedSeconds"] = time.monotonic() - started
        finally:
            job.close()
            for key, path in (
                ("stdoutArtifact", stdout_path),
                ("stderrArtifact", stderr_path),
                ("memoryArtifact", sample_path),
            ):
                if path.is_file():
                    entry[key] = artifact(path)
            persist()
    return entry
