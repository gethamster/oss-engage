# Engage contributor guide

Engage is a Rust-native implementation of Vercel Board for people and agents.
The upstream contract is pinned in PORTING.md. Preserve protocol behavior and
check actual conformance, including skipped checks, before claiming parity.

All Engage-authored source is plain Apache-2.0. Preserve Vercel attribution on
adaptations. Do not import Adam or OSS Delivery code or contributor bundles:
they have additional license conditions. Adam integration belongs in oss-adam.

Use small Rust modules, typed errors, validated boundaries, and explicit async
ownership. Never log claim tokens, bearer tokens, provider credentials, or
private conversation content. Keep authentication separate from message metadata.

Ship tested increments to the explicitly authorized gethamster/oss-engage GitHub
repository. Preserve unrelated work. Run relevant existing checks; do not add
coverage quotas or process gates. Before committing inspect the diff and check
for secrets. Use one shared target cache with a 12 GiB retained-size bound; do
not run cargo clean while another worker is using it. Preserve required evidence
before retiring task-owned resources. The current main checkout is retained.

Required verification: cargo fmt --all --check; cargo clippy --workspace
--all-targets -- -D warnings; cargo test --workspace; cargo doc --workspace
--no-deps; inspector release WASM build; upstream conformance against both SQL
backends; multi-instance recovery and affected browser/Adam journeys.

Core and store own domain invariants. HTTP, CLI, providers, and inspector must
use the same authorization and mutation pipeline. Cursors are opaque strings;
feed positions must reflect commit order. Never put claim secrets in public
message or event projections. Do not mark unsupported features as conformance
success through 501 skips.
