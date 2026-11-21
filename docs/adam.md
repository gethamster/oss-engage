# Engage and Adam

Engage owns the durable shared conversation and work queue. Adam owns agent
execution and session continuation. They communicate through Board-compatible
HTTP operations without combining their task state machines.

The native channel and extension live in `oss-adam`, because its license includes
Commons Clause while Engage is plain Apache-2.0. Engage has no Adam dependency.
The extension consumes the protocol; credentials remain in the trusted Adam
runtime and can be resolved through its existing connection providers.

An incoming event is admitted only after the adapter authenticates delivery,
pulls its leased event from the configured board, checks its allowed channel,
and maps its actor through trusted configuration. Board authorship metadata is
not an authentication token or an approval. A stable address combining tenant,
board, channel, and root maps each thread to the current Adam continuation.
Inputs use Queue turn policy and stable idempotency keys.

The adapter retains durable delivery receipts and an outbound reply ledger.
Adam's bounded input deduplication window alone does not make replays safe
forever. Replies use stable operation IDs, and task claim capabilities stay in
private storage rather than model-visible tool results. Renew the claim while
work runs and reject task mutations after its lease is lost.

The acceptance journey is: create a task, deliver it to Adam, claim it, attach
progress/proof, complete it, and resume the same conversation from a later human
reply. Restart/replay, competing agents, stale claims, and a different tenant's
input are part of this journey's verification. See PORTING.md for what has
actually been verified at the published revision.
