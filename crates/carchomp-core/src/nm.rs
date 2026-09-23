//! Parsing for NetworkManager's `nmcli --terse` output, and the settings
//! carchompd gives the Wi-Fi profiles it makes.

use crate::eapconfig::{Method, Profile};
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Network {
    pub ssid: String,
    /// 0..=100
    pub signal: u8,
    pub secure: bool,
    /// WPA-Enterprise (802.1X): joining needs an identity as well as a password.
    pub enterprise: bool,
    pub active: bool,
}

/// Split one terse line into fields. Fields are colon-separated; literal
/// colons and backslashes inside a field are backslash-escaped.
pub fn fields(line: &str) -> Vec<String> {
    let mut fields = vec![String::new()];
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => fields.last_mut().unwrap().extend(chars.next()),
            ':' => fields.push(String::new()),
            c => fields.last_mut().unwrap().push(c),
        }
    }
    fields
}

/// Parse `nmcli -t -f ACTIVE,SSID,SIGNAL,SECURITY device wifi list`. An SSID
/// served by several access points is listed once, by its strongest signal;
/// hidden networks are skipped. Strongest first.
pub fn wifi_list(output: &str) -> Vec<Network> {
    let mut networks: Vec<Network> = Vec::new();
    for line in output.lines() {
        let [active, ssid, signal, security] = &fields(line)[..] else { continue };
        let network = Network {
            ssid: ssid.clone(),
            signal: signal.parse().unwrap_or(0),
            secure: !security.is_empty() && security != "--",
            enterprise: security.contains("802.1X"),
            active: active == "yes",
        };
        if network.ssid.is_empty() {
            continue;
        }
        match networks.iter_mut().find(|n| n.ssid == network.ssid) {
            Some(seen) => {
                seen.active |= network.active;
                seen.enterprise |= network.enterprise;
                seen.signal = seen.signal.max(network.signal);
            }
            None => networks.push(network),
        }
    }
    networks.sort_by_key(|n| (!n.active, std::cmp::Reverse(n.signal)));
    networks
}

/// Parse `nmcli -t -f NAME,TYPE connection show` into saved Wi-Fi profile names.
pub fn saved_wifi(output: &str) -> Vec<String> {
    output
        .lines()
        .filter_map(|line| match &fields(line)[..] {
            [name, kind] if kind == "802-11-wireless" => Some(name.clone()),
            _ => None,
        })
        .collect()
}

/// Every Wi-Fi client profile carchompd makes or changes retries forever
/// (NetworkManager gives up after 4 failures by default, fatal in a moving
/// car), and an enterprise network wins over others in range.
pub fn autoconnect(enterprise: bool) -> [&'static str; 4] {
    ["connection.autoconnect-retries", "0", "connection.autoconnect-priority", if enterprise { "10" } else { "0" }]
}

