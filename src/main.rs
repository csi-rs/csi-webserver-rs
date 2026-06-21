//! Binary entrypoint and composition root for `csi-webserver`.
//!
//! This module builds the complete runtime graph for the service:
//! - parses CLI bind options,
//! - configures tracing and log filtering,
//! - spawns the hotplug supervisor that discovers ESP32 devices and runs one
//!   serial background worker per device,
//! - mounts Axum routes and starts the TCP listener.
//!
//! # Service role
//!
//! `csi-webserver` is a bridge between ESP32 CSI firmware output and network
//! consumers. Incoming serial frames are forwarded by the background task to:
//! - WebSocket subscribers (`/api/ws`),
//! - session dump files on disk,
//! - or both, depending on configured output mode.
//!
//! Configuration and control commands are exposed as HTTP endpoints and sent to
//! the serial worker through an async command channel.
//!
//! # Startup lifecycle
//!
//! 1. Parse `--interface`, `--port`, `--baud-rate`, `--device`, `--scan-interval-ms`.
//! 2. Initialize tracing using `RUST_LOG` when provided.
//! 3. Assemble an empty device registry into shared
//!    [`AppState`](crate::state::AppState).
//! 4. Spawn the [`serial::run_supervisor`] hotplug task, which discovers
//!    devices, assigns ids, and spawns one [`serial::run_serial_task`] each.
//! 5. Register HTTP routes and begin serving requests.
//!
//! # Route surface
//!
//! - `GET /` basic health text response.
//! - `GET /api/devices` list attached devices and their status.
//! - `/api/devices/{id}/config/*` per-device configuration.
//! - `/api/devices/{id}/control/*` per-device collection lifecycle and status.
//! - `GET /api/devices/{id}/ws` per-device live binary CSI frame stream.
//!
//! # Failure behavior
//!
//! Startup never exits for lack of a device — the server serves with an empty
//! registry until one is plugged in. Per-device disconnects are handled by each
//! worker's reconnect loop; sustained absence is detected by the supervisor,
//! which removes the device. HTTP routes return `404` for unknown device ids.

mod models;
mod routes;
mod serial;
mod state;

use std::sync::Arc;
use std::time::Duration;

use axum::{
    Router,
    routing::{get, post},
};
use clap::Parser;

use state::{AppState, DeviceRegistry};

// ─── CLI ──────────────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
#[command(
    version,
    about = "CSI WebServer — streams ESP32 CSI data over WebSocket"
)]
struct Cli {
    /// Network interface to bind to.
    #[arg(long, default_value = "0.0.0.0")]
    interface: String,

    /// TCP port to listen on.
    #[arg(long, default_value_t = 3000)]
    port: u16,

    /// UART baud rate used to talk to every ESP32. Falls back to the
    /// `CSI_BAUD_RATE` environment variable when the flag is omitted.
    #[arg(long, env = "CSI_BAUD_RATE", default_value_t = 115_200)]
    baud_rate: u32,

    /// Pin a stable device id to a specific port, e.g. `--device lab1=/dev/ttyUSB0`.
    /// Repeatable. Without an override, a device's id is the sanitized port
    /// basename (e.g. `ttyUSB0`).
    #[arg(long = "device", value_name = "ALIAS=PORT")]
    devices: Vec<String>,

    /// How often (in milliseconds) the hotplug supervisor rescans for attached
    /// and removed devices.
    #[arg(long, default_value_t = 2000)]
    scan_interval_ms: u64,
}

#[tokio::main]
async fn main() {
    // ── CLI args ──────────────────────────────────────────────────────────
    let cli = Cli::parse();

    // ── Tracing ───────────────────────────────────────────────────────────
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "csi_webserver=debug".into()),
        )
        .init();

    // ── Alias overrides ───────────────────────────────────────────────────
    // Parse `--device alias=port` pairs the supervisor uses to assign stable ids.
    let aliases: Vec<(String, String)> = cli
        .devices
        .iter()
        .filter_map(|spec| {
            spec.split_once('=').map(|(a, p)| (a.to_string(), p.to_string()))
        })
        .collect();
    for spec in &cli.devices {
        if !spec.contains('=') {
            tracing::warn!("Ignoring malformed --device '{spec}'; expected ALIAS=PORT");
        }
    }

    // ── Shared state ──────────────────────────────────────────────────────
    // The registry starts empty; the hotplug supervisor populates it on its
    // first scan and keeps it in sync as devices are plugged/unplugged.
    let state = AppState {
        devices: Arc::new(DeviceRegistry::default()),
    };

    // ── Hotplug supervisor ────────────────────────────────────────────────
    // Owns device lifecycle: discovers ESP32 ports, spawns one serial task per
    // device, and tears tasks down when a device is unplugged. Unlike the old
    // single-device build, we do NOT exit when nothing is attached — devices
    // can appear at any time.
    tokio::spawn(serial::run_supervisor(
        state.devices.clone(),
        cli.baud_rate,
        Duration::from_millis(cli.scan_interval_ms),
        aliases,
    ));

    // ── Router ────────────────────────────────────────────────────────────
    // Every per-device operation lives under `/api/devices/{id}/...`; the `{id}`
    // segment is resolved to a device by the `Device` extractor.
    let device_routes = Router::new()
        // Config
        .route("/config", get(routes::config::get_config))
        .route("/config/reset", post(routes::config::reset_config))
        .route("/config/wifi", post(routes::config::set_wifi))
        .route("/config/traffic", post(routes::config::set_traffic))
        .route("/config/csi", post(routes::config::set_csi))
        .route(
            "/config/collection-mode",
            post(routes::config::set_collection_mode),
        )
        .route("/config/log-mode", post(routes::config::set_log_mode))
        .route("/config/output-mode", post(routes::config::set_output_mode))
        .route("/config/rate", post(routes::config::set_rate))
        .route("/config/io-tasks", post(routes::config::set_io_tasks))
        .route("/config/csi-delivery", post(routes::config::set_csi_delivery))
        // Control
        .route("/control/start", post(routes::control::start_collection))
        .route("/control/stop", post(routes::control::stop_collection))
        .route("/control/status", get(routes::control::get_collection_status))
        .route("/control/reset", post(routes::control::reset_esp32))
        .route("/control/stats", post(routes::config::show_stats))
        // Firmware identification
        .route("/info", get(routes::info::get_info))
        // WebSocket
        .route("/ws", get(routes::ws::ws_handler));

    let app = Router::new()
        .route("/", get(|| async { "CSI Server Active" }))
        .route("/api/devices", get(routes::devices::list_devices))
        .nest("/api/devices/{id}", device_routes)
        .with_state(state);

    // ── Serve ─────────────────────────────────────────────────────────────
    let addr = format!("{}:{}", cli.interface, cli.port);
    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    tracing::info!("CSI server listening on http://{addr}");
    axum::serve(listener, app).await.unwrap();
}
