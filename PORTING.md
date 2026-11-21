# Board porting record

## Baseline

Vercel Board `fef19e5bb1e17551f119be11f7af8ed3387892bb`, package 0.53.0.
https://github.com/vercel/board/tree/fef19e5bb1e17551f119be11f7af8ed3387892bb

Source implementation, api/routes.ts, and executable tests take precedence over
SPEC.md, which is a draft. Upstream is Apache-2.0, Copyright 2026 Vercel Inc.
Upstream has no NOTICE file. Engage adaptations are modified Rust implementations.

## API and test mapping

| Upstream capability | Engage owner | Verification |
| --- | --- | --- |
| Contract, queries, access, messages, tasks | engage-core, engage-store | Rust regressions and upstream conformance |
| HTTP routes and local backend | engage-server | HTTP regressions and conformance |
| Handle and CLI | engage-client, engage-cli | SDK and command integration tests |
| Sync providers | engage-sync | Signed webhook and platform fixtures |
| Inspector | engage-inspector | New Leptos implementation and browser journeys |
| Factory and swarm | examples and agent skill | End-to-end local journeys |
| Adam channel and tools | oss-adam | Native fixture and session replay |

## Intentional differences

- PostgreSQL and shared S3-compatible blobs support multiple server instances.
- Local schedules and processing retries survive restart; missed repeat slots
  use the next future occurrence rather than replaying downtime.
- A new Leptos/WebAssembly inspector replaces the upstream React inspector.
- Self-hosted credentials use standard OIDC and injected provider resolvers.
  Vercel Connect and deployment wake routing are optional platform integrations.

## Verification status

Bootstrap only. No conformance group or Adam journey is verified yet. A passing
report with unsupported-operation skips is not evidence of complete parity.
Verification reports will record the exact revision, backend, failures, and skips.
