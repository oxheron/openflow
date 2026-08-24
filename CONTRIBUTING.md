# Contributing

Keep the desktop privilege boundary, server/model boundary, and native worker
boundary explicit. New protocol fields must be backward-compatible within a
major protocol version and covered by serialization tests.

Before submitting a change, run the Rust, native, and desktop checks documented
in the README. Never add real transcripts, microphone recordings, model
weights, pairing tokens, certificates, or private endpoints as fixtures.

Keep executable test suites in the component's `tests/` directory. Production
Rust modules must not contain inline `#[cfg(test)]` modules, and frontend tests
must not be colocated under `src/`. Run `scripts/verify-test-layout.sh` to check
this repository invariant.

Use conventional, focused commits. Document security-relevant behavior changes
in `docs/security.md` and include a regression test for every bug involving a
target range, authentication decision, model checksum, or stale revision.
