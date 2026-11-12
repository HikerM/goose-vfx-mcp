"""Exercise real MCP -> ACP media delivery and persisted session replay.

Uses existing, verified local model and audio fixtures. No downloads or model copies.
The returned audio is a prior native TTS result, not a new synthesis acceptance run.
"""

import argparse
import base64
import datetime
import hashlib
import json
import os
from pathlib import Path
import queue
import sqlite3
import subprocess
import sys
import threading
import time
import wave


ROOT = Path(__file__).resolve().parents[1]
BASE = ROOT / "target/lifecycle-validation"
TOOL = "emit_media"


def digest(path):
    value = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            value.update(block)
    return value.hexdigest()


def save(path, value):
    path.write_text(json.dumps(value, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")


def media_blocks(wav):
    return [
        {"type": "text", "text": "Existing native audio fixture follows."},
        {"type": "audio", "data": base64.b64encode(wav.read_bytes()).decode("ascii"),
         "mimeType": "audio/wav", "annotations": {"audience": ["user"], "priority": 0.75}},
        {"type": "resource_link", "uri": "mcp://media-forwarding/pocket-tts-english.wav",
         "name": "pocket-tts-english.wav", "title": "Native speech fixture",
         "description": "Existing 2.4 second native speech; media forwarding verification only.",
         "mimeType": "audio/wav", "size": wav.stat().st_size,
         "annotations": {"audience": ["user"], "priority": 1.0},
         "_meta": {"fixture": "media-forwarding", "sha256": digest(wav)}},
        {"type": "text", "text": "End of media fixture."},
    ]


def serve_fixture(wav, log):
    blocks = media_blocks(wav)
    with log.open("a", encoding="utf-8") as history:
        for line in sys.stdin:
            request = json.loads(line)
            history.write(json.dumps(request, ensure_ascii=False) + "\n")
            history.flush()
            if "id" not in request:
                continue
            method = request.get("method")
            if method == "initialize":
                result = {"protocolVersion": request["params"]["protocolVersion"],
                          "capabilities": {"tools": {}},
                          "serverInfo": {"name": "media-forwarding", "version": "1.0.0"}}
            elif method == "ping":
                result = {}
            elif method == "tools/list":
                result = {"tools": [{"name": TOOL,
                          "description": "Return the fixed audio and resource-link verification fixture. Call once with an empty object.",
                          "inputSchema": {"type": "object", "properties": {}, "additionalProperties": False},
                          "annotations": {"readOnlyHint": True, "destructiveHint": False,
                                          "idempotentHint": True, "openWorldHint": False}}]}
            elif method == "tools/call" and request["params"]["name"] == TOOL:
                assert request["params"].get("arguments", {}) == {}
                result = {"content": blocks, "isError": False}
            else:
                print(json.dumps({"jsonrpc": "2.0", "id": request["id"],
                                  "error": {"code": -32601, "message": "Unknown fixture method"}}), flush=True)
                continue
            print(json.dumps({"jsonrpc": "2.0", "id": request["id"], "result": result}), flush=True)


class AcpProcess:
    def __init__(self, cli, app, run, phase, model):
        self.events = []
        self.requests = []
        self.queue = queue.Queue()
        self.run = run
        self.phase = phase
        self.output = (run / (phase + ".stdout.jsonl")).open("xb")
        self.error = (run / (phase + ".stderr.log")).open("xb")
        env = dict(os.environ, LUMINA_PATH_ROOT=str(app), LUMINA_PROVIDER="local",
                   LUMINA_MODEL=model, LUMINA_MODE="auto", MODELSCOPE_TOKEN="",
                   TEMP="E:/Codex/temp", TMP="E:/Codex/temp", XDG_CACHE_HOME="E:/Codex/cache",
                   CUDA_CACHE_PATH="E:/Codex/cache/cuda", PYTHONDONTWRITEBYTECODE="1", RUST_LOG="warn")
        env.pop("LUMINA_LOCAL_DRAFT_MODEL", None)
        self.process = subprocess.Popen([str(cli), "acp"], cwd=ROOT, env=env,
                                        stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                        stderr=self.error, creationflags=subprocess.CREATE_NO_WINDOW)
        self.reader = threading.Thread(target=self.read, daemon=True)
        self.reader.start()

    def read(self):
        for raw in self.process.stdout:
            self.output.write(raw)
            self.output.flush()
            try:
                event = json.loads(raw)
            except (ValueError, UnicodeError):
                continue
            self.events.append(event)
            self.queue.put(event)

    def request(self, method, params, timeout=420):
        identity = len(self.requests) + 1
        request = {"jsonrpc": "2.0", "id": identity, "method": method, "params": params}
        self.requests.append(request)
        self.process.stdin.write((json.dumps(request) + "\n").encode("utf-8"))
        self.process.stdin.flush()
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            try:
                event = self.queue.get(timeout=0.5)
            except queue.Empty:
                assert self.process.poll() is None, "ACP exited before response"
                continue
            assert not ("method" in event and "id" in event), "Unexpected ACP client request"
            if event.get("id") == identity:
                assert "error" not in event, event
                return event["result"]
        raise TimeoutError("ACP request timed out: " + method)

    def close(self):
        if not self.process.stdin.closed:
            self.process.stdin.close()
        try:
            code = self.process.wait(timeout=40)
        except subprocess.TimeoutExpired:
            subprocess.run(["taskkill", "/PID", str(self.process.pid), "/T", "/F"],
                           capture_output=True, check=False)
            self.process.wait(timeout=10)
            raise
        finally:
            self.reader.join(timeout=5)
            self.output.close()
            self.error.close()
            save(self.run / (self.phase + ".requests.json"), self.requests)
        assert code == 0, "ACP exited unsuccessfully"


def notifications(events):
    return [event for event in events if event.get("method") == "session/update"]


def completed_media(updates, session, expected):
    matches = []
    for event in updates:
        assert event["params"]["sessionId"] == session
        update = event["params"]["update"]
        if update.get("sessionUpdate") != "tool_call_update" or "content" not in update:
            continue
        content = [block["content"] for block in update["content"] if block.get("type") == "content"]
        if any(block.get("type") == "audio" for block in content):
            assert update["status"] == "completed", update["status"]
            assert content == expected, "Media content, metadata or order changed"
            matches.append(update["toolCallId"])
    assert len(matches) == 1, "Expected one actual completed audio tool result"
    return matches[0]


def run(args):
    assert os.name == "nt", "This acceptance entry exercises the Windows desktop backend"
    run_dir = args.run_dir.resolve()
    assert run_dir.drive.upper() in {"D:", "E:"}
    assert run_dir.is_relative_to(BASE), "Evidence must stay in this checkout's validation directory"
    run_dir.mkdir()
    record = {"status": "RUNNING", "startedAt": datetime.datetime.now(datetime.timezone.utc).isoformat(),
              "scope": "Actual local-model MCP call, live ACP delivery and process-restart SQLite replay. Existing native TTS WAV; no new synthesis, MediaResultRef, file actions or packaged Electron acceptance."}
    process = None
    try:
        source = json.loads(args.model_source.read_text(encoding="utf-8"))
        model_path = Path(source["localPath"])
        tts = json.loads((ROOT / "documentation/docs/lumina-architecture/model-lifecycle-evidence/2026-11-01-audio-asr-tts-feasibility.json").read_text(encoding="utf-8"))["ttsOutput"]
        assert digest(args.wav) == tts["sha256"]
        with wave.open(str(args.wav), "rb") as sound:
            assert (sound.getnchannels(), sound.getframerate(), sound.getnframes()) == (1, 24000, 57600)
        inputs = [args.cli, args.wav, args.model_source, args.reference_registry, Path(__file__),
                  ROOT / "crates/lumina/src/acp/server.rs", ROOT / "crates/lumina/src/acp/server/load_session.rs"]
        frozen = {str(path.resolve()): digest(path) for path in inputs}
        assert model_path.stat().st_size == source["sizeBytes"] and digest(model_path) == source["sha256"]
        record.update(inputSha256=frozen, model={"path": str(model_path), "sha256": source["sha256"]})
        registry = json.loads(args.reference_registry.read_text(encoding="utf-8"))
        entry = registry["models"][0]
        model = source["repository"] + ":media-forwarding"
        entry.update(id=model, repo_id=source["repository"], filename=source["file"],
                     local_path=str(model_path), size_bytes=source["sizeBytes"], mmproj_path=None,
                     mmproj_size_bytes=0, shard_files=[], quantization="q4_k_m")
        entry["settings"].update(n_gpu_layers=0, n_threads=12, context_size=16384,
                                 max_output_tokens=192, enable_thinking=False, vision_capable=False,
                                 draft_model=None, mmproj_size_bytes=0)
        app = run_dir / "app"
        (app / "data/models").mkdir(parents=True)
        save(app / "data/models/registry.json", registry)
        fixture_log = run_dir / "mcp.requests.jsonl"
        server = {"name": "mediaforwarding", "command": sys.executable,
                  "args": ["-B", str(Path(__file__).resolve()), "--serve-fixture", str(args.wav.resolve()), str(fixture_log)],
                  "env": [{"name": "PYTHONDONTWRITEBYTECODE", "value": "1"}]}
        process = AcpProcess(args.cli, app, run_dir, "live", model)
        process.request("initialize", {"protocolVersion": 1, "clientCapabilities": {}})
        session = process.request("session/new", {"cwd": str(ROOT), "mcpServers": [server]})["sessionId"]
        available = process.request("_lumina/unstable/tools/list", {"sessionId": session})["tools"]
        assert any(tool["name"] == TOOL or tool["name"].endswith("__" + TOOL) for tool in available), \
            "MCP media fixture was not activated"
        record["availableTools"] = [tool["name"] for tool in available]
        process.request("session/prompt", {"sessionId": session, "prompt": [{"type": "text", "text":
                        "Call the emit_media tool exactly once with {} to return its audio and resource fixture. Then reply only MEDIA_DELIVERED. Do not reproduce or describe binary data."}]})
        expected = media_blocks(args.wav)
        live = notifications(process.events)
        tool_id = completed_media(live, session, expected)
        process.close()
        process = None
        save(run_dir / "live-updates.json", live)
        process = AcpProcess(args.cli, app, run_dir, "replay", model)
        process.request("initialize", {"protocolVersion": 1, "clientCapabilities": {}})
        process.request("session/load", {"sessionId": session, "cwd": str(ROOT), "mcpServers": []})
        replay = notifications(process.events)
        assert completed_media(replay, session, expected) == tool_id
        process.close()
        process = None
        save(run_dir / "replay-updates.json", replay)
        calls = [json.loads(line) for line in fixture_log.read_text(encoding="utf-8").splitlines()]
        calls = [call for call in calls if call.get("method") == "tools/call"]
        assert len(calls) == 1 and calls[0]["params"]["name"] == TOOL
        database = app / "data/sessions/sessions.db"
        with sqlite3.connect(database.as_uri() + "?mode=ro", uri=True) as connection:
            assert connection.execute("PRAGMA integrity_check").fetchone() == ("ok",)
            persisted = connection.execute("SELECT content_json FROM messages WHERE session_id=?", (session,)).fetchall()
        assert any(expected[1]["data"] in row[0] and expected[2]["uri"] in row[0] for row in persisted)
        assert frozen == {str(path.resolve()): digest(path) for path in inputs}
        assert digest(model_path) == source["sha256"]
        record.update(status="PASS", sessionId=session, toolCallId=tool_id, actualMcpCalls=len(calls),
                      audioSha256=tts["sha256"], audioDurationSeconds=2.4,
                      liveUpdatesSha256=digest(run_dir / "live-updates.json"),
                      replayUpdatesSha256=digest(run_dir / "replay-updates.json"),
                      persistedMessageRows=len(persisted), databaseIntegrity="ok")
    except Exception as error:
        record.update(status="FAIL", failure=repr(error))
        raise
    finally:
        if process is not None:
            try:
                process.close()
            except Exception as cleanup_error:
                record["cleanupFailure"] = repr(cleanup_error)
        record["finishedAt"] = datetime.datetime.now(datetime.timezone.utc).isoformat()
        save(run_dir / "result.json", record)
        print(json.dumps({"status": record["status"], "evidence": str(run_dir / "result.json"),
                          "actualMcpCalls": record.get("actualMcpCalls"),
                          "audioDurationSeconds": record.get("audioDurationSeconds"),
                          "scope": record["scope"]}), flush=True)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--serve-fixture", nargs=2, type=Path, metavar=("WAV", "LOG"))
    parser.add_argument("--execute", action="store_true")
    parser.add_argument("--run-dir", type=Path)
    parser.add_argument("--cli", type=Path, default=ROOT / "target/debug/lumina.exe")
    parser.add_argument("--model-source", type=Path, default=BASE / "moe-cuda-source.json")
    parser.add_argument("--reference-registry", type=Path, default=BASE / "selftest-clean-entry-app/data/models/registry.json")
    parser.add_argument("--wav", type=Path, default=BASE / "audio-runtime-results/pocket-tts-english.wav")
    arguments = parser.parse_args()
    if arguments.serve_fixture:
        serve_fixture(*arguments.serve_fixture)
    else:
        assert arguments.execute and arguments.run_dir, "Pass --execute and a new --run-dir to run real inference"
        run(arguments)
