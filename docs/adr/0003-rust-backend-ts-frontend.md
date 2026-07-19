# Rust backend, TypeScript frontend

The local process, Collector, and Relay are Rust; the board UI is TypeScript. Chosen over TypeScript-everywhere (which would have shared watcher code with a Node relay) for single-binary distribution with no runtime, and for the free path to a Tauri desktop shell later. Cost accepted: slower iteration than TS and two languages in the repo; the session-watching logic is still shared between local mode and Collector, just as a Rust crate instead of an npm module.
