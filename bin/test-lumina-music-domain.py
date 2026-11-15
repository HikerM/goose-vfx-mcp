"""Prepare/measure a proposed Stable Audio prompt+seed domain; never register budgets.

prepare is inference-free. Token boundary cases stay unresolved unless supplied a
receipt from a source-inspected, caller-hash-pinned native SentencePiece oracle.
execute requires the extended existing host/owner probes, not the old fixed case.

Oracle JSON v1: {schemaVersion:1, implementation:'native-sentencepiece-encode-v1',
 binaryPath, binarySha256, tokenizerModelPath, tokenizerModelSha256,
 modelWeightsSha256, tokenizerEmbeddedSha256, modelConfigEmbeddedSha256,
 sources:{repoRelativePath:sha256}, samples:[{prompt, tokenIds:[integer,...]}]}.
The oracle must call the installed engine's load_sentencepiece_model and
tokenize_sentencepiece (Encode without BOS/EOS), using the extracted model below.
The explicit --oracle-sha256 fixes that independently built executable identity;
it is a reproducibility constraint, not a request for additional user approval.

Probe input env LUMINA_MUSIC_DOMAIN_CASE_FILE points to one immutable case JSON.
fixture.json/driver.json/owner.json/publication.json must echo domainCaseSha256.
Host uses real import_readers, creates project scope, and reports projectArchive
{verified:true,reopenedVerified:true}; owner retains its test-only bounded receipt.
This is measurement preparation, not production admission or model-quality proof.
Fresh-task publication does not measure project_reuse authorization or version creation.
"""

import argparse
from contextlib import ExitStack
import hashlib
import json
import os
from pathlib import Path
import re
import struct
import subprocess
import sys

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[1]
BASE = ROOT / "target/lifecycle-validation"
TOOLS = ROOT / "crates/lumina-local-inference/src/lifecycle/task_resources"
NATIVE = ROOT / "target/runtime-engines/audio-cpp/c0cd77fa80285f8113883e0de06ffc781cf502f6/source"
ENGINE_SHA = "5e53e8441457c990d11955873e513c29030e04c3d19229c62eeb03b0e118ad8b"
WEIGHTS_SHA = "a6d9176e11b857409b28cf5a69f4fefd8f3e61907b3b10d867f1f4c0a7eb9418"
ENGINE = NATIVE.parent / "validated-binaries" / ENGINE_SHA / "audiocpp_cli.exe"
SIDECARS = BASE / "m4-stable-music-offline-r3/temp/audiocpp-gguf/f96053412c9c4b11"
TOKENIZER = SIDECARS / "t5gemma-b-b-ul2/tokenizer.model"
MODEL_CONFIG = SIDECARS / "model_config.json"
MAX_BYTES = 1024
WEIGHTS = BASE / "m4-managed-audio-install-20261113-r1/model-lifecycle/blobs/sha256/a6" / WEIGHTS_SHA
ORACLE_SOURCE = TOOLS / "music_tokenizer_probe.cpp"
LEGACY_ORACLE_SOURCE = BASE / "project-music-tokenizer-20261115.cpp"
HOST_TEST = "media::task_output::tests::music_probe::measure_installed_music_publication"
FIXED = dict(negativePrompt=None, durationSeconds=4.0, steps=8,
             guidanceScale=1.0, apgScale=1.0, sampler="pingpong",
             durationPaddingSeconds=6.0, truncateToDuration=True, chunkedDecode=True)
NATIVE_FILES = ["src/framework/tokenizers/sentencepiece.cpp",
                "src/framework/assets/tensor_source.cpp",
                "src/models/stable_audio/conditioner.cpp",
                "src/models/stable_audio/t5gemma.cpp",
                "src/models/stable_audio/request.cpp",
                "src/models/stable_audio/session.cpp",
                "src/models/stable_audio/rf_dit.cpp",
                "src/models/stable_audio/sampler.cpp"]
HOST_SOURCES = {"crates/lumina/src/media/project_assets.rs",
                "crates/lumina/src/project/assets.rs",
                "crates/lumina/src/media/task_configuration.rs",
                "crates/lumina/src/media/task_output.rs",
                "crates/lumina/tests/support/music_publication_probe.rs"}


