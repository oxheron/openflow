# Architecture

OpenFlow is split at a privilege boundary rather than just a process boundary.
The desktop client owns microphone access, the global shortcut, target
verification, clipboard access, and text insertion. The inference server owns
model files, accelerator resources, decoding, and cleanup. Native inference
libraries live in separate worker processes so failures or incompatible GGML
versions cannot corrupt the desktop process.

## Data flow

```text
microphone -> desktop session -> ordered audio frames -> inference server
                                                      -> ASR worker
                                                      -> cleanup worker
desktop target <- versioned correction patch <-------+
```

The server publishes stable raw transcript prefixes during speech. At a voice
boundary it publishes a final raw segment followed by a correction patch whose
base revision identifies exactly which client-side range may be replaced. The
client rejects stale patches. Desktop events pass through one ordered async
dispatcher, so correction hash verification and native writes complete before a
following session-stopped event can release the captured target.

Socket reads, inference, and socket writes run as separate bounded stages. The
server accepts at most 512 pending messages and 2 MiB of queued audio per
connection; a producer that outruns inference receives a retryable overload
error instead of growing server memory without bound. Cancelling a decode never
abandons an unread native-worker frame: a bounded worker actor drains it before
processing session cleanup, and restarts a crashed or timed-out worker before
one safe retry. Audio accumulated while a partial decode is running is coalesced
into the next rolling request, avoiding a decode backlog for every queued frame.
Server-to-worker audio uses base64 PCM S16LE rather than JSON float arrays, keeping a
60-second segment well below the 16 MiB frame limit with substantially less parsing.
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
