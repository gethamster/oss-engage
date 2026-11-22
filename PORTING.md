# Board porting record

## Baseline

Vercel Board `fef19e5bb1e17551f119be11f7af8ed3387892bb`, package 0.53.0.
https://github.com/vercel/board/tree/fef19e5bb1e17551f119be11f7af8ed3387892bb

Source implementation, api/routes.ts, and executable tests take precedence over
SPEC.md, which is a draft. Upstream is Apache-2.0, Copyright 2026 Vercel Inc.
Upstream has no NOTICE file. Engage adaptations are modified Rust implementations.

The subject-scoped OIDC grant fix from upstream
`c935700a992313a6b6614159ce0c024c7df8a5e1` is also implemented. The conformance
oracle remains pinned to the baseline above.

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
- Native middleware uses typed Continue/Drop decisions. Its borrowed board
  capability shares the write transaction; side effects roll back with a dropped
  or failed write. This differs from upstream JavaScript callback side effects.
- Self-hosted credentials use standard OIDC and injected provider resolvers.
  Vercel Connect and deployment wake routing are optional platform integrations.

## Verification status

- Pinned upstream oracle: 85 passed, 0 failed, 6 contextual skips on SQLite and
  PostgreSQL. All ten groups have passing checks; none are skipped as unsupported.
  Full named-check reports are retained in docs/verification/.
- The six upstream skips concern absent team roles/apps, owner lockout, a
  non-owner scenario the oracle cannot exercise safely, and unspecified cross-team
  grants. Native tests independently exercise grant precedence and revocation.
- Two PostgreSQL replicas sharing an S3-compatible emulator passed conditional
  edit/claim races, idempotency, stale fencing, secret redaction, shared files, and
  snapshot/feed continuity. Restart tests preserve active delivery leases,
  acknowledgements, and a scheduled SQLite send through downtime.
- 131 native tests passed; formatting, strict Clippy, rustdoc, dependency audit,
  inspector WASM build/Clippy, and native release build passed. Dependency audit
  reports zero vulnerabilities and three unmaintained dependency warnings.
  Advisory fixes select SQLx 0.9 with
  libsqlite3-sys 0.40, object_store 0.14, and AWS-LC-backed JWT verification.
- Browser journeys cover task creation, claiming, progress replies, completion,
  desktop and mobile. Remote OIDC and large-file proxy flows have transport tests;
  those two flows have not been exercised end-to-end in a remote browser.
- Adam canonical Foundry commit a1326ae6b5dc32dc7f9ffd2e568117ee217fb060 passed
  actual HTTP wake delivery into its native channel, real workflow worker/session
  engine, and deterministic model: claim, rival rejection, proof, renewal,
  completion, and same-session human follow-up. No paid model was used.
- Provider live installations, scopes, and real external writes are unverified.
  Signed fixtures, mock platforms, and actual SQLite integration journeys cover
  their protocol behavior. The S3 test is an emulator, not a live cloud bucket.

## Native surface differences

- Rust middleware uses typed Continue/Drop decisions and a borrowed transactional
  board capability rather than JavaScript next callbacks.
- The standalone CLI accepts grant update JSON instead of every upstream
  add/remove flag. Vercel-hosted account/board selection commands are not ported;
  explicit Board URLs and named local profiles select services.
- The standalone backend accepts up to 64 KiB of message text rather than the
  upstream local backend's 10,000-character limit. Platform inbox text remains
  whole. Rust strings cannot contain lone UTF-16 surrogates.
- The embedding router convenience is HTTP-only. Use Service and WorkerHandle
  when scheduling, sync, and wake delivery must run inside another application.

The container build recipe includes the native package and dependency notices.
It was not executed locally because the Docker daemon is unavailable. The native
macOS archive was booted and its bundled inspector/WASM and task API verified.

Task command runners enforce confirmed monotonic lease deadlines even while a
renewal request is stalled. They use closed standard input; on Unix, lease loss
stops the worker process group. Client transport diagnostics redact upload
capability URLs. Dedicated regressions cover these boundaries.

The follow-up gap audit corrected forward thread paging, creation-based activity
order, phrase search, assigned-task selection, canceled schedule filtering, and
HTTP/SSE wait validation. Shutdown now drains active streams, polls, and request
bodies. Inspector task/conversation paging preserves filters; long threads can
be read from the start in bounded pages using the pinned `after` protocol.
CLI regressions cover JSON task creation, malformed input, oversized timeouts,
and acquisition leases after a long poll.

Provider regressions cover Slack form arguments, reply reactions and root replay,
repository-scoped GitHub backfill IDs, ignored Slack callbacks, redacted transport
errors, and independent delivery progress after another thread fails. Existing
OIDC grants without a subject require owner review as described in operations.
Adam regressions cover renewal capability redaction, private deadline updates,
cached response sanitation, canonical wait roots, and independent reply retries.
