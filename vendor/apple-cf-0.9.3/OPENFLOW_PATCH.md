# OpenFlow compatibility patch

This directory contains the published `apple-cf` 0.9.3 crate under its
original MIT OR Apache-2.0 license.

OpenFlow replaces the trailing-closure form of `DispatchQueue.asyncAndWait`
with an equivalent explicit `DispatchWorkItem`. The Swift 5.9 compiler accepts
the package manifest, while this call shape also compiles against the Dispatch
overlay shipped with Xcode 14.3. This lets storage-constrained Intel Macs use a
standalone Swift 5.9 toolchain without installing a second full Xcode.

The build script also reads the active compiler's `runtimeLibraryPaths` and
passes them to Rust's final link. Upstream only searches the selected Xcode
installation, which cannot resolve objects compiled by a newer standalone
Swift toolchain.
