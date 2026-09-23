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
use carchomp_core::{
    eapconfig::{self, Method, Profile},
    nm,
};
use serde::Deserialize;
use std::{io, path::PathBuf, process::Stdio, time::Duration};
use tokio::{io::AsyncWriteExt, process::Command, sync::Mutex};

/// The fallback access point's profile (deploy/install.sh). Not a network to
/// join or forget: deleting it would strand every client of the hotspot.
const HOTSPOT: &str = "carchomp-hotspot";

/// One radio, one NetworkManager: one nmcli conversation at a time. A change
/// waits out a scan but is turned away if another change holds the radio.
static NMCLI: Mutex<()> = Mutex::const_new(());

/// Longest any nmcli call may take. Activations pass `--wait` (below this),
/// so NetworkManager gives up first.
const NMCLI_TIMEOUT: Duration = Duration::from_secs(60);
/// How long a change waits for the lock: long enough for a scan (the UI
/// polls the list), not for another change.
const CHANGE_WAIT: Duration = Duration::from_secs(15);
const WAIT: &str = "45";

/// Where the CA bundle and client certificate of an `.eap-config` join live,
/// one directory per SSID (see [`files_dir`]).
const FILES: &str = "/var/lib/carchomp/wifi";

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

/// The UUID in `nmcli device wifi connect`'s "Device 'wlan0' successfully
/// activated with 'UUID'."
fn activated_uuid(output: &str) -> Option<&str> {
    let (_, rest) = output.rsplit_once(" with '")?;
    let (uuid, _) = rest.split_once('\'')?;
    (!uuid.is_empty() && uuid.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-')).then_some(uuid)
}

/// The directory for an SSID's files: bytes other than ASCII letters, digits,
/// `-` and `_` become `%XX`, so no two SSIDs share one, and the path has no
/// space or comma (nmcli reads a private key as "path [password]").
fn files_dir(ssid: &str) -> PathBuf {
    let mut name = String::new();
    for b in ssid.bytes() {
        if b.is_ascii_alphanumeric() || b == b'-' || b == b'_' {
            name.push(b as char);
        } else {
            name.push_str(&format!("%{b:02X}"));
        }
    }
    PathBuf::from(FILES).join(name)
}

/// Write an `.eap-config` join's CA bundle and (EAP-TLS) client certificate
/// into `dir`, readable by us and NetworkManager (root) only. Returns their
/// paths.
fn save_files(dir: &std::path::Path, profile: &Profile) -> io::Result<(String, Option<String>)> {
    use std::{
        io::Write,
        os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    };
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    let write = |name: &str, data: &[u8]| -> io::Result<String> {
        let path = dir.join(name);
        // A fresh file, so it has the mode below whatever was there before.
        match std::fs::remove_file(&path) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
        std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&path)?.write_all(data)?;
        path.into_os_string().into_string().map_err(|_| io::ErrorKind::InvalidData.into())
    };
    let ca = write("ca.pem", profile.ca_pem.as_bytes())?;
    let client = match (&profile.client_p12, profile.method) {
        (Some(p12), Method::Tls) => Some(write("client.p12", p12)?),
        _ => None,
    };
    Ok((ca, client))
}

/// Remove an SSID's files, if it has any.
fn remove_files(ssid: &str) {
    match std::fs::remove_dir_all(files_dir(ssid)) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => tracing::warn!("removing {}: {e}", files_dir(ssid).display()),
        _ => {}
    }
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
    /// WPA-Enterprise user name.
    identity: Option<String>,
    /// The text of an eduroam CAT / geteduroam `.eap-config` file: the
    /// method, and the CA and server names that identify the RADIUS server.
    eap_config: Option<String>,
    /// Without a file: the RADIUS server's domain, checked against the
    /// system CAs.
    domain: Option<String>,
}

/// A checked enterprise join: who, the secret for `--ask` (password, or the
/// EAP-TLS key's passphrase), and how the server is checked.
struct Enterprise {
    identity: String,
    secret: String,
    server: EnterpriseServer,
}

