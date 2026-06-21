//! HTTP route handler modules.

pub mod config;
pub mod control;
pub mod devices;
pub mod info;
pub mod ws;

use std::sync::Arc;

use axum::{
    Json,
    extract::{FromRequestParts, Path},
    http::{StatusCode, request::Parts},
};

use crate::{
    models::ApiResponse,
    state::{AppState, DeviceHandle},
};

/// Extractor that resolves the `{id}` path segment to a [`DeviceHandle`] from
/// the registry. Rejects with `404 Not Found` when no such device exists, so
/// every per-device handler shares one lookup-and-reject path.
pub struct Device(pub Arc<DeviceHandle>);

impl FromRequestParts<AppState> for Device {
    type Rejection = (StatusCode, Json<ApiResponse>);

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let Path(id) = Path::<String>::from_request_parts(parts, state)
            .await
            .map_err(|_| {
                (
                    StatusCode::BAD_REQUEST,
                    Json(ApiResponse {
                        success: false,
                        message: "Missing device id in request path".to_string(),
                    }),
                )
            })?;

        state.devices.get(&id).map(Device).ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                Json(ApiResponse {
                    success: false,
                    message: format!("No device with id '{id}'"),
                }),
            )
        })
    }
}
