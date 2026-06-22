//! Core modules for the `csi-webserver` executable.
//!
//! This package is primarily run as a process (`csi-webserver`), but the
//! library target is intentionally documented so docs.rs provides a complete
//! technical reference for request payloads, state models, route handlers, and
//! serial processing behavior.
//!
//! ## Architecture
//!
//! 1. `serial` discovers ESP32 devices (hotplug supervisor), maintains one
//!    serial connection per device, and parses frame boundaries.
//! 2. `routes` exposes HTTP and WebSocket handlers using Axum.
//! 3. `state` holds the device registry; each `DeviceHandle` owns that device's
//!    runtime channels and mutable config snapshot.
//! 4. `models` defines API payloads, response types, and command mappers.
//!
//! ## Typical runtime flow
//!
//! 1. List attached devices via `/api/devices` and pick an id.
//! 2. Configure the device through `/api/devices/{id}/config/*` (the CSI wire
//!    format is fixed to `serialized`).
//! 3. Start session via `/api/devices/{id}/control/start`.
//! 4. Consume raw frames from `/api/devices/{id}/ws` or read the Parquet dump.
//! 5. Inspect runtime status via `/api/devices/{id}/control/status`.
//!
//! ## Public modules
//!
//! - [`models`] request and response schema types.
//! - [`state`] application-wide shared state.
//! - [`serial`] serial I/O and framing pipeline.
//! - [`routes`] HTTP and WebSocket route handlers.

pub mod csi;
pub mod models;
pub mod parquet_sink;
pub mod routes;
pub mod serial;
pub mod state;