/// By hand, so that no `{:?}` can print the secret.
impl std::fmt::Debug for Enterprise {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.debug_struct("Enterprise")
            .field("identity", &self.identity)
            .field("secret", &"<redacted>")
            .field("server", &self.server)
            .finish()
    }
}

#[derive(Debug)]
enum EnterpriseServer {
    File(Profile),
    Domain(String),
}

/// Check an enterprise join. There is no way to join without checking the
/// server: a file must name its CA and server (eapconfig refuses it
/// otherwise), and without one a domain is required. `Err` says why not.
fn enterprise_join(join: &Join) -> Result<Enterprise, String> {
    // Values go on nmcli's argument list, or (secrets) one line of its input.
    let arg = |s: &str| !s.is_empty() && !s.starts_with('-') && !s.chars().any(char::is_control);
    let password = join.password.clone().unwrap_or_default();
    let file = join.eap_config.as_deref().map(eapconfig::parse).transpose()?;
    let body_identity = join.identity.clone().filter(|i| !i.is_empty());
    let identity = match &file {
        // The certificate's own identity, when the file has it.
        Some(p) if p.method == Method::Tls => p.identity.clone().or(body_identity),
        Some(p) => body_identity.or(p.identity.clone()),
        None => body_identity,
    };
    let identity = identity.ok_or("a username is needed")?;
    if !arg(&identity) {
        return Err("that username cannot be used".into());
    }
    let (secret, server) = match file {
        Some(p) => {
            // NetworkManager reads domain-suffix-match as a ';'-separated list.
            if !p.server_names.iter().all(|n| server_name(n)) {
                return Err("the profile has a server name (ServerID) that cannot be used".into());
            }
            if p.anonymous_identity.as_deref().is_some_and(|a| !arg(a)) {
                return Err("the profile has an outer identity that cannot be used".into());
            }
            let secret = match p.method {
                Method::Tls => p.passphrase.clone().unwrap_or(password),
                _ if password.is_empty() => return Err("a password is needed".into()),
                _ => password,
            };
            if secret.chars().any(char::is_control) {
                return Err("the profile's passphrase cannot be used".into());
            }
            (secret, EnterpriseServer::File(p))
        }
        None => {
            let domain = join.domain.as_deref().unwrap_or_default().trim();
            if domain.is_empty() {
                return Err("the server domain is needed to check the network's server (or load the institution's profile)".into());
            }
            let label = |l: &str| !l.is_empty() && !l.starts_with('-') && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-');
            if !domain.contains('.') || !domain.split('.').all(label) {
                return Err("that is not a server domain (e.g. radius.example.edu)".into());
            }
            if password.is_empty() {
                return Err("a password is needed".into());
            }
            (password, EnterpriseServer::Domain(domain.to_string()))
        }
    };
    Ok(Enterprise { identity, secret, server })
}

/// A server name for `802-1x.domain-suffix-match`: one entry of its
/// `;`-separated list, and not something nmcli could take for an option.
fn server_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('-')
        && !name.chars().any(|c| c == ';' || c == ',' || c.is_whitespace() || c.is_control())
}

