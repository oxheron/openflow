# OpenFlow desktop client

The desktop process owns microphone capture, global shortcuts, the tray UI, and all text-target access. The inference server receives audio and returns versioned transcript events; it never controls the keyboard or clipboard.

## Development

```sh
npm install
npm run test
npm run tauri dev
```

`cargo test` and `npm run build` do not require release sidecars. The ordinary
Tauri configuration intentionally has no `externalBin` entries; release builds
merge `packaging/tauri.release.conf.json` only after the executables exist.

Use `npm run dev` for a browser-only UI preview; the browser cannot use native
text-target APIs and therefore uses the overlay and clipboard. The native app
supports semantic direct insertion through macOS Accessibility and Linux
AT-SPI. It captures only focused, editable, non-secure text controls, retains
the control identity, and re-verifies focus, complete value, and caret before
each mutation. It also checks the exact value and caret afterward. A permission
failure, unsupported control, external edit, or focus change invalidates the
lease and falls back to the overlay and clipboard instead of synthesizing keys.
The native overlay is a separate always-on-top, click-through window. On Wayland,
the start/stop shortcut uses the XDG GlobalShortcuts portal; macOS and X11 use
the native global-shortcut backend. The portal is never used for typing.

The local default starts an `openflow-server` binary installed beside the
desktop executable, rotates and captures its per-launch bootstrap credential,
and connects at `http://127.0.0.1:8765`. During development, build both Rust
binaries into the same target directory. If no sibling is installed, the
client can connect to an independently managed loopback service instead. Remote
endpoints should use HTTPS/WSS with a paired device token. The client sends
that token as the `openflow.bearer.<token>` WebSocket subprotocol so it never
appears in a URL.

For a foreground model host, the remote connection page can send a public
`POST /v1/pair/interactive` request containing a printable device name and a
client-generated six-digit comparison code. The host operator must approve the
matching prompt before the returned revocable device token is stored in the OS
credential service. Administrator-created single-use codes remain available
for background servers.

## Server interface

- `GET /v1/capabilities` and `GET /v1/models` use the shared Rust protocol's snake_case JSON.
- Model mutations use `POST /v1/models/download`, `POST /v1/models/cancel`,
  `POST /v1/models/activate`, `POST /v1/models/deactivate`, and
  `DELETE /v1/models/:id`.
- `WS /v1/dictation` uses `{ "type": ..., "payload": ... }` envelopes. Binary messages are little-endian, 16 kHz, mono PCM16 frames.
- The client sends `commit` after confirmed speech followed by about 900 ms of silence. This detector never drops audio.

REST requests use Tauri's scoped HTTP plugin so loopback development is not blocked by webview CORS. The scope permits loopback HTTP and remote HTTPS only. The low-latency audio stream uses the webview WebSocket API; the content-security policy allows loopback WS and remote WSS, while the server rejects plaintext non-loopback binds.

Non-secret settings persist in local storage. Server credentials are scoped by
endpoint and stored in macOS Keychain or Linux Secret Service; a legacy
local-storage token is migrated and removed on first native launch. The UI marks
remote configuration as advanced and never writes audio or transcript history.

## Installer build

From the repository root:

```sh
./scripts/build-installers.sh
```

On macOS this builds target-native Metal inference workers, an application ZIP,
and a DMG. On Linux it builds target-native Vulkan inference workers plus an
AppImage, Debian package, RPM, and standalone headless-server archive. The installed `openflow-server` and both
native workers are Tauri sidecars beside the desktop executable, so local mode
does not require a separate service installation. Model weights remain
on-demand downloads.

On Arch Linux with an AMD GPU,
`scripts/build-arch-bundle.sh --profile rocm` creates a portable native bundle
whose workers are compiled for HIP/ROCm. It uses the same client and model cache
semantics as the standard installer.

The build fetches reviewed upstream revisions into `target/release-upstream`.
Use `OPENFLOW_WHISPER_CPP_DIR` and `OPENFLOW_LLAMA_CPP_DIR` to supply audited
local checkouts instead. See `docs/deployment.md` for system dependencies,
profile selection, artifact names, signing, notarization, and CI releases.
