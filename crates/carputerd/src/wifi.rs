//! Wi-Fi management, by asking NetworkManager through `nmcli`. Falling back
//! to a hotspot when no known network is in range needs no code at all: it is
//! a low-priority NetworkManager profile created by the installer.

use axum::{
    Json, Router,
    extract::Path,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{delete, get},
};
use carputer_core::nm;
use serde::Deserialize;
use tokio::process::Command;

pub fn router() -> Router {
    Router::new().route("/api/wifi", get(list).post(connect)).route("/api/wifi/{ssid}", delete(forget))
}

/// Run nmcli; `Err` is a response explaining why it did not work.
async fn nmcli(args: &[&str]) -> Result<String, Response> {
    match Command::new("nmcli").args(args).output().await {
        Ok(out) if out.status.success() => Ok(String::from_utf8_lossy(&out.stdout).into_owned()),
        Ok(out) => Err((StatusCode::BAD_GATEWAY, String::from_utf8_lossy(&out.stderr).trim().to_owned()).into_response()),
        Err(_) => Err((StatusCode::NOT_IMPLEMENTED, "NetworkManager is not available").into_response()),
    }
}

/// SSIDs are passed to nmcli as arguments; one starting with `-` would be
/// read as an option.
fn valid(ssid: &str) -> bool {
    !ssid.is_empty() && ssid.len() <= 32 && !ssid.starts_with('-')
}

async fn list() -> Result<Response, Response> {
    let networks = nm::wifi_list(&nmcli(&["-t", "-f", "ACTIVE,SSID,SIGNAL,SECURITY", "device", "wifi", "list"]).await?);
    let saved = nm::saved_wifi(&nmcli(&["-t", "-f", "NAME,TYPE", "connection", "show"]).await?);
    Ok(Json(serde_json::json!({ "networks": networks, "saved": saved })).into_response())
}

#[derive(Deserialize)]
struct Join {
    ssid: String,
    password: Option<String>,
}

async fn connect(Json(join): Json<Join>) -> Result<StatusCode, Response> {
    if !valid(&join.ssid) {
        return Err(StatusCode::BAD_REQUEST.into_response());
    }
    let saved = nm::saved_wifi(&nmcli(&["-t", "-f", "NAME,TYPE", "connection", "show"]).await?);
    match (&join.password, saved.contains(&join.ssid)) {
        (None, true) => nmcli(&["connection", "up", "id", &join.ssid]).await?,
        (None, false) => nmcli(&["device", "wifi", "connect", &join.ssid]).await?,
        (Some(password), _) => nmcli(&["device", "wifi", "connect", &join.ssid, "password", password]).await?,
    };
    Ok(StatusCode::NO_CONTENT)
}

async fn forget(Path(ssid): Path<String>) -> Result<StatusCode, Response> {
    if !valid(&ssid) {
        return Err(StatusCode::BAD_REQUEST.into_response());
    }
    nmcli(&["connection", "delete", "id", &ssid]).await?;
    Ok(StatusCode::NO_CONTENT)
}
