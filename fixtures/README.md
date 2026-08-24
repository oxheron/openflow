# Shared fixtures

These fixtures describe security and correctness invariants shared across
language implementations. They deliberately contain no recorded audio or real
user transcripts.

`confidence-gates.json` encodes the initial cleanup policy:

- formatting edits are allowed independently of lexical confidence;
- lexical edits at or above `0.75` source confidence are blocked;
- lexical edits from `0.35` through `0.75` require a score advantage of at
  least `0.5` nats per source token;
- lexical edits below `0.35` require a non-negative score advantage; and
- protected content is never changed.

Implementations may load this file in integration tests or mirror the cases in
native unit tests. A policy change must update both the fixture version and the
security documentation.

