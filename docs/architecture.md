# Architecture

OpenFlow is split at a privilege boundary rather than just a process boundary.
The desktop client owns microphone access, the global shortcut, target
verification, clipboard access, and text insertion. The inference server owns
model files, accelerator resources, decoding, and cleanup. Native inference
libraries live in separate worker processes so failures or incompatible GGML
versions cannot corrupt the desktop process.

## Data flow

```text
microphone -> desktop VAD -> ordered PCM -> 25 s rolling server window
                                             |
                                             v every 2 s
                                      Whisper N-best (up to 3)
                                             |
                              3-pass / 6 s-tail consensus
                                  | converged      | mature disagreement
                                  v                v
                           immutable prefix   constrained LLM scorer
                                  |                |
                                  +------ selected ASR wording
                                                   |
desktop target <- versioned final/correction <- bounded surface normalization
```

The desktop sends 16 kHz mono PCM continuously and uses VAD only to request a
segment boundary; it never discards speech. The default VAD boundary is 700 ms
of silence after at least 160 ms of speech. Independently, the server starts a
new partial decode for each 2 seconds of newly received audio. Each decode
retranscribes only the latest 25 seconds, so consecutive requests are sliding,
23-second-overlapping windows rather than disjoint chunks.

The Whisper adapter uses beam search and exposes up to three unique full-window
hypotheses, with token probabilities and length-normalized log probability. A
rolling consensus tracker retains three consecutive passes. Words must agree
lexically across all three passes, align within 1.5 seconds, and be older than a
6-second unstable tail before they become an immutable prefix. Capitalization
and edge punctuation do not prevent lexical agreement. The latest 512 bytes of
that committed prefix, plus the session glossary, are passed back as optional
Whisper prompt context. At a VAD or explicit commit boundary, the strongest
remaining ASR-supported path is finalized rather than waiting for another
rolling pass.

The LLM is not a freeform transcript editor. Converged rolling spans do not use
it for wording selection. A mature disagreement is sent as a finite list of
Whisper-supported candidates with at most 1,024 bytes of preceding committed
context. Candidate language likelihood is the length-normalized score of the
candidate plus its effect on right-context likelihood. The server combines
that with cross-pass support, N-best support/rank, and ASR probabilities; pass
support is weighted much more heavily, and the normalized LLM contribution is
capped at 0.75. A candidate still needs support from at least two ASR passes and
a minimum combined-score margin before it can be committed.

At a voice boundary, a separate constrained surface-normalization pass may
return up to eight exact-span edits for punctuation, capitalization, word
boundaries, spoken symbols, orthography, or canonical names. The worker never
applies these proposals. The server revalidates UTF-8 ranges, overlap, edit
size, grounding type, and lexical equivalence. Lexical changes must be a known
spoken alias (for example, `pie torch` -> `PyTorch`, `get hub` -> `GitHub`, or
`see plus plus` -> `C++`) or a bounded sound-oriented match to an exact
session-glossary entry. Unsupported changes fail closed. The server publishes the final raw ASR
segment followed, when needed, by a versioned correction patch whose base
revision and raw-text hash identify exactly which client-side range may be
replaced. The client rejects stale patches. Desktop events pass through one
ordered async dispatcher, so correction hash verification and native writes
complete before a following session-stopped event can release the captured
target.

Socket reads, inference, and socket writes run as separate bounded stages. The
server accepts at most 4,096 pending messages and 2 MiB of queued audio per
connection; a producer that outruns inference receives a retryable overload
error instead of growing server memory without bound. Cancelling a decode never
abandons an unread native-worker frame: a bounded worker actor drains it before
processing session cleanup, and restarts a crashed or timed-out worker before
one safe retry. Audio accumulated while a partial decode is running is coalesced
into the next rolling request, avoiding a decode backlog for every queued frame.
Server-to-worker audio uses base64 PCM S16LE rather than JSON float arrays, keeping a
25-second rolling window well below the 16 MiB frame limit with substantially less parsing.
The desktop independently caps browser WebSocket buffering at 1 MiB. Model
activation and session start load the selected worker models before
`session_started`, and microphone capture begins only after that acknowledgement;
multi-gigabyte cold loads therefore cannot consume the audio queue.

Local mode uses the same API as remote mode on `127.0.0.1:8765` and requires an
administrator or paired-device credential. Remote mode requires authenticated
TLS.

The native worker handshake reports both model adapters and compiled compute
backends. Server hardware capabilities are intersected with that handshake, so
a CPU build does not advertise or recommend against GPU memory merely because a
GPU happens to be installed.

## Dependency direction

`protocol` contains data-only contracts and has no dependency on applications.
The inference policy may depend on protocol types. Server and desktop depend on
both, while native workers communicate through their own framed protocol and do
not link into either application.

## State ownership

- Desktop: current target anchor, expected caret/range, UI state, hotkey, and
  short-lived audio capture buffers. The visible dictation overlay is a separate
  click-through, always-on-top webview rather than content in the settings window.
- Server: model registry/cache, paired-device hashes, hardware capabilities,
  and the single active inference lease.
- Workers: loaded model and active decoder context only.

No layer stores completed dictation text.