async fn connect(Json(join): Json<Join>) -> Result<StatusCode, Response> {
    let control = |s: &Option<String>| s.as_deref().is_some_and(|s| s.chars().any(char::is_control));
    if !valid(&join.ssid) || control(&join.password) || control(&join.identity) || control(&join.domain) {
        return Err(StatusCode::BAD_REQUEST.into_response());
    }
    let enterprise_as = if join.identity.is_some() || join.eap_config.is_some() || join.domain.is_some() {
        Some(enterprise_join(&join).map_err(|why| (StatusCode::BAD_REQUEST, why).into_response())?)
    } else {
        None
    };
    let Ok(_nmcli) = tokio::time::timeout(CHANGE_WAIT, NMCLI.lock()).await else { return Err(busy()) };
    let saved = saved().await?;
    let profile = saved.iter().find(|(ssid, _)| *ssid == join.ssid).map(|(_, uuid)| uuid.as_str());
    if let Some(join_as) = enterprise_as {
        return enterprise(&join.ssid, join_as, profile).await;
    }
    let out = match (&join.password, profile) {
        (None, Some(uuid)) => return nmcli(&["--wait", WAIT, "connection", "up", "uuid", uuid]).await.map(|_| StatusCode::NO_CONTENT),
        (None, None) => nmcli(&["--wait", WAIT, "device", "wifi", "connect", &join.ssid]).await?,
        (Some(password), _) => nmcli_with(&["--wait", WAIT, "--ask", "device", "wifi", "connect", &join.ssid], Some(password)).await?,
    };
    // Connected; a profile that then does not retry forever is logged, not an error.
    match activated_uuid(&out) {
        Some(uuid) => _ = nmcli(&[&["connection", "modify", "uuid", uuid][..], &nm::autoconnect(false)].concat()).await,
        None => tracing::warn!("nmcli device wifi connect: unexpected output {:?}", out.trim()),
    }
    Ok(StatusCode::NO_CONTENT)
}

