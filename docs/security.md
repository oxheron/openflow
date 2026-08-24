# Security model

OpenFlow handles microphone audio and can modify text in other applications.
Those are separate capabilities and remain separated in the design.

## Desktop safety

The client verifies the focused editable control and expected caret/range before
each replacement. A changed verified target cancels the session. Protected
fields and terminals use overlay-and-clipboard delivery. Wayland targets that
cannot be verified through AT-SPI also use overlay-and-clipboard delivery; the
RemoteDesktop portal is not used as an unverified typing fallback.

The macOS adapter uses AXUIElement semantic selected-text operations; the Linux
adapter uses AT-SPI Text and EditableText. Both retain the exact native element,
full field value, selection, and OpenFlow-owned range and verify them before and
after every mutation. Direct typing is never enabled merely because synthetic
input is available.

These checks reduce misdelivery; they cannot make cross-application editing
transactional. An accessibility provider or target application can fail after a
range was selected or deleted but before replacement finishes. OpenFlow then
invalidates the lease, stops direct writes, and preserves the complete transcript
in its overlay/clipboard fallback, but the original field may require manual
repair. Accessibility permission also allows the OS API to expose text from
other applications, so it should be granted only to a trusted OpenFlow build.

### Portal and synthetic-input risks

- Wayland GlobalShortcuts is used only to register the start/stop trigger. It
  requires compositor/portal consent and can be unavailable or revoked; the tray
  action remains available. It never grants or performs text insertion.
- RemoteDesktop/InputCapture-style synthetic input can race focus changes and
  type into another window, a password field, a terminal, or a privileged UI.
- Key synthesis depends on keyboard layout, dead keys, IMEs, and compositor
  behavior, so Unicode text and corrections can be corrupted.
- Accessibility APIs provide the semantic verification OpenFlow needs, but
  their broad permissions can expose focused text and must be requested and
  handled conservatively.
- Clipboard fallback can overwrite user clipboard contents or expose text to a
  clipboard manager. OpenFlow copies only on an explicit fallback path and
  never pastes automatically into an unverified target.

## Server exposure

The server binds to loopback by default. A non-loopback bind is rejected unless
TLS is configured. Pairing codes are random, single-use, and short lived;
enrolled device credentials are independently revocable and stored as hashes.
Administrative and dictation capabilities are checked separately.

A paired-device credential can dictate and manage the shared model cache. That
is required for the remote desktop model library, but it also delegates server
disk, download-bandwidth, deletion, and activation decisions. Pair only trusted
clients and revoke lost devices. Pairing-code issuance and device revocation are
administrator-only operations.

The optional foreground pairing endpoint is disabled unless
`OPENFLOW_INTERACTIVE_PAIRING` is set and the process has a terminal. It validates
the printable device name and six-digit comparison code before displaying them,
allows only one pending prompt, and enrolls a client only after an explicit
`y`/`yes` response. The comparison code is not an authentication secret: its
purpose is to bind the request visible on the client to the terminal prompt.
Do not approve an unexpected request or a mismatched name/code. Background
services should leave the feature disabled and use administrator-created,
single-use pairing codes instead.

Tailscale Serve is the recommended remote path because it can terminate HTTPS
for a loopback service inside a tailnet. Application authentication is still
required. Direct port forwarding must use the built-in TLS listener or a
trusted reverse proxy and is never configured automatically.

WebSocket authentication uses a credential-bearing subprotocol header instead of
a query parameter. Reverse proxies and diagnostic middleware must redact the
`Sec-WebSocket-Protocol` request header; TLS protects it in transit but cannot
prevent logging at a terminating proxy.

## Data retention

Audio frames and transcript data are bounded in memory and cleared when a
session ends, is cancelled, or disconnects. Logs contain identifiers, timings,
sizes, and error categories—not audio or transcript bodies. Model files and
model metadata are the only expected durable inference data.

## Desktop credentials

Server credentials are scoped to the normalized endpoint and stored in macOS
Keychain or Linux Secret Service. They are held in renderer memory only while
the application is running and are excluded from webview local storage. A
legacy local-storage credential is migrated once and removed. Remote plaintext
HTTP, URL-embedded credentials, query strings, and fragments are rejected before
pairing or authentication; loopback HTTP remains available for the bundled
local service.

OS credential storage protects secrets at rest, not against a compromised user
session, desktop process, accessibility client, or unlocked keychain. Revoke the
paired device on the server after suspected compromise.
