//! WebSocket upgrade and frame-stream forwarding endpoint.

use axum::{
    Json,
    extract::{
        FromRequestParts,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::StatusCode,
    response::{IntoResponse, Response},
};
use tokio::sync::broadcast;
use tokio::time::Duration;
use tokio_util::sync::CancellationToken;

use crate::{
    models::{ApiResponse, OutputMode},
    routes::Device,
};

// ─── GET /api/ws ────────────────────────────────────────────────────────────

/// Upgrade an HTTP connection to a WebSocket and stream raw CSI frames.
///
/// Returns `403 Forbidden` when the server is in `dump` output mode, since
/// frames are being written exclusively to the session dump file.
/// The mode check happens before the WebSocket upgrade handshake so that any
/// HTTP client (not just WebSocket clients) receives the 403 correctly.
///
/// Each binary message sent to the client is one unmodified serialized frame
/// as received from the ESP32 over serial — a COBS-framed postcard record
/// (trailing `\0` stripped). The client COBS-decodes then postcard-decodes it
/// (see the WebSocket frame schema in API.md).
pub async fn ws_handler(Device(dev): Device, req: axum::extract::Request) -> Response {
    // Check the mode BEFORE attempting the WebSocket upgrade extraction.
    // If WebSocketUpgrade were an extractor in the function signature, Axum
    // would reject non-upgrade requests with 400 before this body runs.
    if *dev.output_mode_tx.borrow() == OutputMode::Dump {
        return (
            StatusCode::FORBIDDEN,
            Json(ApiResponse {
                success: false,
                message: "Server is in dump-only mode; WebSocket streaming is disabled".to_string(),
            }),
        )
            .into_response();
    }

    let (mut parts, _body) = req.into_parts();
    // WebSocketUpgrade reads only the request headers; its state type is
    // irrelevant here, so extract it against the unit state.
    let ws = match WebSocketUpgrade::from_request_parts(&mut parts, &()).await {
        Ok(ws) => ws,
        Err(rejection) => return rejection.into_response(),
    };

    let rx = dev.csi_tx.subscribe();
    // Clone (not the whole handle) so an unplug — which cancels this token via
    // the supervisor — promptly closes the socket regardless of Arc lifetimes.
    let shutdown = dev.shutdown.clone();
    let id = dev.id.clone();
    ws.on_upgrade(move |socket| handle_socket(socket, rx, shutdown, id))
        .into_response()
}

async fn handle_socket(
    mut socket: WebSocket,
    mut rx: broadcast::Receiver<Vec<u8>>,
    shutdown: CancellationToken,
    id: String,
) {
    // Per-second WebSocket-side throughput counters, the consumer-side
    // companion to the serial-side metrics in `crate::serial`. `sent` is what
    // the client actually received; `dropped` is what the broadcast channel
    // discarded because this client could not keep up (`Lagged`). A device that
    // streams slowly with high `dropped` is consumer-bound, not serial-bound.
    let mut sent: u64 = 0;
    let mut dropped: u64 = 0;
    let mut metrics = tokio::time::interval(Duration::from_secs(1));
    metrics.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    metrics.tick().await; // consume the immediate first tick

    loop {
        tokio::select! {
            // ── Per-second throughput report ──────────────────────────────
            _ = metrics.tick() => {
                if sent > 0 || dropped > 0 {
                    tracing::debug!(
                        target: "csi_metrics",
                        "ws {id}: sent={sent}/s dropped={dropped}/s",
                    );
                    sent = 0;
                    dropped = 0;
                }
            }

            // ── Device unplugged: close the socket cleanly ────────────────
            _ = shutdown.cancelled() => {
                let _ = socket.send(Message::Close(None)).await;
                break;
            }

            // ── Forward raw CSI frame to the WebSocket client ─────────────
            result = rx.recv() => {
                match result {
                    Ok(data) => {
                        if socket.send(Message::Binary(data.into())).await.is_err() {
                            // Client disconnected or send failed.
                            break;
                        }
                        sent += 1;
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        // Broadcast channel shut down (server stopping).
                        break;
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        // The client is too slow; skip dropped packets but stay connected.
                        dropped += n;
                        tracing::warn!("WebSocket client for {id} lagged — dropped {n} CSI packets");
                    }
                }
            }

            // ── Detect client-initiated close or disconnect ────────────────
            msg = socket.recv() => {
                match msg {
                    Some(Ok(Message::Close(_))) | None => break,
                    _ => {} // Ignore pings / pong / unexpected binary frames.
                }
            }
        }
    }

    tracing::debug!("WebSocket client for {id} disconnected");
}
