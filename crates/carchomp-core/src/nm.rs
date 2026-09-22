//! Parsing for NetworkManager's `nmcli --terse` output.

use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Network {
    pub ssid: String,
    /// 0..=100
    pub signal: u8,
    pub secure: bool,
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
            active: active == "yes",
        };
        if network.ssid.is_empty() {
            continue;
        }
        match networks.iter_mut().find(|n| n.ssid == network.ssid) {
            Some(seen) => {
                seen.active |= network.active;
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
    }

    #[test]
    fn saved_profiles_are_wifi_only() {
        let out = "home:802-11-wireless\nWired connection 1:802-3-ethernet\ncarchomp-hotspot:802-11-wireless\n";
        assert_eq!(saved_wifi(out), ["home", "carchomp-hotspot"]);
    }
}
