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
use std::{process::Stdio, time::Duration};
use tokio::{io::AsyncWriteExt, process::Command, sync::Mutex};

/// The fallback access point's profile (deploy/install.sh). Not a network to
/// join or forget: deleting it would strand every client of the hotspot.
const HOTSPOT: &str = "carchomp-hotspot";

/// One radio, one NetworkManager: changes go one at a time, and a request
/// that finds one under way is turned away instead of queued.
static NMCLI: Mutex<()> = Mutex::const_new(());

/// Longest any nmcli call may take. Activations pass `--wait` (below this),
/// so NetworkManager gives up first.
const NMCLI_TIMEOUT: Duration = Duration::from_secs(60);
const WAIT: &str = "45";

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
    let child = Command::new("nmcli")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn();
    let mut child = match child {
        Ok(child) => child,
        Err(e) => return Err(not_run(e)),
    };
    let mut stdin = child.stdin.take().expect("piped");
    if let Some(input) = input {
        let _ = stdin.write_all(format!("{input}\n").as_bytes()).await;
    }
    drop(stdin);
    // Dropping the future on timeout drops the child, which kills it.
    match tokio::time::timeout(NMCLI_TIMEOUT, child.wait_with_output()).await {
        Err(_) => {
            tracing::warn!("nmcli {}: no answer in {} s", args.first().unwrap_or(&""), NMCLI_TIMEOUT.as_secs());
            Err((StatusCode::GATEWAY_TIMEOUT, "NetworkManager did not answer").into_response())
        }
        Ok(Ok(out)) if out.status.success() => Ok(String::from_utf8_lossy(&out.stdout).into_owned()),
        Ok(Ok(out)) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            tracing::warn!("nmcli {}: {}", args.first().unwrap_or(&""), stderr.trim());
            Err((StatusCode::BAD_GATEWAY, explain(&stderr)).into_response())
        }
        Ok(Err(e)) => Err(not_run(e)),
    }
}

