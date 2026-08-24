# OpenFlow

OpenFlow is an open-source, local-first dictation system for macOS and Linux.
It produces stable speech-to-text partials for the active workflow, then uses a
small local language model and Whisper confidence evidence to conservatively
polish each completed sentence. The desktop client can insert text through
verified macOS Accessibility and Linux AT-SPI targets, and fails closed to its
overlay and explicit clipboard fallback when a target cannot be proven safe.

The inference engine is deliberately separate from the desktop client. The
default installation runs both on one computer; an advanced installation can
run the models on a GPU workstation and connect one or more paired clients over
a private network such as Tailscale.

> **Project status:** feature-complete development release. The repository has
> production inference adapters, verified direct typing, authenticated remote
> mode, model management, and macOS/Linux installer automation. The mock
> inference backend remains for tests. Treat the project as beta-quality until
> the release artifacts and real model/hardware combinations in the deployment
> matrix have been exercised on their target operating systems.

## Repository layout

```text
apps/desktop/       Tauri tray client, overlay, hotkeys, and safe text insertion
apps/server/        Headless inference/model server and administration API
crates/protocol/    Shared versioned network and worker contracts
native/             Workers, confidence policy, whisper.cpp, and llama.cpp adapters
docs/               Architecture, security, and deployment documentation
```

The client never delegates keyboard or clipboard privileges to the server.
Remote servers receive microphone audio and return versioned transcript events
only.

## Development

Prerequisites are Rust 1.88+, Node.js 22+, CMake 3.20+, and a C++17 compiler.
Linux desktop builds additionally need the normal Tauri/WebKitGTK development
packages. Model runtimes are optional for the default test build.

```bash
cargo test --locked --workspace
cmake -S native -B native/build -DOPENFLOW_BUILD_TESTS=ON
cmake --build native/build
ctest --test-dir native/build --output-on-failure
npm --prefix apps/desktop ci
npm --prefix apps/desktop test
npm --prefix apps/desktop run build
```

For distributable packages with bundled inference workers, use
`scripts/build-installers.sh`; see the deployment guide for CPU, Metal, Vulkan,
CUDA, and ROCm profiles. Arch Linux users can build a native ROCm application
bundle with `scripts/build-arch-bundle.sh --profile rocm`. Linux releases also
include a standalone model-server archive for the advanced remote-host setup;
its `openflow-host` launcher provides one-command foreground startup and
terminal-approved client pairing.

See [the architecture guide](docs/architecture.md),
[deployment guide](docs/deployment.md), and [security model](docs/security.md)
before changing protocol, storage, pairing, or text-injection behavior.

## Privacy defaults

- No audio or transcript history.
- No network access after selected models have been downloaded, unless the
  user explicitly connects to a remote OpenFlow server.
- No unverified key injection on Wayland.
- No plaintext non-loopback server listener.
- Transcript content is excluded from normal logs.

## License

OpenFlow source is licensed under Apache-2.0. Downloadable model weights retain
their own licenses, which are shown before download and recorded in the model
registry.
