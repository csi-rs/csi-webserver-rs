//! Core modules for the `csi-webserver` executable.
//!
//! This package is primarily run as a process (`csi-webserver`), but the
//! library target is intentionally documented so docs.rs provides a complete
//! technical reference for request payloads, state models, route handlers, and
//! serial processing behavior.
//!
//! ## Architecture
//!
//! 1. `serial` maintains an ESP32 serial connection and parses frame boundaries.
//! 2. `routes` exposes HTTP and WebSocket handlers using Axum.
//! 3. `state` holds shared runtime channels and mutable config snapshots.
//! 4. `models` defines API payloads, response types, and command mappers.
//!
//! ## Typical runtime flow
//!
//! 1. Configure device and parser mode through `/api/config/*` endpoints.
//! 2. Start session via `/api/control/start`.
//! 3. Consume frames from `/api/ws` or dump file output.
//! 4. Inspect runtime status via `/api/control/status`.
//!
//! ## Public modules
//!
//! - [`models`] request and response schema types.
//! - [`state`] application-wide shared state.
//! - [`serial`] serial I/O and framing pipeline.
//! - [`routes`] HTTP and WebSocket route handlers.

pub mod models;
pub mod routes;
pub mod serial;
pub mod state;
