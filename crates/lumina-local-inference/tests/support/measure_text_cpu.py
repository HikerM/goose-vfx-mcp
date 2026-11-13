"""Measure the exact CPU text launch before admitting a new GGUF content digest."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import secrets
import socket
import subprocess
import time
import urllib.error
import urllib.request

import psutil


def digest(path):
    value = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            value.update(chunk)
    return value.hexdigest()


def probe(args):
    root = args.out.resolve()
    root.mkdir(exist_ok=False)
    source = json.loads(args.component.read_bytes())["component"]["observation"]["files"]
    if len(source) != 1:
        raise ValueError("Expected one catalogued main file")
    artifact = source[0]["artifact"]
    if digest(args.model) != artifact["source_sha256"] or args.model.stat().st_size != artifact["size_bytes"]:
        raise ValueError("Local weight differs from the observed source")
    engine_digest = digest(args.engine)
    if engine_digest != "5031f8cc21505b931581b96e37270e1cc8c6c378d38b3dfc9b1c254db8f9c58e":
        raise ValueError("CPU helper differs from the pinned runtime")
    with socket.socket() as reservation:
        reservation.bind(("127.0.0.1", 0))
        port = reservation.getsockname()[1]
    alias = "resource-measurement"
    cmd = [str(args.engine), "-m", str(args.model), "--host", "127.0.0.1", "--port", str(port),
           "--alias", alias, "--ctx-size", "2048", "--batch-size", "512", "--ubatch-size", "512",
           "--parallel", "1", "--threads", "4", "--threads-batch", "4", "--gpu-layers", "0",
           "--cache-type-k", "f16", "--cache-type-v", "f16", "--flash-attn", "on",
           "--cache-ram", "0", "--ctx-checkpoints", "0", "--spec-type", "none", "--fit", "off",
           "--log-verbosity", "4", "--no-webui", "--no-warmup", "--offline", "--no-kv-offload",
           "--no-context-shift"]
    token = secrets.token_hex(32)
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))

    def request(path, body=None, timeout=180):
        payload = json.dumps(body).encode() if body is not None else None
        req = urllib.request.Request(f"http://127.0.0.1:{port}/{path}", data=payload,
                                     headers={"Authorization": "Bearer " + token, "Content-Type": "application/json"})
        with opener.open(req, timeout=timeout) as response:
            data = response.read(2 * 1024 * 1024 + 1)
            if len(data) > 2 * 1024 * 1024:
                raise ValueError("Oversized helper response")
            return json.loads(data)

    body = dict(model=alias, messages=[{"role": "user", "content": "Explain evaporation in one sentence."}],
                max_tokens=64, temperature=0, seed=42, chat_template_kwargs={"enable_thinking": False})
    launched = time.monotonic()
    log_path = root / "helper.log"
    with log_path.open("xb") as log:
        process = subprocess.Popen(cmd, stdout=log, stderr=subprocess.STDOUT, cwd=root,
                                   env=dict(os.environ, LLAMA_API_KEY=token),
                                   creationflags=subprocess.CREATE_NO_WINDOW)
        try:
            while True:
                if process.poll() is not None or time.monotonic() - launched > 240:
                    raise RuntimeError("Pinned helper failed to start")
                try:
                    if request("health", timeout=2)["status"] == "ok":
                        break
                except (urllib.error.URLError, TimeoutError):
                    pass
                time.sleep(0.25)
            result = request("v1/chat/completions", body)
            if result["model"] != alias or not result["choices"][0]["message"]["content"].strip():
                raise ValueError("Expected text from the actual pinned model")
            mem = psutil.Process(process.pid).memory_info()
        finally:
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(10)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(10)
    raw = log_path.read_bytes()
    log_path.write_bytes(raw.replace(token.encode(), b"[REDACTED]"))
    lines = [line for line in log_path.read_text(errors="replace").splitlines()
             if re.search(r"buffer size|n_ctx\s+=|n_batch\s+=|n_seq_max\s+=|n_ubatch\s+=", line)]
    compute = [float(v) for line in lines for v in re.findall(r"CPU compute buffer size\s*=\s*([\d.]+) MiB", line)]
    if not compute or mem.peak_wset <= 0 or mem.peak_pagefile <= 0:
        raise ValueError("Missing native buffer or Windows peak memory observation")
    if digest(args.model) != artifact["source_sha256"] or digest(args.engine) != engine_digest:
        raise ValueError("Measurement inputs changed")
    report = dict(schema="cpu-text-measurement-v1", source=artifact, source_revision=source[0]["snapshot"]["resolved_revision"],
                  engine_sha256=engine_digest, workload=dict(context=2048, batch=512, microbatch=512,
                  parallel=1, threads=4, gpu_layers=0, kv="f16", flash_attention=True),
                  argv=cmd, output=result, peak_working_set_bytes=mem.peak_wset,
                  peak_commit_bytes=mem.peak_pagefile, compute_mib=max(compute), memory_lines=lines,
                  log_sha256=digest(log_path), elapsed_s=time.monotonic() - launched)
    (root / "report.json").write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    for name in ("engine", "model", "component", "out"):
        parser.add_argument("--" + name, type=Path, required=True)
    probe(parser.parse_args())
