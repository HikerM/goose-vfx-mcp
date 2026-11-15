"""Run the Windows installed-model contract suites and report actual Rust test results."""

import json
import os
from pathlib import Path
import re
import subprocess
import sys


ROOT = Path(__file__).resolve().parent.parent
RESULT = re.compile(r"^test result: ok\. (\d+) passed; (\d+) failed; (\d+) ignored;", re.M)
SHARED = "language_weights_share_the_distribution_record_in_either_install_order"
DUAL = "both_installed_weights_prepare_in_one_store"


def run_suite(arguments, expected=None):
    command = [str(ROOT / "bin" / "cargo.cmd"), "test", "--offline", "-p",
               "lumina-local-inference", *arguments]
    process = subprocess.run(command, cwd=ROOT, stdout=subprocess.PIPE,
                             stderr=subprocess.STDOUT, encoding="utf-8", errors="replace")
    summaries = RESULT.findall(process.stdout)
    ok = process.returncode == 0 and len(summaries) == 1
    if ok:
        passed, failed, ignored = map(int, summaries[0])
        ok = passed > 0 and failed == ignored == 0
        if expected:
            ok = ok and passed == 1 and re.search(
                rf"^test (?:[\w:]+::)?{expected} \.\.\. ok\s*$", process.stdout, re.M
            ) is not None
    result = {"arguments": arguments, "exitCode": process.returncode, "status": "PASS" if ok else "FAIL"}
    if summaries:
        result["testCounts"] = [dict(zip(("passed", "failed", "ignored"), map(int, counts)))
                                for counts in summaries]
    if not ok:
        result["diagnostic"] = process.stdout[-4000:]
    if expected == DUAL and ok:
        rows = [json.loads(line) for line in process.stdout.splitlines()
                if line.startswith('{"installedVersions"')]
        result["installationOrders"] = rows
        if not (len(rows) == 2 and all(r["installedVersions"] == r["sharedSpecReferences"] == 2 for r in rows)
                and rows[0]["first"] != rows[0]["second"]
                and (rows[0]["first"], rows[0]["second"]) == (rows[1]["second"], rows[1]["first"])
                and len(set(rows[0]["parameterSchemas"])) == 2
                and rows[0]["parameterSchemas"] == rows[1]["parameterSchemas"][::-1]):
            result["status"] = "FAIL"
    return result


def main():
    if os.name != "nt":
        raise RuntimeError("This product validation uses the Windows Cargo entry point")
    suites = [
        run_suite(["--test", "lifecycle_task_parameters"]),
        run_suite(["--test", "lifecycle_audio_task_parameters"]),
        run_suite(["--test", "lifecycle_task_resources"]),
        run_suite(["--lib", SHARED], SHARED),
    ]
    keys = ["LUMINA_AUDIO_OBSERVATION_PAIR", "LUMINA_AUDIO_CATALOG"]
    missing = [key for key in keys if not Path(os.environ.get(key, "")).is_file()]
    if missing:
        suites.append({"name": DUAL, "status": "UNVERIFIED", "missingInputs": missing})
    else:
        suites.append(run_suite(["--test", "lifecycle_managed_audio", DUAL, "--",
                                 "--exact", "--ignored", "--nocapture"], DUAL))
    status = "FAIL" if any(s["status"] == "FAIL" for s in suites) else (
        "UNVERIFIED" if missing else "PASS")
    print(json.dumps({"status": status, "suites": suites}, ensure_ascii=True))
    return {"PASS": 0, "FAIL": 1, "UNVERIFIED": 2}[status]


if __name__ == "__main__":
    sys.exit(main())
