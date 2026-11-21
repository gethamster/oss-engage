# Product

<!-- impeccable:product-schema 1 -->

## Platform

web

## Stack

Rust and WebAssembly with Leptos client-side rendering, Trunk builds, and an Axum service. Build directly in working code; no Node runtime in production.

## Users

People and their Adam agents coordinating work through channels, conversations, tasks, and attached proof. Operators also manage access, schedules, schemas, watchers, and platform sync.

## Product Purpose

Engage provides Board-compatible shared conversations and leased work queues, implemented natively in Rust. People can hand a task to an agent, follow its progress and proof, and continue the same conversation after completion or restart.

## Operating Context

The console runs alongside a self-hosted service. PostgreSQL and shared blob storage support multiple servers and agents; SQLite supports local operation. The console reads and writes through the same authenticated HTTP API as the CLI and Rust client.

## Capabilities and Constraints

Preserve the observable Board contract pinned in PORTING.md. Engage owns shared conversations, task claims, and delivery retries. Adam owns agent execution and session continuity. Keep the Engage distribution plain Apache-2.0 and independent of Adam's implementation code. Browser credentials stay in memory during sign-in and in HttpOnly sessions afterwards.

## Brand Commitments

Engage is named as the counter to Board's wordplay on bored. Product copy uses channels, threads, tasks, and proof directly.

## Product Principles

- Show the work and the controls needed to advance it.
- Preserve conversation and task identity across retries and restarts.
- Authenticate actions independently from displayed message authorship.
- Keep administration available without crowding daily work.
- Reuse complete interactions and retain meaningful errors and recovery.
