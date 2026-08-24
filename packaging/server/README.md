# OpenFlow headless server bundle

This archive is the model-hosting half of OpenFlow. It contains the server and
both native inference workers. Desktop clients retain microphone, hotkey,
clipboard, and accessibility privileges; this server has no keyboard API.

For an interactive, unprivileged host, run `bin/openflow-host` in a foreground
terminal. It creates a private administrator credential and durable user cache,
then lets clients request access with a device name and six-digit comparison
code. The four executables must remain together.

For a customized foreground host, copy `openflow-server.env.example`, adjust its
data paths to directories you own, export those variables, and run
`bin/openflow-server` directly.

For a systemd installation:

1. Create a locked `openflow` service account and `/var/lib/openflow` owned by it.
2. Copy this directory to `/opt/openflow-server`.
3. Copy the environment example to `/etc/openflow/openflow-server.env`, owned by
   root with mode `0600`, and review every listener/TLS setting.
4. Copy `openflow-server.service` to `/etc/systemd/system/`, then enable it.
5. Read the bootstrap JSON from the service journal once, store its token in a
   password manager, and enroll clients with short-lived pairing codes.

The default loopback listener is appropriate behind Tailscale Serve. A direct
non-loopback listener is refused unless both a TLS certificate and private key
are configured. Model weights are downloaded only after selection by an
authenticated desktop client and are verified against pinned size and SHA-256.

The CPU archive works without a GPU. Vulkan, CUDA, and ROCm archives require the
matching system driver and runtime. ROCm additionally requires access to
`/dev/kfd` and the GPU render node. No model weights are included.
