# Native worker protocol v1

## Transport

Workers take no command-line arguments. They read requests from standard input, write
responses to standard output, and reserve standard error for diagnostics. Every message
is one frame:

1. four-byte unsigned payload length in network byte order (big endian);
2. exactly that many bytes of UTF-8 JSON.

Frames are limited to 16 MiB. EOF cleanly terminates a worker. A truncated/oversized
frame is a fatal transport error; a valid frame containing a bad request receives a
structured error and the worker continues.

Requests have the form:

```json
{"id":"caller-defined","command":"ping","params":{}}
```

`id` is copied unchanged into the response. `params` may be omitted and defaults to an
empty object. Responses are exactly one of:

```json
{"id":"caller-defined","ok":true,"result":{}}
{"id":"caller-defined","ok":false,"error":{"code":"invalid_request","message":"..."}}
```

The error code is `invalid_request` for malformed JSON fields/unknown commands and
`worker_error` for runtime/model failures. Common commands are `ping` and `shutdown`.

## ASR worker

Binary: `openflow-asr-worker`.

- `list_backends {}` → `{"backends":["mock", ...],"compute_backends":["cpu", ...]}`.
  Compiled compute names are `cpu`, `cuda`, `rocm`, `metal`, and `vulkan`.
- `load_model {"backend":"auto|mock|whisper.cpp","model_path":"..."}` loads and
  atomically replaces the active backend; existing sessions end. `auto` uses
  whisper.cpp for a nonempty path when compiled, otherwise mock.
- `unload_model {}` releases the model and sessions.
- `start_session {"session_id":"...","language":"auto","initial_prompt":"..."}`.
- `end_session {"session_id":"..."}`.
- `transcribe {"session_id":"...","samples_s16le_base64":"...","final":false}` where
  the string is canonical standard base64 containing complete 16 kHz mono PCM S16LE
  samples. For protocol compatibility, callers may instead supply `samples` as normalized,
  finite f32 values, but never both encodings. Result fields are `session_id`,
  `final`, `text`, detected `language`, flat `tokens`, and timestamped `segments`.
  Tokens are `{"text":"...","probability":0.0}`; segment times are milliseconds.

For deterministic tests, mock transcription additionally accepts `mock_text` and
`mock_probabilities`. These fields have no effect on whisper.cpp.

## LLM worker

Binary: `openflow-llm-worker`.

- `list_backends {}` → `{"backends":["mock", ...],"compute_backends":["cpu", ...]}`.
  Compiled compute names are `cpu`, `cuda`, `rocm`, `metal`, and `vulkan`.
- `load_model {"backend":"auto|mock|llama.cpp","model_path":"..."}` and
  `unload_model {}` behave like the ASR equivalents.
- `start_session {"session_id":"...","context":"..."}` and `end_session` manage
  caller ownership. Version 1 reserves `context` but scores the explicit supplied text.
- `score {"session_id":"...","text":"..."}` → total/mean log probability, token
  count, and per-token log probabilities.
- `propose_edits {"session_id":"...","text":"..."}` emits deterministic safe
  formatting/exact adjacent-duplicate candidates plus llama.cpp-generated lexical
  candidates. Generation is greedy, non-thinking, capped at 512 tokens/eight edits, and
  grammar-constrained to JSON. Every generated source string must exactly match its UTF-8
  byte range before the proposal reaches confidence gating. The mock backend deliberately
  generates no lexical edits.
- `cleanup` scores and gates candidates, then returns corrected `text`, `original_text`,
  and a decision record for every candidate.

The llama.cpp adapter keeps a reusable 4096-token context per loaded model. Prompts are
limited to 2048 tokens and must leave room for the 512-token generation cap; oversized
cleanup requests fail explicitly instead of reallocating enough context to exhaust a
consumer GPU. Context memory is cleared between independent operations, so text from one
dictation session cannot condition another.

`cleanup` accepts:

```json
{
  "session_id": "s1",
  "text": "raw transcript",
  "tokens": [
    {"text":"raw","probability":0.42},
    {"text":" transcript","probability":0.88}
  ],
  "candidates": [
    {"start_byte":0,"end_byte":3,"replacement":"Raw","kind":"formatting"}
  ],
  "protected_ranges": [{"start_byte":4,"end_byte":14}]
}
```

`tokens` are aggregated into word confidence with the geometric mean (equivalently,
mean log probability). Callers may instead provide `words` with `text`, `start_byte`,
`end_byte`, `confidence`, and optional `protected`. With neither, lexical content is
conservatively treated as confidence 1. Candidate kinds are `formatting`,
`adjacent_duplicate`, and `lexical`. If `candidates` is omitted, the safe deterministic
proposals are used.

An optional `policy` object may override `high_confidence` (0.75), `low_confidence`
(0.35), `medium_min_advantage_nats` (0.5), `low_min_advantage_nats` (0.0), and
`maximum_result_length_change_ratio` (0.25). Defaults are the stable application policy;
production callers should not alter them per utterance.