/// Join a WPA-Enterprise network, updating the saved profile if there is
/// one. Its old secrets are cleared so NetworkManager asks for them, and
/// `--ask` answers from standard input, as for WPA-PSK: never on the
/// argument list. NetworkManager keeps a secret it had to ask for. A profile
/// made here is removed again (with its files) if it does not connect.
async fn enterprise(ssid: &str, join: Enterprise, profile: Option<&str>) -> Result<StatusCode, Response> {
    let dir = files_dir(ssid);
    let settings = match &join.server {
        EnterpriseServer::File(p) => {
            let (ca_cert, client_cert) = save_files(&dir, p).map_err(|e| {
                tracing::warn!("saving Wi-Fi files in {}: {e}", dir.display());
                (StatusCode::INTERNAL_SERVER_ERROR, "could not save the profile's certificates (see the daemon log)").into_response()
            })?;
            let server = nm::Server::File { profile: p, ca_cert: &ca_cert, client_cert: client_cert.as_deref() };
            nm::enterprise_settings(&join.identity, &server)
        }
        EnterpriseServer::Domain(domain) => nm::enterprise_settings(&join.identity, &nm::Server::Domain(domain)),
    };
    let settings: Vec<&str> = settings.iter().map(String::as_str).collect();
    let (uuid, created) = match profile {
        Some(uuid) => {
            nmcli(&[&["connection", "modify", "uuid", uuid][..], &settings].concat()).await?;
            (uuid.to_owned(), false)
        }
        None => {
            let out = match nmcli(&[&["connection", "add", "type", "wifi", "con-name", ssid, "ssid", ssid][..], &settings].concat()).await {
                Ok(out) => out,
                Err(response) => {
                    remove_files(ssid);
                    return Err(response);
                }
            };
            let Some(uuid) = added_uuid(&out) else {
                tracing::warn!("nmcli connection add: unexpected output {:?}", out.trim());
                remove_files(ssid);
                return Err((StatusCode::BAD_GATEWAY, "NetworkManager could not do that (see the daemon log)").into_response());
            };
            (uuid.to_owned(), true)
        }
    };
    if matches!(join.server, EnterpriseServer::Domain(_)) {
        // The profile no longer points at any.
        remove_files(ssid);
    }
    if let Err(response) = nmcli_with(&["--wait", WAIT, "--ask", "connection", "up", "uuid", &uuid], Some(&join.secret)).await {
        if created {
            let _ = nmcli(&["connection", "delete", "uuid", &uuid]).await;
            remove_files(ssid);
        }
        return Err(response);
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn forget(Path(ssid): Path<String>) -> Result<StatusCode, Response> {
    if !valid(&ssid) {
        return Err(StatusCode::BAD_REQUEST.into_response());
    }
    let Ok(_nmcli) = tokio::time::timeout(CHANGE_WAIT, NMCLI.lock()).await else { return Err(busy()) };
    let saved = saved().await?;
    let Some((_, uuid)) = saved.iter().find(|(saved, _)| *saved == ssid) else {
        return Err(StatusCode::NOT_FOUND.into_response());
    };
    nmcli(&["connection", "delete", "uuid", uuid]).await?;
    remove_files(&ssid);
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

    #[test]
    fn reads_the_uuid_of_an_activated_profile() {
        let out = "Device 'wlan0' successfully activated with '0f5c2c3e-1b2a-4d7e-9f00-0123456789ab'.\n";
        assert_eq!(activated_uuid(out), Some("0f5c2c3e-1b2a-4d7e-9f00-0123456789ab"));
        assert_eq!(activated_uuid("Error: nope\n"), None);
    }

    #[test]
    fn one_plain_directory_per_ssid() {
        assert_eq!(files_dir("eduroam"), PathBuf::from("/var/lib/carchomp/wifi/eduroam"));
        assert_eq!(files_dir("Caf\u{e9} a,b"), PathBuf::from("/var/lib/carchomp/wifi/Caf%C3%A9%20a%2Cb"));
        assert_eq!(files_dir("../x"), PathBuf::from("/var/lib/carchomp/wifi/%2E%2E%2Fx"));
        // Escaping is one-to-one: "a b" and "a%20b" differ.
        assert_ne!(files_dir("a b"), files_dir("a%20b"));
    }

    #[test]
    fn saves_private_files() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("carchomp-wifi-test-{}", std::process::id())).join("net");
        let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        let mut p = eapconfig::parse(&xml("25", "<EAPMethod><Type>26</Type></EAPMethod>")).unwrap();
        p.client_p12 = Some(vec![1, 2, 3]);
        let (ca, client) = save_files(&dir, &p).unwrap();
        assert_eq!(std::fs::read_to_string(&ca).unwrap(), p.ca_pem);
        assert_eq!(client, None, "PEAP needs no client certificate");
        assert_eq!((mode(&dir), mode(ca.as_ref())), (0o700, 0o600));
        p.method = Method::Tls;
        let (_, client) = save_files(&dir, &p).unwrap();
        let client = client.unwrap();
        assert_eq!((std::fs::read(&client).unwrap(), mode(client.as_ref())), (vec![1, 2, 3], 0o600));
        std::fs::remove_dir_all(dir.parent().unwrap()).unwrap();
    }

    fn xml(outer: &str, inner: &str) -> String {
        format!(
            r#"<EAPIdentityProviderList><EAPIdentityProvider><AuthenticationMethods><AuthenticationMethod>
            <EAPMethod><Type>{outer}</Type></EAPMethod>
            <ServerSideCredential><CA format="X.509" encoding="base64">MIIBAjCBqaADAgECAgEBMAoGCCqGSM49BAMCMA8xDTALBgNVBAMMBENBIDI=</CA><ServerID>radius.example.edu</ServerID></ServerSideCredential>
            <ClientSideCredential><OuterIdentity>anonymous@example.edu</OuterIdentity></ClientSideCredential>
            <InnerAuthenticationMethod>{inner}</InnerAuthenticationMethod>
            </AuthenticationMethod></AuthenticationMethods></EAPIdentityProvider></EAPIdentityProviderList>"#
        )
    }

    fn join(body: serde_json::Value) -> Result<Enterprise, String> {
        enterprise_join(&serde_json::from_value(body).unwrap())
    }

    #[test]
    fn enterprise_joins_always_check_the_server() {
        let ok = join(serde_json::json!({ "ssid": "eduroam", "identity": "prof@pdx.edu", "password": "pw", "domain": " radius.pdx.edu " })).unwrap();
        assert_eq!((ok.identity.as_str(), ok.secret.as_str()), ("prof@pdx.edu", "pw"));
        assert!(matches!(ok.server, EnterpriseServer::Domain(d) if d == "radius.pdx.edu"));
        for bad in [
            serde_json::json!({ "ssid": "e", "identity": "prof", "password": "pw" }),
            serde_json::json!({ "ssid": "e", "identity": "prof", "password": "pw", "domain": "" }),
            serde_json::json!({ "ssid": "e", "identity": "prof", "password": "pw", "domain": "a;b.edu" }),
            serde_json::json!({ "ssid": "e", "identity": "prof", "password": "pw", "domain": "-x.edu" }),
            serde_json::json!({ "ssid": "e", "identity": "prof", "password": "pw", "domain": "localhost" }),
            serde_json::json!({ "ssid": "e", "identity": "prof", "password": "pw", "domain": "a,b.edu" }),
            serde_json::json!({ "ssid": "e", "identity": "prof", "password": "pw", "domain": "radius pdx.edu" }),
            serde_json::json!({ "ssid": "e", "identity": "prof", "domain": "radius.pdx.edu" }),
            serde_json::json!({ "ssid": "e", "identity": "", "password": "pw", "domain": "radius.pdx.edu" }),
            serde_json::json!({ "ssid": "e", "identity": "-o", "password": "pw", "domain": "radius.pdx.edu" }),
            serde_json::json!({ "ssid": "e", "password": "pw", "domain": "radius.pdx.edu" }),
        ] {
            assert!(join(bad.clone()).is_err(), "{bad}");
        }
    }

    #[test]
    fn enterprise_joins_with_a_profile() {
        let peap = xml("25", "<EAPMethod><Type>26</Type></EAPMethod>");
        let ok = join(serde_json::json!({ "ssid": "eduroam", "identity": "prof@example.edu", "password": "pw", "eap_config": peap })).unwrap();
        assert!(matches!(&ok.server, EnterpriseServer::File(p) if p.method == Method::Peap));
        assert_eq!(ok.secret, "pw");
        // PEAP needs the password; the domain field does not stand in for the file's server check.
        assert!(join(serde_json::json!({ "ssid": "eduroam", "identity": "prof", "eap_config": peap })).is_err());
        let no_server = peap.replace("<ServerID>radius.example.edu</ServerID>", "");
        let why = join(serde_json::json!({ "ssid": "e", "identity": "p", "password": "pw", "eap_config": no_server, "domain": "x.edu" }))
            .unwrap_err();
        assert!(why.contains("ServerID"), "{why}");
        for bad in ["a.edu;b.edu", "a.edu,b.edu", "a.edu b.edu", "-radius.example.edu"] {
            let file = peap.replace("radius.example.edu<", &format!("{bad}<"));
            let why = join(serde_json::json!({ "ssid": "e", "identity": "p", "password": "pw", "eap_config": file })).unwrap_err();
            assert!(why.contains("ServerID"), "{bad}: {why}");
        }
    }

    #[test]
    fn debug_hides_the_secret() {
        let ok = join(serde_json::json!({ "ssid": "e", "identity": "prof", "password": "hunter2", "domain": "radius.pdx.edu" })).unwrap();
        let shown = format!("{ok:?}");
        assert!(!shown.contains("hunter2") && shown.contains("<redacted>"), "{shown}");
    }

    #[test]
    fn eap_tls_uses_the_files_identity_and_passphrase() {
        let tls = xml("13", "").replace(
            "<OuterIdentity>",
            "<UserName>dev@example.edu</UserName><Passphrase>secret</Passphrase><ClientCertificate format=\"PKCS12\" encoding=\"base64\">AQID</ClientCertificate><OuterIdentity>",
        );
        let ok = join(serde_json::json!({ "ssid": "eduroam", "identity": "typed", "eap_config": tls })).unwrap();
        assert_eq!((ok.identity.as_str(), ok.secret.as_str()), ("dev@example.edu", "secret"));
        let bare = tls.replace("<UserName>dev@example.edu</UserName><Passphrase>secret</Passphrase>", "");
        let ok = join(serde_json::json!({ "ssid": "eduroam", "identity": "typed", "password": "pw", "eap_config": bare })).unwrap();
        assert_eq!((ok.identity.as_str(), ok.secret.as_str()), ("typed", "pw"));
    }
}
