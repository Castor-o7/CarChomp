//! Wi-Fi management, by asking NetworkManager through `nmcli`. Falling back
//! to a hotspot when no known network is in range needs no code at all: it is
//! a low-priority NetworkManager profile created by the installer.

use axum::{
    Json, Router,
    extract::Path,
    http::StatusCode,
    middleware,
    response::{IntoResponse, Response},
    routing::{delete, get},
};
use carchomp_core::nm;
use serde::Deserialize;
use std::process::Stdio;
use tokio::{io::AsyncWriteExt, process::Command};

/// The fallback access point's profile (deploy/install.sh). Not a network to
/// join or forget: deleting it would strand every client of the hotspot.
const HOTSPOT: &str = "carchomp-hotspot";

pub fn router() -> Router {
    Router::new()
        .route("/api/wifi", get(list).post(connect))
        .route("/api/wifi/{ssid}", delete(forget))
        .route_layer(middleware::from_fn(crate::api::same_origin))
}

/// Run nmcli, writing `input` to its standard input (where `--ask` reads
/// secrets, which must not appear in the argument list). `Err` is a response
/// explaining why it did not work.
async fn nmcli_with(args: &[&str], input: Option<&str>) -> Result<String, Response> {
    let child = Command::new("nmcli").args(args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn();
    let Ok(mut child) = child else {
        return Err((StatusCode::NOT_IMPLEMENTED, "NetworkManager is not available").into_response());
    };
    let mut stdin = child.stdin.take().expect("piped");
    if let Some(input) = input {
        let _ = stdin.write_all(format!("{input}\n").as_bytes()).await;
    }
    drop(stdin);
    match child.wait_with_output().await {
        Ok(out) if out.status.success() => Ok(String::from_utf8_lossy(&out.stdout).into_owned()),
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            tracing::warn!("nmcli {}: {}", args.first().unwrap_or(&""), stderr.trim());
            Err((StatusCode::BAD_GATEWAY, explain(&stderr)).into_response())
        }
        Err(_) => Err((StatusCode::NOT_IMPLEMENTED, "NetworkManager is not available").into_response()),
    }
}

async fn nmcli(args: &[&str]) -> Result<String, Response> {
    nmcli_with(args, None).await
}

/// A fixed message for the client instead of whatever nmcli printed.
fn explain(stderr: &str) -> &'static str {
    if stderr.contains("Secrets were required") || stderr.contains("password") {
        "wrong or missing password"
    } else if stderr.contains("No network with SSID") {
        "network not in range"
    } else {
        "NetworkManager could not do that (see the daemon log)"
    }
}

/// SSIDs are passed to nmcli as arguments; one starting with `-` would be
/// read as an option.
fn valid(ssid: &str) -> bool {
    !ssid.is_empty() && ssid.len() <= 32 && !ssid.starts_with('-')
}

/// UUIDs of saved Wi-Fi profiles in `nmcli -t -f NAME,UUID,TYPE connection
/// show`, without the hotspot.
fn wifi_profiles(output: &str) -> Vec<String> {
    output
        .lines()
        .filter_map(|line| match &nm::fields(line)[..] {
            [name, uuid, kind] if kind == "802-11-wireless" && name != HOTSPOT => Some(uuid.clone()),
            _ => None,
        })
        .collect()
}

/// Saved networks as (SSID, profile UUID). A profile's name need not be its
/// SSID: Raspberry Pi Imager calls its one `preconfigured`.
async fn saved() -> Result<Vec<(String, String)>, Response> {
    let mut saved = Vec::new();
    for uuid in wifi_profiles(&nmcli(&["-t", "-f", "NAME,UUID,TYPE", "connection", "show"]).await?) {
        let out = nmcli(&["-g", "802-11-wireless.ssid", "connection", "show", "uuid", &uuid]).await?;
        let ssid = nm::fields(out.strip_suffix('\n').unwrap_or(&out)).join(":");
        if !ssid.is_empty() {
            saved.push((ssid, uuid));
        }
    }
    Ok(saved)
}

async fn list() -> Result<Response, Response> {
    let networks = nm::wifi_list(&nmcli(&["-t", "-f", "ACTIVE,SSID,SIGNAL,SECURITY", "device", "wifi", "list"]).await?);
    let saved: Vec<String> = saved().await?.into_iter().map(|(ssid, _)| ssid).collect();
    Ok(Json(serde_json::json!({ "networks": networks, "saved": saved })).into_response())
}

#[derive(Deserialize)]
struct Join {
    ssid: String,
    password: Option<String>,
}

async fn connect(Json(join): Json<Join>) -> Result<StatusCode, Response> {
    if !valid(&join.ssid) || join.password.as_deref().is_some_and(|p| p.contains('\n')) {
        return Err(StatusCode::BAD_REQUEST.into_response());
    }
    let saved = saved().await?;
    let profile = saved.iter().find(|(ssid, _)| *ssid == join.ssid).map(|(_, uuid)| uuid.as_str());
    match (&join.password, profile) {
        (None, Some(uuid)) => nmcli(&["connection", "up", "uuid", uuid]).await?,
        (None, None) => nmcli(&["device", "wifi", "connect", &join.ssid]).await?,
        (Some(password), _) => nmcli_with(&["--ask", "device", "wifi", "connect", &join.ssid], Some(password)).await?,
    };
    Ok(StatusCode::NO_CONTENT)
}

async fn forget(Path(ssid): Path<String>) -> Result<StatusCode, Response> {
    if !valid(&ssid) {
        return Err(StatusCode::BAD_REQUEST.into_response());
    }
    let saved = saved().await?;
    let Some((_, uuid)) = saved.iter().find(|(saved, _)| *saved == ssid) else {
        return Err(StatusCode::NOT_FOUND.into_response());
    };
    nmcli(&["connection", "delete", "uuid", uuid]).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profiles_are_wifi_only_without_the_hotspot() {
        let out = "preconfigured:1111:802-11-wireless\nWired connection 1:2222:802-3-ethernet\ncarchomp-hotspot:3333:802-11-wireless\nCaf\\:e:4444:802-11-wireless\n";
        assert_eq!(wifi_profiles(out), ["1111", "4444"]);
    }

    #[test]
    fn nmcli_errors_are_not_passed_through() {
        assert_eq!(explain("Error: Connection activation failed: Secrets were required, but not provided."), "wrong or missing password");
        assert_eq!(explain("Error: No network with SSID 'x' found."), "network not in range");
        assert_eq!(explain("Error: something about 192.168.1.1"), "NetworkManager could not do that (see the daemon log)");
    }
}
