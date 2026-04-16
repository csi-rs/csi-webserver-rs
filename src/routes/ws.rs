//! WebSocket upgrade and frame-stream forwarding endpoint.

use axum::{
    Json,
    extract::{
        FromRequestParts, State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::StatusCode,
    response::{IntoResponse, Response},
};
use tokio::sync::broadcast;

use crate::{
    models::{ApiResponse, OutputMode},
    state::AppState,
};

// ─── GET /api/ws ────────────────────────────────────────────────────────────

/// Upgrade an HTTP connection to a WebSocket and stream raw CSI frames.
///
/// Returns `403 Forbidden` when the server is in `dump` output mode, since
/// frames are being written exclusively to the session dump file.
/// The mode check happens before the WebSocket upgrade handshake so that any
/// HTTP client (not just WebSocket clients) receives the 403 correctly.
///
/// Each binary message sent to the client is one unmodified frame as received
/// from the ESP32 over serial. The client is responsible for decoding based
/// on the active log mode (e.g. array-list text or COBS binary).
pub async fn ws_handler(State(state): State<AppState>, req: axum::extract::Request) -> Response {
    // Check the mode BEFORE attempting the WebSocket upgrade extraction.
    // If WebSocketUpgrade were an extractor in the function signature, Axum
    // would reject non-upgrade requests with 400 before this body runs.
    if *state.output_mode_tx.borrow() == OutputMode::Dump {
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
    let ws = match WebSocketUpgrade::from_request_parts(&mut parts, &state).await {
        Ok(ws) => ws,
        Err(rejection) => return rejection.into_response(),
    };

    let rx = state.csi_tx.subscribe();
    ws.on_upgrade(|socket| handle_socket(socket, rx))
        .into_response()
}

async fn handle_socket(mut socket: WebSocket, mut rx: broadcast::Receiver<Vec<u8>>) {
    loop {
        tokio::select! {
            // ── Forward raw CSI frame to the WebSocket client ─────────────
            result = rx.recv() => {
                match result {
                    Ok(data) => {
                        if socket.send(Message::Binary(data.into())).await.is_err() {
                            // Client disconnected or send failed.
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        // Broadcast channel shut down (server stopping).
                        break;
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        // The client is too slow; skip dropped packets but stay connected.
                        tracing::warn!("WebSocket client lagged — dropped {n} CSI packets");
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

    tracing::debug!("WebSocket client disconnected");
}