def require(ok, message):
    if not ok:
        raise ValueError(message)


def sha(path):
    digest = hashlib.sha256()
    with Path(path).open("rb") as stream:
        for chunk in iter(lambda: stream.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


def digest_bytes(value):
    return hashlib.sha256(value).hexdigest()


def read(path):
    with Path(path).open(encoding="utf-8") as stream:
        return json.load(stream)


def new_json(path, value):
    path = Path(path).resolve()
    require(path.drive.upper() in ("D:", "E:"), "Artifacts must stay on D/E")
    require(path.parent.is_dir(), "Artifact parent must already exist")
    with path.open("x", encoding="utf-8", newline="\n") as stream:
        json.dump(value, stream, ensure_ascii=False, indent=2, allow_nan=False)
        stream.write("\n")
        stream.flush()
        os.fsync(stream.fileno())


def exact_bytes(pattern, size):
    value = pattern * (size // len(pattern.encode("utf-8")))
    return value + "a" * (size - len(value.encode("utf-8")))


def case(identifier, prompt, seed, stage="boundary", raw_tokens=None):
    return dict(caseId=identifier, stage=stage, parameters={**FIXED, "prompt": prompt, "seed": seed},
                expectedRawTokenCount=raw_tokens,
                promptUtf8Bytes=None if prompt is None else len(prompt.encode("utf-8")))


def cases():
    baseline = "A gentle solo piano melody, clear notes, calm instrumental music, no vocals."
    result = [case(f"seed-{seed}", baseline, seed)
              for seed in (0, 1, 42, 2147483647, 2147483648, 4294967295)]
    result += [case("short-one-byte", "a", 7),
               case("unicode-cjk", "清澈钢琴与柔和弦乐，缓慢节奏，纯器乐，没有人声。", 123),
               case("unicode-combining", "Cafe\u0301, naïve strings; 雨音 🎹, gentle rhythm.", 456),
               case("utf8-1023", exact_bytes("钢琴 🎹 ", MAX_BYTES - 1), 9001),
               case("utf8-1024", exact_bytes("钢琴 🎹 ", MAX_BYTES), 9002),
               case("ascii-1024", exact_bytes("piano ", MAX_BYTES), 9003),
               case("combining-1024", exact_bytes("e\u0301", MAX_BYTES), 9004)]
    result += [case(f"tokens-{n}", None, 1000 + n, raw_tokens=n) for n in (255, 256, 257)]
    result += [case("holdout-english", "A dry marimba plays a syncopated ostinato beside soft brushed drums.",
                    17, "holdout"),
               case("holdout-unicode", "古筝与低音提琴对答，留白、轻微回响；sans voix, rythme paisible.",
                    3500000123, "holdout")]
    return result


def token_candidates():
    # Repetition lengths are search candidates, never asserted token counts.
    values = [c["parameters"]["prompt"] for c in cases() if c["parameters"]["prompt"] is not None]
    for word in ("piano", "music", "drum", "a"):
        values += [" ".join([word] * n) for n in range(240, 270)]
    return [p for p in dict.fromkeys(values) if len(p.encode("utf-8")) <= MAX_BYTES]


def source_hashes():
    return {(NATIVE / rel).relative_to(ROOT).as_posix(): sha(NATIVE / rel) for rel in NATIVE_FILES}


def model_token_limit():
    config = read(MODEL_CONFIG)
    conditioners = config["model"]["conditioning"]["configs"]
    text = [item for item in conditioners if item["id"] == "prompt"]
    require(len(text) == 1 and text[0]["config"]["max_length"] == 256,
            "Installed sidecar no longer establishes a 256-position prompt encoder")
    return 256


def oracle_samples(path, expected_binary_sha):
    receipt = read(path)
    require(receipt.get("schemaVersion") == 1 and
            receipt.get("implementation") == "native-sentencepiece-encode-v1", "Wrong tokenizer receipt protocol")
    require(expected_binary_sha and receipt["binarySha256"] == expected_binary_sha.lower(),
            "Explicit caller-pinned oracle binary SHA is required")
    binary = Path(receipt["binaryPath"]).resolve(strict=True)
    require(binary.is_relative_to(ROOT / "target") and sha(binary) == expected_binary_sha.lower(),
            "Oracle executable identity differs")
    model = Path(receipt["tokenizerModelPath"]).resolve(strict=True)
    require(model == TOKENIZER.resolve(strict=True) and sha(model) == receipt["tokenizerModelSha256"],
            "Oracle tokenizer model differs from the engine sidecar")
    require(receipt["modelWeightsSha256"] == WEIGHTS_SHA
            and receipt["tokenizerEmbeddedSha256"] == sha(TOKENIZER)
            and receipt["modelConfigEmbeddedSha256"] == sha(MODEL_CONFIG),
            "Oracle must verify the sidecars against the pinned GGUF embedded bytes")
    sources = receipt.get("sources", {})
    tokenizer_source = (NATIVE / NATIVE_FILES[0]).relative_to(ROOT).as_posix()
    require(tokenizer_source in sources, "Native tokenizer implementation fingerprint is absent")
    require((NATIVE / NATIVE_FILES[1]).relative_to(ROOT).as_posix() in sources,
            "Native GGUF sidecar extraction implementation fingerprint is absent")
    helper_sources = [path for path in (ORACLE_SOURCE, LEGACY_ORACLE_SOURCE)
                      if path.relative_to(ROOT).as_posix() in sources]
    require(len(helper_sources) == 1, "Tokenizer receipt must identify its actual helper compilation source")
    helper_source = helper_sources[0]
    require(sources[helper_source.relative_to(ROOT).as_posix()] == sha(ORACLE_SOURCE),
            "Historical compiled helper source differs from the versioned helper bytes")
    for rel, expected in sources.items():
        source = (ROOT / rel).resolve(strict=True)
        require(source.is_relative_to(ROOT) and sha(source) == expected, "Oracle source changed: " + rel)
    samples = {}
    for sample in receipt["samples"]:
        prompt, ids = sample["prompt"], sample["tokenIds"]
        require(isinstance(prompt, str) and isinstance(ids, list)
                and all(type(token) is int and token >= 0 for token in ids), "Invalid oracle sample")
        require(prompt not in samples, "Duplicate oracle prompt")
        samples[prompt] = ids
    return samples


def tokenize(args):
    require(os.name == "nt", "Tokenizer helper was built for Windows")
    oracle = args.oracle.resolve(strict=True)
    require(oracle.is_relative_to(BASE) and sha(oracle) == args.oracle_sha256.lower(),
            "Native tokenizer helper differs from the caller-pinned executable")
    require(sha(WEIGHTS) == WEIGHTS_SHA, "Installed GGUF differs from the fixed music weights")
    run = args.run_dir.resolve()
    require(run.parent == BASE.resolve() and not run.exists(), "Tokenizer artifacts require a new direct validation child")
    run.mkdir()
    prompts = token_candidates()
    request = run / "requests.bin"
    with request.open("xb") as stream:
        stream.write(struct.pack("<I", len(prompts)))
        for prompt in prompts:
            encoded = prompt.encode("utf-8")
            stream.write(struct.pack("<I", len(encoded)))
            stream.write(encoded)
    output = run / "native-tokens.json"
    command = [str(oracle), str(WEIGHTS), str(TOKENIZER), str(MODEL_CONFIG), str(request), str(output)]
    with (run / "stdout.log").open("xb") as out, (run / "stderr.log").open("xb") as err:
        completed = subprocess.run(command, cwd=run, env={**os.environ, "TMP":str(run), "TEMP":str(run)},
            stdin=subprocess.DEVNULL, stdout=out, stderr=err, timeout=120, creationflags=subprocess.CREATE_NO_WINDOW)
    require(completed.returncode == 0, "Native tokenizer helper failed; inspect its retained stderr")
    result = read(output)
    require(result["tokenizerEmbeddedMatches"] and result["modelConfigEmbeddedMatches"]
            and len(result["samples"]) == len(prompts), "Incomplete native tokenizer receipt")
    sources = {**source_hashes(), ORACLE_SOURCE.relative_to(ROOT).as_posix(): sha(ORACLE_SOURCE)}
    receipt = dict(schemaVersion=1, implementation="native-sentencepiece-encode-v1",
        binaryPath=str(oracle), binarySha256=args.oracle_sha256.lower(), tokenizerModelPath=str(TOKENIZER),
        tokenizerModelSha256=sha(TOKENIZER), modelWeightsSha256=WEIGHTS_SHA, modelWeightsPath=str(WEIGHTS),
        tokenizerEmbeddedSha256=sha(TOKENIZER), modelConfigEmbeddedSha256=sha(MODEL_CONFIG), sources=sources,
        nativeInputSha256=sha(request), nativeOutputSha256=sha(output), command=command,
        nativeLibraries={str(path):sha(path) for path in (
            NATIVE.parent / "build-cpu/external/sentencepiece/src/Release/sentencepiece.lib",
            NATIVE.parent / "build-cpu/ggml/src/Release/ggml-base.lib")},
        samples=[dict(prompt=prompt, tokenIds=row["tokenIds"]) for prompt,row in zip(prompts,result["samples"])])
    require(sha(oracle) == args.oracle_sha256.lower(), "Tokenizer executable changed during execution")
    new_json(args.output, receipt)
    verified = oracle_samples(args.output, args.oracle_sha256)
    print(json.dumps(dict(status="NATIVE_TOKENIZER_ONLY_COMPLETE", samples=len(verified),
        availableBoundaries=sorted(set(len(ids) for ids in verified.values()) & {255,256,257}),
        modelInferenceStarted=False, output=str(args.output.resolve()), sha256=sha(args.output))))


def prepare(args):
    token_limit = model_token_limit()
    prepared = cases()
    samples = oracle_samples(args.token_evidence, args.oracle_sha256) if args.token_evidence else {}
    for entry in prepared:
        prompt = entry["parameters"]["prompt"]
        target = entry["expectedRawTokenCount"]
        if prompt is None:
            choices = [p for p in token_candidates() if p in samples and len(samples[p]) == target]
            if choices:
                prompt = min(choices, key=lambda p: (len(p.encode("utf-8")), p))
                entry["parameters"]["prompt"] = prompt
                entry["promptUtf8Bytes"] = len(prompt.encode("utf-8"))
        if prompt in samples:
            ids = samples[prompt]
            entry["tokenEvidence"] = dict(rawCount=len(ids), effectiveCount=min(token_limit, len(ids)),
                rawIdsSha256=digest_bytes(json.dumps(ids, separators=(",", ":")).encode()),
                truncated=len(ids) > token_limit)
    pending = [c["caseId"] for c in prepared if "tokenEvidence" not in c]
    manifest = dict(schemaVersion=1, protocol="project-music-domain-measurement-v1",
        preparationTool=dict(path=Path(__file__).resolve().relative_to(ROOT).as_posix(), sha256=sha(__file__),
            versionedOracleSource=ORACLE_SOURCE.relative_to(ROOT).as_posix(), oracleSourceSha256=sha(ORACLE_SOURCE),
            historicalReceiptPreservesOriginalCompilationSource=True),
        status="TOKEN_EVIDENCE_COMPLETE_PROBES_REQUIRED" if not pending else "BLOCKED_TOKEN_EVIDENCE",
        scope="Unapproved candidate prompt/seed resource domain; no quality or production-admission claim",
        proposedDomain=dict(promptUtf8Bytes=dict(minimum=1, maximum=MAX_BYTES),
            seed=dict(minimum=0, maximum=4294967295), fixedParameters=FIXED,
            compilerRulesRemainRequired=True, rawTokenCountsAreNotWordCounts=True),
        engineSha256=ENGINE_SHA, modelWeightsSha256=WEIGHTS_SHA,
        tokenizerModelSha256=sha(TOKENIZER), modelConfigSha256=sha(MODEL_CONFIG),
        nativeSources=source_hashes(),
        tokenEvidence=None if not args.token_evidence else dict(path=str(args.token_evidence.resolve()),
            sha256=sha(args.token_evidence), oracleSha256=args.oracle_sha256.lower()),
        unresolvedCases=pending, cases=prepared,
        tokenizerRequests=[dict(prompt=p, promptSha256=digest_bytes(p.encode("utf-8"))) for p in token_candidates()],
        outsideDomain=[dict(reason="byte-bound", promptUtf8Bytes=MAX_BYTES + 1),
            dict(reason="fixed-field", steps=9), dict(reason="fixed-field", guidanceScale=2.0),
            dict(reason="fixed-field", negativePrompt="vocals")],
        executionContract=dict(hostTest=HOST_TEST, env="LUMINA_MUSIC_DOMAIN_CASE_FILE",
            requiredReports=["fixture.json", "driver.json", "owner.json", "publication.json"],
            requiredCaseReceipt="domainCaseSha256", requiredProjectReceipt="projectArchive.verified/reopenedVerified",
            oldFixedFixtureProbesAreRejected=True, noAutomaticBudgetRegistration=True))
    new_json(args.output, manifest)
    print(json.dumps(dict(status=manifest["status"], cases=len(prepared), unresolvedCases=pending,
                          manifest=str(args.output.resolve()), sha256=sha(args.output))))


def contains_marker(path, marker):
    tail = b""
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1 << 20), b""):
            if marker in tail + chunk:
                return True
            tail = chunk[-len(marker):]
    return False


