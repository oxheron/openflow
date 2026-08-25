# OpenFlow native inference workers

This directory contains the native model boundary. `openflow-asr-worker` owns speech
recognition state and `openflow-llm-worker` owns candidate scoring and constrained surface
normalization. Each is a persistent subprocess, so a model is loaded once and reused across
dictation sessions. Desktop input handling and network access intentionally do not live
in these processes.

The default build has deterministic mock adapters and no downloaded dependencies. The
mocks exercise process supervision and the complete wire/policy path in CI; they are not
presented as useful inference models. See [PROTOCOL.md](PROTOCOL.md) for the wire contract.

## Build

```sh
cmake -S native -B build/native -DCMAKE_BUILD_TYPE=Release
cmake --build build/native --parallel
ctest --test-dir build/native --output-on-failure
```

Optional model adapters are explicitly enabled against local source checkouts:

```sh
cmake -S native -B build/native-whisper \
  -DOPENFLOW_ENABLE_WHISPER_CPP=ON \
  -DOPENFLOW_WHISPER_CPP_DIR=/path/to/whisper.cpp

cmake -S native -B build/native-llama \
  -DOPENFLOW_ENABLE_LLAMA_CPP=ON \
  -DOPENFLOW_LLAMA_CPP_DIR=/path/to/llama.cpp
```

The production fetch script applies
`packaging/patches/whisper.cpp-v1.9.1-nbest.patch` to the pinned whisper.cpp v1.9.1
checkout. A manually supplied `OPENFLOW_WHISPER_CPP_DIR` must use that pinned revision
with the same patch applied; CMake rejects an unpatched checkout so release builds cannot
silently lose N-best beam hypotheses.

Backend acceleration flags such as `GGML_CUDA=ON`, `GGML_HIP=ON`,
`GGML_VULKAN=ON`, or `GGML_METAL=ON` pass through to the embedded upstream
CMake project. For ROCm, optionally set `GPU_TARGETS` to the exact AMDGPU ISA
(for example `gfx1102`) so the workers do not carry kernels for unrelated GPUs.
Build the adapters in separate CMake directories, as shown above. Each pinned
upstream carries its own ggml revision, so combining them in one CMake graph can
bind one adapter to the other's incompatible headers. The release staging
script performs these isolated builds automatically. OpenFlow does not fetch
either dependency during configuration.

## Design notes

- The shared library has no model dependencies. It owns strict JSON parsing, bounded
  framing, the worker loop, token-to-word probability aggregation, protected-text
  recognition, and edit policy.
- ASR accepts compact base64 PCM S16LE or compatibility-mode normalized floating-point
  samples at 16 kHz mono. The orchestrating server is responsible for decoding Opus and
  resampling before invoking the worker.
- The primary LLM API scores only caller-supplied ASR candidates. Optional surface
  normalization emits bounded, exact-span, grammar-constrained proposals which the worker
  never applies. The server owns cross-pass acoustic weighting and independent grounding.
  The older `cleanup` command remains protocol-compatible, but production dictation no
  longer routes raw transcripts through that freeform lexical proposal path.
- llama.cpp keeps one 4096-token context with a 2048-token prompt batch alive for the
  loaded model and clears its memory between independent scores/generations. Safe
  formatting and exact-duplicate candidates bypass scoring because their gates do not
  use likelihood; lexical candidates retain full original-versus-proposal scoring.
- Text offsets are UTF-8 byte offsets. Invalid code-point boundaries, stale/out-of-range
  edits, overlapping edits, and result length changes over 25% are rejected.
