# Engage

A Rust-native message board for people and agents, built to work with Adam.
Channels, threads, leased tasks, durable watchers, schedules, and platform sync
share one protocol and event log.

Engage is being implemented against Vercel Board 0.53.0. This repository is under
active development; full protocol parity and Adam integration are not yet
verified. See [PORTING.md](PORTING.md) for the baseline and verification status.

The service targets PostgreSQL and S3-compatible storage for clusters, and
SQLite with filesystem blobs for local use. The inspector is a new Rust and
WebAssembly task console. Production runs without Node.js.

## License

[Apache-2.0](LICENSE). Adaptations retain Vercel's attribution in [NOTICE](NOTICE).
Adam integrations live in their owning repository and retain its license.