/// How an enterprise join checks that it talks to the real RADIUS server.
pub enum Server<'a> {
    /// An `.eap-config` file, its CA bundle (and EAP-TLS client certificate)
    /// saved at these paths.
    File { profile: &'a Profile, ca_cert: &'a str, client_cert: Option<&'a str> },
    /// No file: PEAP/MSCHAPv2, a server certificate from the system CAs for
    /// this domain.
    Domain(&'a str),
}

/// The realm of `user@realm`, if any.
pub fn realm(identity: &str) -> Option<&str> {
    identity.rsplit_once('@').map(|(_, realm)| realm).filter(|realm| !realm.is_empty())
}

/// `nmcli connection add|modify` arguments (property, value, ...) for an
/// enterprise join. Every 802.1X setting is given, empty when unused, so a
/// changed profile keeps nothing stale. Secrets are cleared: NetworkManager
/// then asks for them and `--ask` answers from standard input.
pub fn enterprise_settings(identity: &str, server: &Server) -> Vec<String> {
    let ((eap, phase2, phase2_eap), anonymous, ca_cert, domains, client_cert) = match server {
        Server::File { profile, ca_cert, client_cert } => {
            let method = match profile.method {
                Method::Tls => ("tls", "", ""),
                Method::Peap => ("peap", "mschapv2", ""),
                Method::TtlsPap => ("ttls", "pap", ""),
                Method::TtlsMschapv2 => ("ttls", "mschapv2", ""),
                Method::TtlsEapMschapv2 => ("ttls", "", "mschapv2"),
            };
            let anonymous = profile.anonymous_identity.clone().unwrap_or_default();
            (method, anonymous, *ca_cert, profile.server_names.join(";"), client_cert.unwrap_or_default())
        }
        Server::Domain(domain) => {
            let anonymous = realm(identity).map(|realm| format!("anonymous@{realm}")).unwrap_or_default();
            (("peap", "mschapv2", ""), anonymous, "", domain.to_string(), "")
        }
    };
    // The file's CA alone, or else the system's: never neither.
    let system_ca = if ca_cert.is_empty() { "yes" } else { "no" };
    let settings = [
        ("wifi-sec.key-mgmt", "wpa-eap"),
        ("802-1x.eap", eap),
        ("802-1x.phase2-auth", phase2),
        ("802-1x.phase2-autheap", phase2_eap),
        ("802-1x.identity", identity),
        ("802-1x.anonymous-identity", &anonymous),
        ("802-1x.ca-cert", ca_cert),
        ("802-1x.system-ca-certs", system_ca),
        ("802-1x.domain-suffix-match", &domains),
        // nmcli reads "path [password]": the path holds no spaces.
        ("802-1x.client-cert", client_cert),
        ("802-1x.private-key", client_cert),
        ("802-1x.password", ""),
        ("802-1x.private-key-password", ""),
    ];
    settings.iter().flat_map(|(k, v)| [k, *v]).chain(autoconnect(true)).map(str::to_string).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unescapes_fields() {
        assert_eq!(fields(r"yes:Caf\:e \\ bar:70:WPA2"), ["yes", r"Caf:e \ bar", "70", "WPA2"]);
        assert_eq!(fields("a::b"), ["a", "", "b"]);
    }

    #[test]
    fn lists_each_network_once_strongest_first() {
        let out = "no:home:40:WPA2\nyes:home:72:WPA2\nno::55:WPA2\nno:cafe:90:\nno:lab:80:--\ngarbage\n";
        let got = wifi_list(out);
        let summary: Vec<_> = got.iter().map(|n| (n.ssid.as_str(), n.signal, n.secure, n.active)).collect();
        assert_eq!(summary, [("home", 72, true, true), ("cafe", 90, false, false), ("lab", 80, false, false)]);
        assert!(got.iter().all(|n| !n.enterprise));
    }

    #[test]
    fn flags_enterprise_networks() {
        let got = wifi_list("no:eduroam:60:WPA2 802.1X\nno:home:50:WPA2\n");
        let summary: Vec<_> = got.iter().map(|n| (n.ssid.as_str(), n.secure, n.enterprise)).collect();
        assert_eq!(summary, [("eduroam", true, true), ("home", true, false)]);
    }

    #[test]
    fn saved_profiles_are_wifi_only() {
        let out = "home:802-11-wireless\nWired connection 1:802-3-ethernet\ncarchomp-hotspot:802-11-wireless\n";
        assert_eq!(saved_wifi(out), ["home", "carchomp-hotspot"]);
    }

    fn setting<'a>(settings: &'a [String], key: &str) -> &'a str {
        let i = settings.iter().position(|s| s == key).unwrap_or_else(|| panic!("no {key}"));
        &settings[i + 1]
    }

    fn file(method: Method) -> Profile {
        Profile {
            method,
            ca_pem: "-----BEGIN CERTIFICATE-----\n".into(),
            server_names: vec!["radius.example.edu".into(), "radius2.example.edu".into()],
            anonymous_identity: Some("anonymous@example.edu".into()),
            identity: None,
            client_p12: None,
            passphrase: None,
            ssids: vec!["eduroam".into()],
        }
    }

    #[test]
    fn realms() {
        assert_eq!(realm("prof@pdx.edu"), Some("pdx.edu"));
        assert_eq!(realm("a@b@c.org"), Some("c.org"));
        assert_eq!(realm("prof"), None);
        assert_eq!(realm("prof@"), None);
    }

    #[test]
    fn domain_join_validates_against_the_system_cas() {
        let s = enterprise_settings("prof@pdx.edu", &Server::Domain("radius.pdx.edu"));
        assert_eq!(s.len() % 2, 0);
        let want = [
            ("wifi-sec.key-mgmt", "wpa-eap"),
            ("802-1x.eap", "peap"),
            ("802-1x.phase2-auth", "mschapv2"),
            ("802-1x.identity", "prof@pdx.edu"),
            ("802-1x.anonymous-identity", "anonymous@pdx.edu"),
            ("802-1x.ca-cert", ""),
            ("802-1x.system-ca-certs", "yes"),
            ("802-1x.domain-suffix-match", "radius.pdx.edu"),
            ("802-1x.client-cert", ""),
            ("802-1x.password", ""),
            ("connection.autoconnect-retries", "0"),
            ("connection.autoconnect-priority", "10"),
        ];
        for (k, v) in want {
            assert_eq!(setting(&s, k), v, "{k}");
        }
        let s = enterprise_settings("prof", &Server::Domain("radius.pdx.edu"));
        assert_eq!(setting(&s, "802-1x.anonymous-identity"), "");
    }

    #[test]
    fn file_join_pins_its_ca_and_server_names() {
        let peap = file(Method::Peap);
        let s = enterprise_settings("prof@example.edu", &Server::File { profile: &peap, ca_cert: "/d/ca.pem", client_cert: None });
        for (k, v) in [
            ("802-1x.eap", "peap"),
            ("802-1x.phase2-auth", "mschapv2"),
            ("802-1x.anonymous-identity", "anonymous@example.edu"),
            ("802-1x.ca-cert", "/d/ca.pem"),
            ("802-1x.system-ca-certs", "no"),
            ("802-1x.domain-suffix-match", "radius.example.edu;radius2.example.edu"),
            ("802-1x.private-key", ""),
        ] {
            assert_eq!(setting(&s, k), v, "{k}");
        }
        for (method, eap, phase2, phase2_eap) in [
            (Method::TtlsPap, "ttls", "pap", ""),
            (Method::TtlsMschapv2, "ttls", "mschapv2", ""),
            (Method::TtlsEapMschapv2, "ttls", "", "mschapv2"),
        ] {
            let p = file(method);
            let s = enterprise_settings("u", &Server::File { profile: &p, ca_cert: "/d/ca.pem", client_cert: None });
            let got = (setting(&s, "802-1x.eap"), setting(&s, "802-1x.phase2-auth"), setting(&s, "802-1x.phase2-autheap"));
            assert_eq!(got, (eap, phase2, phase2_eap), "{method:?}");
        }
        let tls = Profile { anonymous_identity: None, ..file(Method::Tls) };
        let s = enterprise_settings("dev@example.edu", &Server::File { profile: &tls, ca_cert: "/d/ca.pem", client_cert: Some("/d/client.p12") });
        for (k, v) in [
            ("802-1x.eap", "tls"),
            ("802-1x.phase2-auth", ""),
            ("802-1x.anonymous-identity", ""),
            ("802-1x.client-cert", "/d/client.p12"),
            ("802-1x.private-key", "/d/client.p12"),
            ("802-1x.private-key-password", ""),
        ] {
            assert_eq!(setting(&s, k), v, "{k}");
        }
    }

    #[test]
    fn client_profiles_retry_forever() {
        assert_eq!(autoconnect(false), ["connection.autoconnect-retries", "0", "connection.autoconnect-priority", "0"]);
    }
}
