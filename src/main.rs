//! Default CSI webserver executable — hotplug supervisor plus HTTP/WebSocket service.
//!
//! This binary depends only on the `csi-webserver-core` library.

use std::time::Duration;

use clap::Parser;
use csi_webserver_core::{AppState, ServerConfig, SupervisorConfig, run_supervisor, serve};

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

    /// Pin a friendly device id to a specific port or MAC, e.g.
    /// `--device lab1=/dev/ttyUSB0` or `--device lab1=D0:CF:13:E2:90:E8`.
    #[arg(long = "device", value_name = "ALIAS=PORT_OR_MAC")]
    devices: Vec<String>,

    /// How often (in milliseconds) the hotplug supervisor rescans for attached
    /// and removed devices.
    #[arg(long, default_value_t = 2000)]
    scan_interval_ms: u64,
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "csi_webserver_core=debug,csi_webserver=debug".into()),
        )
        .init();

    let aliases: Vec<(String, String)> = cli
        .devices
        .iter()
        .filter_map(|spec| {
            spec.split_once('=')
                .map(|(a, p)| (a.to_string(), p.to_string()))
        })
        .collect();
    for spec in &cli.devices {
        if !spec.contains('=') {
            tracing::warn!("Ignoring malformed --device '{spec}'; expected ALIAS=PORT");
        }
    }

    let state = AppState::new();

    tokio::spawn(run_supervisor(SupervisorConfig {
        registry: state.devices.clone(),
        baud_rate: cli.baud_rate,
        scan_interval: Duration::from_millis(cli.scan_interval_ms),
        aliases,
    }));

    let bind = format!("{}:{}", cli.interface, cli.port);
    serve(ServerConfig { bind }, state)
        .await
        .expect("server failed");
}