/// Only a missing nmcli means NetworkManager is not there.
fn not_run(e: std::io::Error) -> Response {
    if e.kind() == std::io::ErrorKind::NotFound {
        (StatusCode::NOT_IMPLEMENTED, "NetworkManager is not available").into_response()
    } else {
        tracing::warn!("nmcli: {e}");
        (StatusCode::INTERNAL_SERVER_ERROR, "could not run nmcli (see the daemon log)").into_response()
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
/// read as an option, and control characters (NUL above all) cannot be
/// passed at all.
fn valid(ssid: &str) -> bool {
    !ssid.is_empty() && ssid.len() <= 32 && !ssid.starts_with('-') && !ssid.chars().any(char::is_control)
}

/// Whether wlan0 is hosting the hotspot, from `nmcli -t -f NAME,DEVICE
/// connection show --active`. While it is, the radio cannot scan.
fn hosting(output: &str) -> bool {
    output.lines().any(|line| matches!(&nm::fields(line)[..], [name, device] if name == HOTSPOT && device == "wlan0"))
}

/// The UUID in `nmcli connection add`'s "Connection 'x' (UUID) successfully
/// added." The name may itself hold parentheses, so take the last pair.
fn added_uuid(output: &str) -> Option<&str> {
    let (_, rest) = output.rsplit_once(" (")?;
    let (uuid, _) = rest.split_once(')')?;
    (!uuid.is_empty() && uuid.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-')).then_some(uuid)
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
    // Waits rather than turns away: a scan must not race a change.
    let _nmcli = NMCLI.lock().await;
    let networks = nm::wifi_list(&nmcli(&["-t", "-f", "ACTIVE,SSID,SIGNAL,SECURITY", "device", "wifi", "list"]).await?);
    let saved: Vec<String> = saved().await?.into_iter().map(|(ssid, _)| ssid).collect();
    let hotspot = hosting(&nmcli(&["-t", "-f", "NAME,DEVICE", "connection", "show", "--active"]).await?);
    Ok(Json(serde_json::json!({ "networks": networks, "saved": saved, "hotspot": hotspot })).into_response())
}

fn busy() -> Response {
    (StatusCode::CONFLICT, "another Wi-Fi change is under way").into_response()
}

#[derive(Deserialize)]
struct Join {
    ssid: String,
    password: Option<String>,
    /// WPA-Enterprise user name; joins with PEAP/MSCHAPv2.
    identity: Option<String>,
}

async fn connect(Json(join): Json<Join>) -> Result<StatusCode, Response> {
    let control = |s: &Option<String>| s.as_deref().is_some_and(|s| s.chars().any(char::is_control));
    // PEAP/MSCHAPv2 needs both halves.
    let bad_identity = join.identity.is_some() && join.password.as_deref().is_none_or(str::is_empty)
        || join.identity.as_deref().is_some_and(|i| i.is_empty() || i.starts_with('-'));
    if !valid(&join.ssid) || control(&join.password) || control(&join.identity) || bad_identity {
        return Err(StatusCode::BAD_REQUEST.into_response());
    }
    let Ok(_nmcli) = NMCLI.try_lock() else { return Err(busy()) };
    let saved = saved().await?;
    let profile = saved.iter().find(|(ssid, _)| *ssid == join.ssid).map(|(_, uuid)| uuid.as_str());
    if let Some(identity) = &join.identity {
        return enterprise(&join.ssid, identity, join.password.as_deref().unwrap_or_default(), profile).await;
    }
    match (&join.password, profile) {
        (None, Some(uuid)) => nmcli(&["--wait", WAIT, "connection", "up", "uuid", uuid]).await?,
        (None, None) => nmcli(&["--wait", WAIT, "device", "wifi", "connect", &join.ssid]).await?,
        (Some(password), _) => nmcli_with(&["--wait", WAIT, "--ask", "device", "wifi", "connect", &join.ssid], Some(password)).await?,
    };
    Ok(StatusCode::NO_CONTENT)
}

/// Join a WPA-Enterprise network with PEAP/MSCHAPv2, updating the saved
/// profile if there is one. Its old password is cleared so NetworkManager
/// asks for one, and `--ask` answers from standard input, as for WPA-PSK:
/// never on the argument list. NetworkManager keeps a password it had to ask
/// for. A profile made here is removed again if it does not connect.
async fn enterprise(ssid: &str, identity: &str, password: &str, profile: Option<&str>) -> Result<StatusCode, Response> {
    let settings = [
        "wifi-sec.key-mgmt",
        "wpa-eap",
        "802-1x.eap",
        "peap",
        "802-1x.phase2-auth",
        "mschapv2",
        "802-1x.identity",
        identity,
        "802-1x.system-ca-certs",
        "no",
        "802-1x.password",
        "",
    ];
    let (uuid, created) = match profile {
        Some(uuid) => {
            nmcli(&[&["connection", "modify", "uuid", uuid][..], &settings].concat()).await?;
            (uuid.to_owned(), false)
        }
        None => {
            let out = nmcli(&[&["connection", "add", "type", "wifi", "con-name", ssid, "ssid", ssid][..], &settings].concat()).await?;
            let Some(uuid) = added_uuid(&out) else {
                tracing::warn!("nmcli connection add: unexpected output {:?}", out.trim());
                return Err((StatusCode::BAD_GATEWAY, "NetworkManager could not do that (see the daemon log)").into_response());
            };
            (uuid.to_owned(), true)
        }
    };
    if let Err(response) = nmcli_with(&["--wait", WAIT, "--ask", "connection", "up", "uuid", &uuid], Some(password)).await {
        if created {
            let _ = nmcli(&["connection", "delete", "uuid", &uuid]).await;
        }
        return Err(response);
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn forget(Path(ssid): Path<String>) -> Result<StatusCode, Response> {
    if !valid(&ssid) {
        return Err(StatusCode::BAD_REQUEST.into_response());
    }
    let Ok(_nmcli) = NMCLI.try_lock() else { return Err(busy()) };
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

    #[test]
    fn ssids_without_control_characters() {
        assert!(valid("Caf\u{e9} home"));
        for bad in ["", "-x", "a\0b", "a\nb", &"x".repeat(33)] {
            assert!(!valid(bad), "{bad:?}");
        }
    }

    #[test]
    fn hosting_means_the_hotspot_on_wlan0() {
        assert!(hosting("Wired connection 1:eth0\ncarchomp-hotspot:wlan0\n"));
        assert!(!hosting("home:wlan0\n"));
        assert!(!hosting("carchomp-hotspot:wlan1\n"));
        assert!(!hosting(""));
    }

    #[test]
    fn reads_the_uuid_of_an_added_profile() {
        let out = "Connection 'eduroam (campus)' (0f5c2c3e-1b2a-4d7e-9f00-0123456789ab) successfully added.\n";
        assert_eq!(added_uuid(out), Some("0f5c2c3e-1b2a-4d7e-9f00-0123456789ab"));
        assert_eq!(added_uuid("Error: nope\n"), None);
    }
}
