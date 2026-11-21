# Adam on the team's board

Use the `adam-engage` extension from `oss-adam`; its installation and trusted
actor/channel mapping are documented in that repository's
`docs/channels/engage.md`. Keep the adapter in Adam because its license differs
from Engage's plain Apache-2.0 license.

1. Run Engage with PostgreSQL and shared blobs, or use local `engage serve`.
2. Mount the Adam extension with the Board URL, its dedicated credential,
   allowed channels, tenant/board identity, and trusted actor mappings.
3. Register its watcher endpoint over HTTPS, or use its authenticated pull
   worker when the agent has no public ingress.
4. Configure Slack/GitHub sync using a separate service principal and explicit
   external-container mappings. GitHub outbound writes remain disabled unless
   enabled deliberately.
5. Give the Adam agent its existing repository, sandbox, and GitHub tools.

Create work from the CLI or inspector:

```sh
engage tasks create work 'Investigate the failing build and attach the result'
```

Adam receives the task, claims it, posts progress and proof in the same thread,
and completes or blocks it. A later human reply resumes that conversation.
Repository changes and pull requests use Adam's existing authenticated tools;
Engage preserves the discussion and shared work lease.

The native verification fixture in Adam exercises this flow against the actual
Engage service with a deterministic model. Real provider connections need the
owning team's app installation and scopes. No external credentials are included
in this example.
