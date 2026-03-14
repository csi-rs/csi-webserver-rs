mod models;
mod routes;
mod serial;
mod state;

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use axum::{
    Router,
    routing::{get, post},
};
use clap::Parser;
use tokio::sync::{Mutex, broadcast, mpsc, watch};

use models::{DeviceConfig, OutputMode};
use state::AppState;

// ─── CLI ──────────────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
#[command(version, about = "CSI WebServer — streams ESP32 CSI data over WebSocket")]
struct Cli {
    /// Network interface to bind to.
    #[arg(long, default_value = "0.0.0.0")]
    interface: String,

    /// TCP port to listen on.
    #[arg(long, default_value_t = 3000)]
    port: u16,
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

    // ── Serial port detection ─────────────────────────────────────────────
    let port_path = match serial::detect_esp_port() {
        Ok(p) => p,
        Err(e) => {
            tracing::error!("{e}");
            std::process::exit(1);
        }
    };

    // ── Channels ──────────────────────────────────────────────────────────
    // cmd_tx        → serial task (CLI commands)
    // csi_tx        → all WebSocket clients (raw CSI frame bytes)
    // log_mode_tx   → serial task (frame-delimiter mode)
    // output_mode_tx→ serial task (stream / dump / both)
    // session_file_tx→serial task (current session dump file path)
    let (cmd_tx, cmd_rx) = mpsc::channel::<String>(64);
    let (csi_tx, _) = broadcast::channel::<Vec<u8>>(256);
    let (log_mode_tx, log_mode_rx) = watch::channel(String::new());
    let (output_mode_tx, output_mode_rx) = watch::channel(OutputMode::default());
    let (session_file_tx, session_file_rx) = watch::channel::<Option<String>>(None);

    // ── Shared state ──────────────────────────────────────────────────────
    let state = AppState {
        port_path: Arc::new(Mutex::new(port_path.clone())),
        serial_connected: Arc::new(AtomicBool::new(false)),
        collection_running: Arc::new(AtomicBool::new(false)),
        cmd_tx,
        csi_tx: csi_tx.clone(),
        log_mode_tx: Arc::new(log_mode_tx),
        output_mode_tx: Arc::new(output_mode_tx),
        session_file_tx: Arc::new(session_file_tx),
        config: Arc::new(Mutex::new(DeviceConfig::default())),
    };

    // ── Serial background task ────────────────────────────────────────────
    tokio::spawn(serial::run_serial_task(
        port_path,
        cmd_rx,
        csi_tx,
        log_mode_rx,
        output_mode_rx,
        session_file_rx,
        state.serial_connected.clone(),
        state.collection_running.clone(),
        state.port_path.clone(),
    ));

    // ── Router ────────────────────────────────────────────────────────────
    let app = Router::new()
        .route("/", get(|| async { "CSI Server Active" }))
        // Config
        .route("/api/config", get(routes::config::get_config))
        .route("/api/config/reset", post(routes::config::reset_config))
        .route("/api/config/wifi", post(routes::config::set_wifi))
        .route("/api/config/traffic", post(routes::config::set_traffic))
        .route("/api/config/csi", post(routes::config::set_csi))
        .route(
            "/api/config/collection-mode",
            post(routes::config::set_collection_mode),
        )
        .route("/api/config/log-mode", post(routes::config::set_log_mode))
        .route(
            "/api/config/output-mode",
            post(routes::config::set_output_mode),
        )
        // Control
        .route(
            "/api/control/start",
            post(routes::control::start_collection),
        )
        .route(
            "/api/control/status",
            get(routes::control::get_collection_status),
        )
        .route("/api/control/reset", post(routes::control::reset_esp32))
        // WebSocket
        .route("/api/ws", get(routes::ws::ws_handler))
        .with_state(state);

    // ── Serve ─────────────────────────────────────────────────────────────
    let addr = format!("{}:{}", cli.interface, cli.port);
    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    tracing::info!("CSI server listening on http://{addr}");
    axum::serve(listener, app).await.unwrap();
}