def checked_executable(path, expected, prefix):
    path = path.resolve(strict=True)
    require(path.parent == (ROOT / "target/debug/deps").resolve(strict=True)
            and re.fullmatch(prefix + r"[-_a-zA-Z0-9.]*\.exe", path.name), "Unexpected probe binary path")
    require(sha(path) == expected.lower(), "Probe executable SHA mismatch")
    require(contains_marker(path, b"LUMINA_MUSIC_DOMAIN_CASE_FILE"), "Old fixed-case probe cannot measure a domain")
    return path


def execute(args):
    require(os.name == "nt", "Measurement requires Windows Job Objects")
    manifest = read(args.manifest)
    require(sha(args.manifest) == args.manifest_sha256.lower(), "Manifest SHA differs")
    require(manifest["status"] == "TOKEN_EVIDENCE_COMPLETE_PROBES_REQUIRED", "Resolve real tokenizer evidence first")
    require(manifest["nativeSources"] == source_hashes(), "Native sources changed since case preparation")
    require(manifest["engineSha256"] == ENGINE_SHA and manifest["modelWeightsSha256"] == WEIGHTS_SHA,
            "Manifest changes the fixed engine or weights")
    require(manifest["tokenizerModelSha256"] == sha(TOKENIZER)
            and manifest["modelConfigSha256"] == sha(MODEL_CONFIG) and model_token_limit() == 256,
            "Native tokenizer/config changed since preparation")
    receipt = manifest["tokenEvidence"]
    require(sha(receipt["path"]) == receipt["sha256"], "Tokenizer receipt changed")
    samples = oracle_samples(receipt["path"], receipt["oracleSha256"])
    selected = [c for c in manifest["cases"] if c["caseId"] == args.case]
    require(len(selected) == 1, "Unknown or duplicate case")
    entry = selected[0]
    params = entry["parameters"]
    require({k: params[k] for k in FIXED} == FIXED, "Case changes an unmeasured shape/CFG/steps field")
    require(1 <= len(params["prompt"].encode("utf-8")) <= MAX_BYTES
            and type(params["seed"]) is int and 0 <= params["seed"] <= 0xFFFFFFFF, "Case lies outside candidate range")
    require(entry["tokenEvidence"]["rawCount"] == len(samples[params["prompt"]]), "Token count differs")
    require(entry["expectedRawTokenCount"] is None or
            entry["expectedRawTokenCount"] == len(samples[params["prompt"]]), "Boundary is not the requested true token count")
    frozen = None
    if entry["stage"] == "holdout":
        require(args.frozen_budget and args.frozen_budget_sha256, "Holdout needs an independently frozen candidate budget")
        require(sha(args.frozen_budget) == args.frozen_budget_sha256.lower(), "Frozen holdout budget SHA differs")
        frozen = read(args.frozen_budget)
        require(frozen["manifestSha256"] == args.manifest_sha256.lower() and frozen["boundaryEvidence"],
                "Budget is not bound to this manifest and completed boundary evidence")
        measured_cases = set()
        for record in frozen["boundaryEvidence"]:
            require(sha(record["path"]) == record["sha256"], "Boundary evidence changed")
            evidence = read(record["path"])
            require(evidence["status"] == "MEASURED_REVIEW_REQUIRED"
                    and evidence["stage"] == "boundary"
                    and evidence["manifestSha256"] == args.manifest_sha256.lower(),
                    "Frozen envelope must use completed non-holdout cases from this manifest")
            require(evidence["caseId"] not in measured_cases, "Duplicate frozen boundary evidence")
            measured_cases.add(evidence["caseId"])
        require(measured_cases == {c["caseId"] for c in manifest["cases"] if c["stage"] == "boundary"},
                "Frozen envelope omits a declared boundary case")
    host = checked_executable(args.publication_probe, args.publication_probe_sha256, "lumina-")
    owner = checked_executable(args.owner_probe, args.owner_probe_sha256, "lumina_local_inference-")
    require(sha(ENGINE) == ENGINE_SHA, "Pinned engine changed")
    run = args.run_dir.resolve()
    require(run.parent == BASE.resolve() and not run.exists(), "Run must be a new direct validation child")
    sys.path.insert(0, str(TOOLS))
    import measure_music as music
    import measure_sd15 as monitor_tools
    import windows_job_measurement as jobs
    run.mkdir()
    (run / "output-root/native").mkdir(parents=True)
    case_path = run / "domain-case.json"
    new_json(case_path, {**entry, "manifestSha256": args.manifest_sha256.lower()})
    case_sha = sha(case_path)
    jobs.RUN_ROOT = BASE / "m4-managed-audio-install-20261113-r1"
    jobs.ENVIRONMENT.update(LUMINA_TASK_RESOURCE_OWNER=str(owner), LUMINA_TASK_RESOURCE_RUN_DIR=str(run),
        LUMINA_AUDIO_MUSIC_CPU_CLI=str(ENGINE), LUMINA_PATH_ROOT=str(run / "application"),
        LUMINA_TASK_RESOURCE_VERIFY="measurement", LUMINA_MUSIC_DOMAIN_CASE_FILE=str(case_path), TMP=str(run), TEMP=str(run))
    monitor_tools.ENGINE = ENGINE
    report = dict(status="PREPARING", caseId=args.case, stage=entry["stage"], processes=[],
        manifestSha256=args.manifest_sha256.lower(), domainCaseSha256=case_sha,
        runnerSha256=sha(__file__), publicationProbeSha256=sha(host), ownerProbeSha256=sha(owner),
        helpers={Path(module.__file__).name: sha(module.__file__) for module in (music, monitor_tools, jobs)},
        scope="Full Windows Job fresh-task native+owner+host publication including project archive; test-only measurement admission",
        limitations=["20ms disk sampling is a lower bound; checkpoints and conservative margin remain required",
                     "Does not measure project_reuse parent parsing, authorization persistence or version publication",
                     "Job assignment follows spawn; controller does not claim an atomic spawn/assignment window",
                     "Summed per-process OS WS peaks are conservative, not a simultaneous physical-RAM measurement"],
        frozenBudgetSha256=args.frozen_budget_sha256 if frozen else None)
    def persist():
        music.write_json(run / "result.json", report)
    api = jobs.WindowsApi()
    try:
        with ExitStack() as stack:
            stack.enter_context(jobs.exclusive_run())
            for path, expected in ((ENGINE, ENGINE_SHA), (host, args.publication_probe_sha256),
                                   (owner, args.owner_probe_sha256), (case_path, case_sha)):
                stack.enter_context(api.read_guard(path))
                require(sha(path) == expected.lower(), "Pinned measurement input changed")
            class DomainMonitor(music.MusicMonitor):
                def __init__(self, *a):
                    super().__init__(*a)
                    self.application_bytes = 0
                def sample(self, process):
                    self.application_bytes = max(self.application_bytes, monitor_tools.tree_bytes(run / "application"))
                    return super().sample(process)
                def finish(self, end, host_binary, owner_binary):
                    # Retained process handles let the final query capture a peak
                    # occurring between the last periodic sample and process exit.
                    for row in self.members.values():
                        final = self.read_memory(row["proc"])
                        row["ws"] = max(row["ws"], final["PeakWorkingSetSize"])
                        row["commit"] = max(row["commit"], final["PeakPagefileUsage"])
                    return super().finish(end, host_binary, owner_binary)
            monitor = DomainMonitor(run, api, stack)
            monitor.install()
            report["status"] = "RUNNING"
            persist()
            call = jobs.run_process(api, [str(host), "--exact", HOST_TEST, "--ignored", "--nocapture"],
                run, "lifecycle", 1200, report, persist, [(run / "output-root/native/output.wav", 32 << 20)])
            lines = Path(call["stdout"]).read_text(encoding="utf-8").splitlines()
            require(call["exitCode"] == 0 and call["processTreeExitConfirmed"]
                    and not call.get("terminatedOwnedJob") and call["memoryQueryFailures"] == 0
                    and lines.count(f"test {HOST_TEST} ... ok") == 1, "Incomplete full-tree measurement")
            end = call["jobAfterCleanup"]
            main, engine = monitor.finish(end, host, owner)
            fixture, driver, native, pub = [read(run / p) for p in
                ("fixture.json", "driver.json", "owner.json", "publication.json")]
            for doc in (fixture, driver, native, pub):
                require(doc.get("domainCaseSha256") == case_sha, "Probe ignored or substituted the case file")
            for label, doc in (("host", driver), ("owner", native)):
                require(doc.get("sources"), "Missing probe source fingerprints")
                for rel, expected in doc["sources"].items():
                    path = (ROOT / rel).resolve(strict=True)
                    require(path.is_relative_to(ROOT) and sha(path) == expected, label + " source mismatch: " + rel)
            require(HOST_SOURCES.issubset(driver["sources"]), "Project publication sources are not pinned")
            require(fixture["snapshot"] == read(run / "snapshot.json")
                    and fixture["snapshot"]["parameters"] == params
                    and fixture["key"]["kind"] == "music" and not fixture["key"]["requiresGpu"], "Case snapshot differs")
            require(pub["reopenedVerified"] and pub["projectArchive"]["verified"]
                    and pub["projectArchive"]["reopenedVerified"]
                    and pub["implementation"] == driver["implementation"] == "media::task_output::import_readers"
                    and pub["snapshotDigest"] == native["snapshotDigest"] == fixture["snapshotDigest"], "Publication identity differs")
            wav = sha(run / "output-root/native/output.wav")
            require(wav == native["outputSha256"] == pub["inputSha256"] == pub["publishedSha256"]
                    == sha(Path(pub["publishedPath"])) and pub["publishedBytes"] == 705644
                    and pub["result"]["mime"] == "audio/wav", "Generated/published/reopened WAV differs")
            require(manifest["nativeSources"] == source_hashes() and sha(case_path) == case_sha, "Sources/case changed during run")
            samples_sha = sha(run / "tree.jsonl")
            native_peak = max(native["outputBytes"], monitor.output_bytes)
            require(monitor.native_bytes > 40_176_400, "Missing cold engine sidecar footprint")
            publication_peak = max(monitor.publication_bytes, *(p["directoryLogicalBytes"] for p in pub["checkpoints"]))
            def metrics(item, disk):
                pid, row = item
                return dict(pid=pid, peakWorkingSetBytes=row["ws"], peakCommitBytes=row["commit"],
                    elapsedMillis=max(1, (row["finish"] - row["start"] + 9999) // 10000), exitCode=0,
                    processTreeExited=True, memorySamplesSha256=samples_sha, observedOutputPeakBytes=disk)
            report.update(job=end, nativeDirectoryHighWaterBytes=monitor.native_bytes,
                applicationDirectoryHighWaterBytes=monitor.application_bytes, publicationDirectoryHighWaterBytes=publication_peak,
                members={pid: {k: str(v) if k == "path" else v for k, v in row.items() if k != "proc"}
                         for pid, row in monitor.members.items()})
            new_json(run / "execution-evidence.json", report)
            measurement = dict(schemaVersion=2, rulesVersion="native-task-resources-v1", key=fixture["key"],
                provenance=dict(runnerSha256=sha(__file__), fixtureSha256=sha(run / "fixture.json"),
                    executionEvidenceSha256=sha(run / "execution-evidence.json")), snapshotDigest=fixture["snapshotDigest"],
                hardware=fixture["hardware"], controllerPeakCommitBytes=monitor.controller_peak,
                lifecycleTree=dict(processIds=sorted(monitor.members), peakJobCommitBytes=end["peakJobCommitBytes"],
                    summedPeakWorkingSetBytes=sum(row["ws"] for row in monitor.members.values()), samplesSha256=samples_sha),
                native=metrics(engine, native_peak), publication=dict(kind="media", process=metrics(main, publication_peak),
                    implementation=pub["implementation"], implementationSourcesSha256=pub["implementationSourcesSha256"],
                    helperBinarySha256=sha(host), inputSha256=wav, publishedSha256=wav, publishedBytes=pub["publishedBytes"],
                    peakDirectoryBytes=publication_peak, reopenedVerified=True), gpu=dict(status="cpu_only"),
                nativeExitedBeforePublication=True, reviewedBudget=None)
            new_json(run / "measurement.json", measurement)
            if frozen:
                budget = frozen["budget"]
                peak = monitor.controller_peak + max(end["peakJobCommitBytes"], measurement["lifecycleTree"]["summedPeakWorkingSetBytes"])
                require(peak <= budget["hostRamBytes"] and monitor.native_bytes <= budget["temporaryOutputBytes"]
                        and publication_peak + monitor.application_bytes <= budget["publicationStorageBytes"]
                        and max(row["finish"] for row in monitor.members.values()) - min(row["start"] for row in monitor.members.values())
                        <= budget["lifecycleTimeoutSeconds"] * 10_000_000, "Independent holdout exceeds frozen envelope")
            report.update(status="HOLDOUT_WITHIN_FROZEN_ENVELOPE" if frozen else "MEASURED_REVIEW_REQUIRED",
                          measurementSha256=sha(run / "measurement.json"), outputSha256=wav)
    except BaseException as error:
        report.update(status="FAILED", error=str(error))
        raise
    finally:
        persist()


def self_check():
    entries = cases()
    require(len({c["caseId"] for c in entries}) == len(entries), "Duplicate case")
    require({c["expectedRawTokenCount"] for c in entries if c["parameters"]["prompt"] is None} == {255, 256, 257}, "Missing true-token boundaries")
    for entry in entries:
        prompt = entry["parameters"]["prompt"]
        require(prompt is None or 1 <= len(prompt.encode("utf-8")) <= MAX_BYTES, "Invalid candidate byte bound")
        require({k: entry["parameters"][k] for k in FIXED} == FIXED, "Fixed parameters drifted")
    require(len(exact_bytes("钢琴 🎹 ", 1024).encode("utf-8")) == 1024, "UTF-8 edge construction failed")
    require(len([c for c in entries if c["stage"] == "holdout"]) == 2, "Missing independent holdouts")
    print(json.dumps(dict(status="HARNESS_SELF_CHECK_PASSED", cases=len(entries), tokenizerCandidates=len(token_candidates()),
                         inferenceStarted=False, budgetsRegistered=False)))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    modes = parser.add_subparsers(dest="mode", required=True)
    p = modes.add_parser("prepare")
    p.add_argument("--output", type=Path, required=True)
    p.add_argument("--token-evidence", type=Path)
    p.add_argument("--oracle-sha256")
    p = modes.add_parser("tokenize")
    for name in ("oracle", "run-dir", "output"):
        p.add_argument("--"+name, type=Path, required=True)
    p.add_argument("--oracle-sha256", required=True)
    p = modes.add_parser("execute")
    for name in ("manifest", "publication-probe", "owner-probe", "run-dir"):
        p.add_argument("--" + name, type=Path, required=True)
    for name in ("manifest-sha256", "publication-probe-sha256", "owner-probe-sha256", "case"):
        p.add_argument("--" + name, required=True)
    p.add_argument("--frozen-budget", type=Path)
    p.add_argument("--frozen-budget-sha256")
    modes.add_parser("self-check")
    args = parser.parse_args()
    if args.mode == "prepare":
        prepare(args)
    elif args.mode == "execute":
        execute(args)
    elif args.mode == "tokenize":
        tokenize(args)
    else:
        self_check()


if __name__ == "__main__":
    main()
