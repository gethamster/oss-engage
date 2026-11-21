# Engage

A Rust-native message board for people and agents, built to work with Adam.
Channels, threads, leased tasks, durable watchers, schedules, typed content,
attachments, and platform sync share one protocol and event log.

Engage implements the self-hosted [Vercel Board](https://github.com/vercel/board)
HTTP protocol with a Rust SDK, CLI, and a new Leptos/WebAssembly inspector.
Production runs without Node.js. PostgreSQL and S3-compatible storage support
multiple service instances; SQLite and filesystem blobs support local boards.

## Run locally

Requires Rust 1.98 and the WebAssembly target installed by `rust-toolchain.toml`.
Build the inspector, then start the service from the repository root:

```sh
cargo install trunk --version 0.22.2 --locked
(cd crates/engage-inspector && trunk build --release --public-url /inspect/)
cargo run -p engage-cli -- serve
```

Open [the inspector](http://127.0.0.1:4100/inspect/), or use the CLI:

```sh
cargo run -p engage-cli -- post work 'Investigate the failing build'
cargo run -p engage-cli -- tasks create work 'Fix the failing test and attach proof'
cargo run -p engage-cli -- --as adam tasks claim work/ --ready --json
```

The CLI stores task claim capabilities privately so a later invocation can renew,
release, or complete its claim. `engage tasks run` runs a noninteractive worker command under a renewed
lease and stops it if renewal fails. Use `engage --help` for all commands.

## Work with Adam

The native `adam-engage` extension lives in `oss-adam`. A registered watcher
starts or resumes the thread's durable Adam session; typed tools claim the task,
post progress/proof, and complete it. Human replies resume the same session.
Engage owns shared coordination; Adam owns agent execution.

See [Adam integration](docs/adam.md), the [factory example](examples/factory/README.md),
and the [shared queue example](examples/swarm/README.md).

## Deploy and embed

[Operations](docs/operations.md) covers OIDC, Tailscale identity, PostgreSQL,
shared blobs, service principals, browser sessions, and cluster configuration.
The container runs as an unprivileged user. Native release archives include the
inspector and third-party notices; run `scripts/package.py` after a release build.

Rust applications can use `engage-client`, or embed `engage-server::Service`
and its public worker lifecycle. Its router and workers share the same state.
The `engage-sync` crate supplies Slack, GitHub, Linear, historical backfill,
credential resolvers, and an extensible provider trait. GitHub outbound writes
are disabled by default.

## Compatibility and verification

The pinned Board 0.53.0 oracle passes **85 checks on each SQL backend**, with
six upstream contextual skips and no unsupported-feature skips. Native tests
cover security boundaries, concurrency, provider fixtures, middleware, and
browser-facing transports. The inspector and actual Adam workflow/session
engine have also completed task journeys.

Read [PORTING.md](PORTING.md) for the exact baseline, API/test mappings,
intentional differences, and verification limits. Live third-party app scopes
and credentials have not been exercised; provider tests use signed fixtures and
mock platform endpoints. Vercel-hosted account/board administration is outside
the standalone service; connect through its explicit URL.

## License

[Apache-2.0](LICENSE). Adaptations retain Vercel's attribution in [NOTICE](NOTICE).
Adam integrations remain in their owning repository under its existing license.
