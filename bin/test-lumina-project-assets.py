"""Run existing Windows host project-asset regressions without compiling or inference."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import time


ROOT = Path(__file__).resolve().parent.parent
SCOPE = "Synthetic project-asset storage, publication and query regressions; not real model inference, natural-language editing or desktop acceptance"
SUITES = (
    "session::session_ids::tests::",
    "project::assets::tests::",
    "media::project_assets::tests::",
    "agents::platform_extensions::native_tasks::project_assets_tests::",
)
SUMMARY = re.compile(
    r"^test result: (ok|FAILED)\. (\d+) passed; (\d+) failed; (\d+) ignored; "
    r"(\d+) measured; (\d+) filtered out; finished in [^\r\n]+$", re.M
)
RUNNING = re.compile(r"^running (\d+) tests?\s*$", re.M)
TEST = re.compile(r"^test (\S+) \.\.\. (ok|FAILED|ignored)(?:, [^\r\n]*)?\s*$", re.M)


def digest(path):
    sha = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            sha.update(chunk)
    return sha.hexdigest()


def binary_path():
    value = os.environ.get("LUMINA_PROJECT_ASSET_TEST_BINARY")
    if not value:
        raise RuntimeError("LUMINA_PROJECT_ASSET_TEST_BINARY is required; no binary fallback is allowed")
    path = Path(value)
    if not path.is_absolute():
        raise RuntimeError("Test binary must have an absolute path")
    path = path.resolve(strict=True)
    if (not path.is_file() or path.parent != (ROOT / "target/debug/deps").resolve(strict=True)
            or re.fullmatch(r"lumina-[0-9a-f]+\.exe", path.name) is None):
        raise RuntimeError("Test binary must be this repository's target/debug/deps/lumina-<hash>.exe")
    return path


def report_path(value):
    path = Path(value).resolve()
    if path.drive.upper() not in ("D:", "E:"):
        raise RuntimeError("Report must be a new file on D: or E:")
    if path.exists():
        raise RuntimeError("Report already exists; refusing to overwrite it")
    path.parent.mkdir(parents=True, exist_ok=True)
    path = path.parent.resolve(strict=True) / path.name
    if path.drive.upper() not in ("D:", "E:"):
        raise RuntimeError("Resolved report directory is outside D: and E:")
    return path


def parse_counts(output, prefix):
    summaries = SUMMARY.findall(output)
    running = RUNNING.findall(output)
    cases = TEST.findall(output)
    if len(summaries) != 1 or len(running) != 1:
        raise RuntimeError("Expected exactly one complete Rust summary and running-test count")
    verdict, *numbers = summaries[0]
    counts = dict(zip(("passed", "failed", "ignored", "measured", "filteredOut"), map(int, numbers)))
    counts["total"] = sum(counts[key] for key in ("passed", "failed", "ignored"))
    observed = {"ok": 0, "FAILED": 0, "ignored": 0}
    names = set()
    for name, status in cases:
        if not name.startswith(prefix) or name in names:
            raise RuntimeError("Test names do not match the requested suite or are duplicated")
        names.add(name)
        observed[status] += 1
    if (counts["total"] <= 0 or int(running[0]) != counts["total"]
            or len(cases) != counts["total"] or counts["measured"] != 0
            or (observed["ok"], observed["FAILED"], observed["ignored"])
            != (counts["passed"], counts["failed"], counts["ignored"])):
        raise RuntimeError("Rust per-test outcomes and summary counts disagree or no tests ran")
    return verdict, counts


def run_suite(binary, expected_sha, prefix):
    started = time.monotonic()
    result = {"suite": prefix, "binarySha256": expected_sha, "scope": SCOPE,
              "status": "FAIL", "testCounts": None}
    output = ""
    try:
        if digest(binary) != expected_sha:
            raise RuntimeError("Test binary changed before suite execution")
        command = [str(binary), prefix, "--test-threads=1", "--color", "never"]
        result["command"] = command
        process = subprocess.run(command, cwd=ROOT, stdout=subprocess.PIPE,
                                 stderr=subprocess.STDOUT, encoding="utf-8", errors="replace",
                                 timeout=180, check=False, creationflags=subprocess.CREATE_NO_WINDOW)
        output = process.stdout
        result["exitCode"] = process.returncode
        verdict, counts = parse_counts(output, prefix)
        result["testCounts"] = counts
        if digest(binary) != expected_sha:
            raise RuntimeError("Test binary changed during suite execution")
        if process.returncode != 0 or verdict != "ok" or counts["failed"] or counts["ignored"]:
            raise RuntimeError("Suite failed or ignored required regressions")
        result["status"] = "PASS"
    except subprocess.TimeoutExpired as error:
        output = error.stdout or ""
        if isinstance(output, bytes):
            output = output.decode("utf-8", errors="replace")
        result["error"] = "Suite exceeded its 180-second timeout"
    except Exception as error:
        result["error"] = str(error)
    result["elapsedSeconds"] = round(time.monotonic() - started, 3)
    if result["status"] != "PASS":
        result["diagnostic"] = output[-8000:]
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--report-path", required=True)
    arguments = parser.parse_args()
    started = time.monotonic()
    report = {"status": "FAIL", "scope": SCOPE, "suites": []}
    destination = None
    try:
        if os.name != "nt":
            raise RuntimeError("This driver requires the Windows host test binary")
        destination = report_path(arguments.report_path)
        binary = binary_path()
        sha = digest(binary)
        report.update(binary=str(binary), binarySha256=sha)
        report["suites"] = [run_suite(binary, sha, prefix) for prefix in SUITES]
        if all(suite["status"] == "PASS" for suite in report["suites"]):
            report["status"] = "PASS"
        report["countsComplete"] = all(suite["testCounts"] is not None for suite in report["suites"])
        report["totals"] = {
            key: sum(suite["testCounts"][key] for suite in report["suites"])
            for key in ("passed", "failed", "ignored", "total")
        } if report["countsComplete"] else None
    except Exception as error:
        report["error"] = str(error)
    report["elapsedSeconds"] = round(time.monotonic() - started, 3)
    if destination is not None:
        report["reportPath"] = str(destination)
        try:
            with destination.open("x", encoding="utf-8", newline="\n") as stream:
                json.dump(report, stream, ensure_ascii=True, indent=2)
                stream.write("\n")
                stream.flush()
                os.fsync(stream.fileno())
            if json.loads(destination.read_text(encoding="utf-8")) != report:
                raise RuntimeError("Report read-back differs from the written result")
        except Exception as error:
            report["status"] = "FAIL"
            report["reportError"] = str(error)
    print(json.dumps(report, ensure_ascii=True))
    return 0 if report["status"] == "PASS" else 1


if __name__ == "__main__":
    sys.exit(main())
