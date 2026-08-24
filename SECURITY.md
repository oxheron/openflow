# Security

Please report vulnerabilities through the repository host's private security
advisory mechanism rather than opening a public issue. Include the affected
version, platform, reproduction steps, and whether microphone data, credentials,
model files, or another application's text could be exposed or modified.

The most sensitive areas are target-range verification, credential handling,
WebSocket authentication, non-loopback listener configuration, archive/model
download validation, and logging. Do not include real credentials, recordings,
or private transcripts in a report; use synthetic data.

Until the project publishes a stable release, only the current main branch is
expected to receive security fixes.

