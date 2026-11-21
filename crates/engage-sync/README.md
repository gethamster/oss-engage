# Engage sync

`engage-sync` verifies platform webhooks, normalizes external events, and runs
leased synchronization through the Board HTTP contract. Slack supports messages,
files, reactions, source reads, public-channel joining, and assistant thread
status. GitHub imports PRs, issues, comments, reviews, and CI status; outbound
comments require explicit opt-in and never create an issue or PR. Linear mirrors
issue properties and threaded comments. Each provider also supports authenticated,
paged historical backfill.

Credentials come from a `CredentialResolver` on every use. An Adam connection
broker can implement this trait without adding an Adam dependency to Engage.
`Secret` redacts its debug output and clears its owned memory on drop.

A standalone configuration references environment variable names:

```json
{
  "providers": [
    {
      "key": "slack",
      "platform": "slack",
      "principal": "service:slack",
      "token_env": "SLACK_TOKEN",
      "signing_secret_env": "SLACK_SIGNING_SECRET",
      "bot_id_env": "SLACK_BOT_USER_ID",
      "channels": { "C012345": "work" },
      "allow_join": false
    }
  ]
}
```

Parse as `SyncConfig` and call `Registry::from_config`. Unknown fields, embedded
credential values, duplicate keys, and invalid mappings are rejected. GitHub
uses `outbound: true` to authorize writes to existing threads. Linear personal
API keys use `api_key: true`; OAuth credentials use Bearer authentication.

Use a **dedicated service principal** for each independently operated sync
worker. `SyncWorker::register` owns that principal's complete watcher
registration, including its `@sync` subscription; sharing it with an Adam
application would replace that application's registrations. Multiple providers
in the same registry may intentionally share one service principal.

The service must persist every verified incoming event before returning a
successful webhook acknowledgment. `SyncWorker::inbox` then imports each leased
event before acknowledging it, preserving the platform's thread and author
coordinates. Imported authors are namespaced by sync key. The worker retrieves
an unseen parent thread before importing its first observed reply.

`SyncWorker::backfill` reads one provider page and queues its events through
`POST /inbox/:key`; call it with the returned `next` while `more` is true. It uses
the same receipts, ordering, and retries as live intake. Repeating a page is safe.

Outbound workers pull `@sync` watcher matches and persist delivery receipts.
Messages with `noSync`, imports from the destination key, and recorded deliveries
are suppressed. Platform writes are at least once: a crash after a provider
accepts a write but before its receipt commits can repeat the operation where
the provider offers no idempotency mechanism. Slack messages carry a stable UUID;
provider support, rather than a local lock, determines remote deduplication.

Run `cargo test -p engage-sync` for signed webhook fixtures, worker replay and
failure tests, and real SQLite inbox/import/receipt journeys. Those tests use
synthetic payloads and a mock outbound provider; they do not establish that an
installation's live app scopes, webhook URLs, or credentials work.

The provider behavior is ported from `vercel/board` at
`fef19e5bb1e17551f119be11f7af8ed3387892bb`. Applicable Vercel attribution and
Apache-2.0 terms are retained in the repository's license and notices.
