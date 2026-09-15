//! Default CSI webserver executable — hotplug supervisor plus HTTP/WebSocket service.
//!
//! This binary depends only on the `csi-webserver-core` library.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use clap::{Args, Parser, Subcommand};
use csi_webserver_core::{AppState, ServerConfig, SupervisorConfig, run_supervisor, serve};

#[derive(Parser, Debug)]
#[command(
    version,
    about = "CSI WebServer — streams ESP32 CSI data over WebSocket"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command_>,

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

#[derive(Subcommand, Debug)]
enum Command_ {
    /// Flash a merged firmware image to a board over serial.
    ///
    /// Stop the server first: a serial port has a single holder, and the supervisor owns every
    /// `ttyACM*` it has discovered.
    Flash(FlashArgs),
}

#[derive(Args, Debug)]
struct FlashArgs {
    /// Serial port of the target board, e.g. `/dev/ttyACM0`.
    #[arg(long)]
    port: String,

    /// Target chip: esp32 | c3 | c5 | c6 | s3. Selects the image when `--image` is omitted.
    #[arg(long)]
    chip: String,

    /// Explicit merged `.bin` image. Defaults to
    /// `<firmware-dir>/esp-csi-cli-rs-<chip>.bin`.
    #[arg(long)]
    image: Option<PathBuf>,

    /// Directory holding the merged images.
    #[arg(long, env = "CSI_FIRMWARE_DIR", default_value = "dist")]
    firmware_dir: PathBuf,

    /// Flash offset for merged images (`espflash write-bin` address).
    #[arg(long, default_value = "0x0")]
    address: String,
}

/// Normalise a chip alias to its canonical espflash target name.
///
/// Every chip the firmware builds for can be flashed; which features that firmware then offers is
/// the firmware's business, not this command's.
fn canon_chip(chip: &str) -> Option<&'static str> {
    match chip.trim().to_ascii_lowercase().as_str() {
        "esp32" => Some("esp32"),
        "c3" | "esp32c3" => Some("esp32c3"),
        "c5" | "esp32c5" => Some("esp32c5"),
        "c6" | "esp32c6" => Some("esp32c6"),
        "s3" | "esp32s3" => Some("esp32s3"),
        _ => None,
    }
}

/// Resolve the merged `.bin` path: an explicit `--image` wins, else the conventional
/// `<firmware-dir>/esp-csi-cli-rs-<chip>.bin`.
fn resolve_image(
    chip: &str,
    firmware_dir: &Path,
    explicit: Option<&Path>,
) -> Result<PathBuf, String> {
    if let Some(p) = explicit {
        return Ok(p.to_path_buf());
    }
    let chip = canon_chip(chip)
        .ok_or_else(|| format!("Unknown chip '{chip}' (use esp32, c3, c5, c6, or s3)"))?;
    Ok(firmware_dir.join(format!("esp-csi-cli-rs-{chip}.bin")))
}

fn run_flash(args: FlashArgs) -> Result<(), String> {
    let chip = canon_chip(&args.chip).ok_or_else(|| {
        format!(
            "Unknown chip '{}' (use esp32, c3, c5, c6, or s3)",
            args.chip
        )
    })?;
    let image = resolve_image(&args.chip, &args.firmware_dir, args.image.as_deref())?;
    if !image.is_file() {
        return Err(format!(
            "firmware image not found: {}\n(build it with `espflash save-image --merge` into {})",
            image.display(),
            args.firmware_dir.display()
        ));
    }
    tracing::info!(
        "flashing {chip} on {} with {} at {}",
        args.port,
        image.display(),
        args.address
    );
    // A merged image flashes as one blob at the given offset. This shells out to `espflash`, the
    // same tool the release pipeline uses, so there is no new dependency and no second
    // implementation of the flash protocol to keep in step.
    let status = Command::new("espflash")
        .arg("write-bin")
        .arg("--port")
        .arg(&args.port)
        .arg("--chip")
        .arg(chip)
        .arg(&args.address)
        .arg(&image)
        .status()
        .map_err(|e| format!("failed to launch espflash (is it installed / on PATH?): {e}"))?;
    if !status.success() {
        return Err(format!("espflash exited with {status}"));
    }
    tracing::info!(
        "flash complete — restart the server; GET /api/devices/{{id}}/info shows the new banner"
    );
    Ok(())
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

    if let Some(Command_::Flash(args)) = cli.command {
        if let Err(e) = run_flash(args) {
            tracing::error!("{e}");
            std::process::exit(1);
        }
        return;
    }

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
