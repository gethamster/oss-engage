# Operating Engage

## Storage and instances

One database represents one board. Run multiple service instances against the
same PostgreSQL database and S3-compatible bucket to share that board. Use
separate databases and credential policies for independently administered boards.
SQLite and filesystem blobs serve a local board; they are not a shared cluster
backend. In-memory SQLite is intended for tests and disposable sessions.

Each mutation takes the board clock lock before reading conditions, appending
its events, and updating indexed records in the same transaction. Event positions
therefore follow commit order. Task and delivery leases are enforced in SQL;
requests carry their epoch/token or lease, and stale holders cannot acknowledge
someone else's work. Database time governs cluster lease calculations.

Servers poll durable work. A restart does not depend on having received a wake
notification, and a wake payload never supplies a trusted event. External
provider operations use at-least-once delivery; a network failure after a platform
accepted a request can require reconciliation using its receipts/idempotency
capabilities. Do not claim universal exactly-once external effects.

## Authentication

Local unauthenticated mode binds only to loopback and checks Host and Origin.
Use OIDC or an explicit bearer credential for a remote listener. Configure OIDC
issuer, audience, JWKS URL, and the exact owner subjects; JWT claims are verified
before grant resolution. Profile names do not create new permissions.

Every replica serving the same browser sessions needs the same secret
`ENGAGE_SESSION_KEY` (at least 32 bytes). Supply it from the deployment's secret
manager. The browser receives a signed HttpOnly session cookie; provider and
server bearer credentials are never stored in browser local storage.

Set the canonical external address with `ENGAGE_PUBLIC_URL` and terminate HTTPS
at the ingress. Do not expose a listener without authentication or treat
forwarded identity headers as proof. Tailscale mode resolves the actual socket
peer through the provisioned local tailscaled WhoIs API; remote request headers
cannot provide a Tailscale identity.

The server bounds request bodies, active event streams, and rate-limit state.
Rate limits are per service instance; apply a shared ingress limit when a
cluster needs one global request budget.

## Cluster configuration

Build the inspector and executable, then configure these environment variables
through the deployment environment:

- `ENGAGE_DATABASE_URL`: PostgreSQL connection URL.
- `ENGAGE_S3_BUCKET`, `ENGAGE_S3_REGION`, and optional `ENGAGE_S3_ENDPOINT`.
- The object store's workload identity or standard AWS credential chain.
- `ENGAGE_PUBLIC_URL` and `ENGAGE_SESSION_KEY`.
- `ENGAGE_OIDC_ISSUER`, `ENGAGE_OIDC_AUDIENCE`, `ENGAGE_OIDC_JWKS_URL`, and
  comma-separated `ENGAGE_OIDC_OWNERS`; alternatively `ENGAGE_SERVER_TOKEN`.
- Optional `ENGAGE_SYNC_CONFIG`: JSON containing provider keys, trusted service
  principals, explicit channel mappings, and credential environment-variable
  names. Configuration contains references, not secret values.

Start each instance with `engage serve --bind 0.0.0.0:4100`. For a container, mount
credentials as secret environment variables or use workload identity. The image
runs as an unprivileged user. Do not add live databases or credentials to Git.

## Verification and recovery

Use the owning process supervisor to restart unhealthy instances. Retain the
PostgreSQL data and blob bucket across restarts. Keep backup/restore procedures
with the deployment that owns these stores; never treat them as build caches.

For a disposable board, `scripts/conformance.py --url URL --prepare` checks the
pinned upstream protocol. It creates messages, tasks, schemas, and grants, so
use a scratch database rather than a production conversation board. The runner
fails on failed checks, missing groups, or unsupported-operation skips. Review
all remaining skips in the retained JSON report.

`scripts/cluster-smoke.py URL_A URL_B` checks shared idempotency, conditional
writes, task fencing, shared attachments, and feed continuity. Set
`ENGAGE_SMOKE_TOKEN` only when the scratch service requires a bearer token.
Use the same authenticated test identity on both instances.
