# OpenFlow on Arch Linux

This directory is a native, relocatable OpenFlow build for Arch Linux. Keep
`openflow-desktop`, `openflow-server`, and both worker binaries together; the
desktop discovers and starts its sibling server automatically.

Run it without installing system files:

```sh
./openflow-desktop
```

ROCm builds require a working AMD kernel driver, `/dev/kfd`, and the Arch
`hip-runtime-amd`, `rocblas`, and `hipblas` runtime packages. The user must be
able to open `/dev/kfd` and the relevant `/dev/dri/renderD*` node. Log out and
back in after changing `video`/`render` group membership. Confirm the host sees
the GPU with `rocminfo` before starting OpenFlow.

This build reports `rocm` in the server capabilities only when both inference
workers contain HIP support and `/dev/kfd` is available. Model loading then
uses the ROCm backend automatically. If ROCm initialization fails, use the
Vulkan bundle built with `scripts/build-arch-bundle.sh --profile vulkan`.

The desktop also needs an unlocked Secret Service provider (for example GNOME
Keyring or KWallet) to persist server credentials. If none is available, the
local bootstrap credential remains usable only for the current process.
Wayland hotkeys additionally require `xdg-desktop-portal` and the backend for
the active compositor; if the GlobalShortcuts portal is unavailable, use the
tray control. Verified AT-SPI insertion still fails closed to overlay/clipboard.

For an Arch model host serving a Mac client, keep the server in the foreground
and publish its loopback listener with Tailscale Serve. From this directory:

```sh
./openflow-host
```

`openflow-host` privately creates and reuses the administrator credential, model
cache, and device store. In another terminal, install `tailscale`, enable
`tailscaled`, and run `tailscale serve --bg http://127.0.0.1:8765`. On the Mac,
enter the HTTPS URL reported by `tailscale serve status` and choose **Request
server approval**. Confirm that the device name and six-digit code match, then
answer `y` in the Arch terminal. The Mac receives a revocable credential and
stays paired across restarts. The repository deployment guide contains the
background-service fallback and complete security guidance.
