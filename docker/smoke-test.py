#!/usr/bin/env python3
"""Exercise a fresh image, persistence, and offline restart using only Docker."""

import argparse
import json
from pathlib import Path
import subprocess
import time
import uuid


def docker(*args, check=True):
    return subprocess.run(
        ["docker", *args], check=check, text=True, capture_output=True
    ).stdout.strip()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("image")
    parser.add_argument("--model", default="gemma4:e4b")
    parser.add_argument("--timeout", type=int, default=1800)
    args = parser.parse_args()
    name = f"documentllm-smoke-{uuid.uuid4().hex[:12]}"
    volume = f"{name}-data"

    def request(path, *options):
        return docker(
            "exec", name, "curl", "--fail", "--silent", "--show-error",
            "--max-time", "180", *options, f"http://127.0.0.1:3001{path}",
        )

    def start(offline=False):
        network = ["--network", "none"] if offline else []
        docker(
            "run", "-d", "--name", name, *network,
            "-e", f"DOCUMENTLLM_MODEL={args.model}",
            "-v", f"{volume}:/data", args.image,
        )
        deadline = time.monotonic() + args.timeout
        while time.monotonic() < deadline:
            state = json.loads(docker("inspect", "--format", "{{json .State}}", name))
            if not state["Running"]:
                raise RuntimeError(f"Container exited: {state['ExitCode']}")
            result = subprocess.run(
                ["docker", "exec", name, "healthcheck.sh"],
                capture_output=True, timeout=15,
            )
            if result.returncode == 0:
                return
            time.sleep(2)
        raise TimeoutError("Container did not become ready")

    def stop():
        docker("stop", "--time", "30", name)
        assert docker("inspect", "--format", "{{.State.ExitCode}}", name) == "0"
        docker("rm", name)

    try:
        print(f"Testing fresh startup and {args.model} download...", flush=True)
        start()
        assert docker("exec", name, "id", "-u") == "10001"
        models = json.loads(request("/v1/models"))["data"]
        assert args.model in [model["id"] for model in models]
        # Loading the model catches missing native runtime libraries, even on CPU-only hosts.
        print("Testing CPU inference...", flush=True)
        generated = docker(
            "exec", name, "curl", "--fail", "--silent", "--show-error",
            "--max-time", "300", "-H", "Content-Type: application/json", "-d",
            json.dumps({"model": args.model, "prompt": "Reply with OK.", "stream": True,
                        "think": False, "options": {"num_predict": 8, "num_ctx": 2048}}),
            "http://127.0.0.1:11434/api/generate",
        )
        chunks = [json.loads(line) for line in generated.splitlines() if line.strip()]
        assert chunks[-1]["done"] is True
        assert chunks[-1]["eval_count"] > 0
        assert '<div id="app">' in request("/")
        assert json.loads(request("/upload"))["documents"] == []
        docker("cp", str(Path(__file__).with_name("smoke-document.md")), f"{name}:/tmp/document.md")
        uploaded = json.loads(request("/upload", "-F", "documents=@/tmp/document.md"))
        document_id = uploaded["documents"][0]["id"]
        payload = json.dumps({"document_ids": [document_id]})
        ingested = json.loads(request("/ingest", "-X", "PUT", "-H", "Content-Type: application/json", "-d", payload))
        assert ingested["chunk_count"] > 0
        assert f"Downloading {args.model}" in docker("logs", name)
        stop()

        print("Testing cached startup without network access...", flush=True)
        start(offline=True)
        logs = docker("logs", name)
        assert f"Using cached model: {args.model}" in logs
        assert f"Downloading {args.model}" not in logs
        assert json.loads(request("/upload")) == uploaded
        docker("exec", name, "test", "-d", "/data/database.lancedb")
        # Embedding and ingestion must work from the cache with no internet.
        ingested = json.loads(request("/ingest", "-X", "PUT", "-H", "Content-Type: application/json", "-d", payload))
        assert ingested["chunk_count"] > 0
        stop()
        print("Container smoke test passed.", flush=True)
    except Exception:
        subprocess.run(["docker", "logs", "--tail", "80", name], check=False)
        raise
    finally:
        docker("rm", "-f", name, check=False)
        docker("volume", "rm", volume, check=False)


if __name__ == "__main__":
    main()
