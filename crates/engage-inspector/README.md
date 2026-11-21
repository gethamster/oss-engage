# Engage inspector

The inspector is a Rust/WebAssembly application using the same HTTP API as the CLI. Axum serves its bundled assets at `/inspect/`.

Install the `wasm32-unknown-unknown` Rust target and [Trunk](https://trunkrs.dev/), then build:

```sh
rustup target add wasm32-unknown-unknown
cd crates/engage-inspector
trunk build --release --public-url /inspect/
```

Start `engage serve` from the repository root and open its `/inspect/` URL. The default asset directory is `crates/engage-inspector/dist`. For development, use `trunk watch` while the Rust service serves `dist/`; refresh the browser after a rebuild.

The build emits hashed JavaScript, WebAssembly, and CSS assets. Production does not need Trunk, Cargo, Node, or a separate frontend server. The server must add its CSP nonce to Trunk's inline module initializer.

Local boards use their loopback identity. Protected boards accept a bearer token through `POST /auth/session` and establish an HttpOnly session. The token field is cleared immediately after submission. The console stores task claim secrets only in tab memory and never in local storage or public message views. Switching profiles clears in-memory claims.

The console provides channel and thread navigation, task lifecycle actions, Markdown composition and preview, attachments, conditional edits, labels and reactions, schedules, actor profiles, grants, schemas, watcher registrations, inbox inspection, and provider source reads. JSON definition editors preserve the full schema, grant, and watcher interfaces without inventing a restricted subset.

Verify the browser build and rendering security:

```sh
cargo test -p engage-inspector
cargo clippy -p engage-inspector --target wasm32-unknown-unknown -- -D warnings
```
