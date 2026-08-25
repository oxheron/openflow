#!/usr/bin/env python3
"""Black-box tests for both persistent native worker executables."""

from __future__ import annotations

import base64
import json
import os
import struct
import subprocess
import sys
from pathlib import Path
from typing import Any

MAX_FRAME_BYTES = 16 * 1024 * 1024


class Worker:
    def __init__(self, executable: Path) -> None:
        self.process = subprocess.Popen(
            [str(executable)],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        self.next_id = 1

    def call(self, command: str, params: dict[str, Any] | None = None) -> dict[str, Any]:
        request_id = self.next_id
        self.next_id += 1
        body = json.dumps(
            {"id": request_id, "command": command, "params": params or {}},
            separators=(",", ":"),
        ).encode("utf-8")
        assert len(body) <= MAX_FRAME_BYTES
        assert self.process.stdin is not None
        self.process.stdin.write(struct.pack(">I", len(body)) + body)
        self.process.stdin.flush()

        assert self.process.stdout is not None
        header = self.process.stdout.read(4)
        assert len(header) == 4, self._diagnostics("worker ended before response header")
        (length,) = struct.unpack(">I", header)
        assert length <= MAX_FRAME_BYTES
        payload = self.process.stdout.read(length)
        assert len(payload) == length, self._diagnostics("worker returned a truncated response")
        response = json.loads(payload)
        assert response["id"] == request_id
        return response

    def close(self) -> None:
        response = self.call("shutdown")
        assert response["ok"] is True
        return_code = self.process.wait(timeout=5)
        assert return_code == 0, self._diagnostics(f"worker exited with {return_code}")

    def _diagnostics(self, message: str) -> str:
        assert self.process.stderr is not None
        return f"{message}: {self.process.stderr.read().decode('utf-8', errors='replace')}"


def require_ok(response: dict[str, Any]) -> dict[str, Any]:
    assert response["ok"] is True, response
    result = response.get("result")
    assert isinstance(result, dict), response
    return result


def check_common(worker: Worker, identity: str) -> list[str]:
    ping = require_ok(worker.call("ping"))
    assert ping["worker"] == identity
    assert ping["protocol_version"] == 1
    listed = require_ok(worker.call("list_backends"))
    backends = listed["backends"]
    assert "mock" in backends
    assert "cpu" in listed["compute_backends"]
    expected_compute = os.environ.get("OPENFLOW_EXPECT_COMPUTE_BACKEND", "")
    if expected_compute and expected_compute != "cpu":
        assert expected_compute in listed["compute_backends"], listed

    rejected = worker.call("definitely_not_a_command")
    assert rejected["ok"] is False
    assert rejected["error"]["code"] == "invalid_request"
    # A command error must not desynchronize or terminate the framed stream.
    assert require_ok(worker.call("ping"))["worker"] == identity
    return backends


def check_asr(executable: Path) -> None:
    worker = Worker(executable)
    backends = check_common(worker, "openflow-asr-worker")
    if os.environ.get("OPENFLOW_EXPECT_REAL_BACKENDS") == "1":
        assert "whisper.cpp" in backends
    assert require_ok(worker.call("load_model", {"backend": "mock"}))["backend"] == "mock"
    require_ok(
        worker.call(
            "start_session",
            {"session_id": "asr-test", "language": "auto", "initial_prompt": "OpenFlow"},
        )
    )
    ambiguous = worker.call(
        "transcribe",
        {
            "session_id": "asr-test",
            "samples": [0.0],
            "samples_s16le_base64": "AAA=",
        },
    )
    assert ambiguous["ok"] is False
    assert ambiguous["error"]["code"] == "invalid_request"
    compatibility_result = require_ok(
        worker.call(
            "transcribe",
            {
                "session_id": "asr-test",
                "samples": [0.0] * 320,
                "mock_text": " hello world",
                "mock_probabilities": [0.9, 0.4],
                "final": False,
            },
        )
    )
    assert compatibility_result["text"] == " hello world"
    assert compatibility_result["hypotheses"][0]["text"] == " hello world"
    assert len(compatibility_result["hypotheses"][0]["tokens"]) == 2
    assert len(compatibility_result["hypotheses"][0]["segments"]) == 1
    assert compatibility_result["hypotheses"][0]["mean_log_probability"] < 0.0
    result = require_ok(
        worker.call(
            "transcribe",
            {
                "session_id": "asr-test",
                "samples_s16le_base64": base64.b64encode(bytes(640)).decode("ascii"),
                "mock_text": " hello world",
                "mock_probabilities": [0.9, 0.4],
                "final": True,
            },
        )
    )
    assert result["text"] == " hello world"
    assert result["language"] == "en"
    assert len(result["tokens"]) == 2
    assert result["hypotheses"][0]["text"] == result["text"]
    require_ok(worker.call("end_session", {"session_id": "asr-test"}))
    worker.close()


def check_llm(executable: Path) -> None:
    worker = Worker(executable)
    backends = check_common(worker, "openflow-llm-worker")
    if os.environ.get("OPENFLOW_EXPECT_REAL_BACKENDS") == "1":
        assert "llama.cpp" in backends
    assert require_ok(worker.call("load_model", {"backend": "mock"}))["backend"] == "mock"
    require_ok(worker.call("start_session", {"session_id": "llm-test"}))
    result = require_ok(
        worker.call(
            "cleanup",
            {
                "session_id": "llm-test",
                "text": "hello hello",
                "tokens": [
                    {"text": "hello", "probability": 0.9},
                    {"text": " hello", "probability": 0.2},
                ],
            },
        )
    )
    assert result["original_text"] == "hello hello"
    assert isinstance(result["text"], str)
    assert isinstance(result["decisions"], list)

    ranked = require_ok(
        worker.call(
            "rank_candidates",
            {
                "session_id": "llm-test",
                "left_context": "we use ",
                "right_context": " today.",
                "candidates": [
                    {"id": "supported", "text": "tools"},
                    {"id": "repetition", "text": "use"},
                ],
                "propose_normalizations": True,
            },
        )
    )
    assert [item["id"] for item in ranked["rankings"]] == ["supported", "repetition"]
    assert all(item["token_count"] > 0 for item in ranked["rankings"])
    assert all(isinstance(item["mean_log_probability"], float) for item in ranked["rankings"])
    assert ranked["normalization"] == {
        "candidate_id": "supported",
        "proposals": [],
    }
    assert "text" not in ranked

    duplicate_ids = worker.call(
        "rank_candidates",
        {
            "session_id": "llm-test",
            "candidates": [
                {"id": "same", "text": "one"},
                {"id": "same", "text": "two"},
            ],
        },
    )
    assert duplicate_ids["ok"] is False
    assert duplicate_ids["error"]["code"] == "invalid_request"

    normalization_via_cleanup = worker.call(
        "cleanup",
        {
            "session_id": "llm-test",
            "text": "pie torch",
            "candidates": [
                {
                    "start_byte": 0,
                    "end_byte": 9,
                    "replacement": "PyTorch",
                    "kind": "canonical_name",
                }
            ],
        },
    )
    assert normalization_via_cleanup["ok"] is False
    assert normalization_via_cleanup["error"]["code"] == "invalid_request"
    require_ok(worker.call("end_session", {"session_id": "llm-test"}))
    worker.close()


def main() -> None:
    if len(sys.argv) != 3:
        raise SystemExit("usage: worker_protocol_test.py ASR_WORKER LLM_WORKER")
    check_asr(Path(sys.argv[1]).resolve())
    check_llm(Path(sys.argv[2]).resolve())


if __name__ == "__main__":
    main()
