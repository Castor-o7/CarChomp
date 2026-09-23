//! Parsing for eduroam CAT / geteduroam `.eap-config` files: which EAP method
//! to use, the CA certificates and server names that pin the RADIUS server,
//! and (for EAP-TLS) the client certificate.

use roxmltree::Node;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Tls,
    Peap,
    TtlsPap,
    /// TTLS with MSCHAPv2 as a plain inner method (non-EAP type 3)...
    TtlsMschapv2,
    /// ...and wrapped in EAP (type 26): not the same on the wire.
    TtlsEapMschapv2,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Profile {
    pub method: Method,
    /// One or more PEM CERTIFICATE blocks.
    pub ca_pem: String,
    pub server_names: Vec<String>,
    pub anonymous_identity: Option<String>,
    pub identity: Option<String>,
    /// PKCS#12 client certificate and key (EAP-TLS).
    pub client_p12: Option<Vec<u8>>,
    pub passphrase: Option<String>,
    pub ssids: Vec<String>,
}

/// Parse an `.eap-config` file, using the first authentication method in it
/// that we support (EAP-TLS only when the file carries the client certificate,
/// since the user has no other way to supply one here). A method without a CA
/// or a server name is refused: joining with it would not validate the RADIUS
/// server.
pub fn parse(xml: &str) -> Result<Profile, String> {
    let doc = roxmltree::Document::parse(xml.trim_start_matches('\u{feff}'))
        .map_err(|e| format!("not an eap-config file: {e}"))?;
    let provider = doc
        .descendants()
        .find(|n| is(n, "EAPIdentityProvider"))
        .ok_or("not an eap-config file: no EAPIdentityProvider")?;
    let (method, auth) = provider
        .descendants()
        .filter(|n| is(n, "AuthenticationMethod"))
        .find_map(|n| supported(n).map(|m| (m, n)))
        .ok_or("the file offers no supported method (EAP-TLS with its client certificate, PEAP/MSCHAPv2, TTLS/PAP or TTLS/MSCHAPv2)")?;

    let server = children(auth, "ServerSideCredential").collect::<Vec<_>>();
    let mut ca_pem = String::new();
    for ca in server.iter().flat_map(|s| children(*s, "CA")) {
        if attr_is(ca, "format", "X.509") && attr_is(ca, "encoding", "base64") {
            let der = base64_decode(text(ca)).ok_or("a CA certificate is not valid base64")?;
            ca_pem.push_str(&pem("CERTIFICATE", &der));
        }
    }
    if ca_pem.is_empty() {
        return Err(
            "the file has no CA certificate, so the network's server cannot be verified".into(),
        );
    }
    let server_names: Vec<String> = server
        .iter()
        .flat_map(|s| children(*s, "ServerID"))
        .map(|n| text(n).trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if server_names.is_empty() {
        return Err("the file has no ServerID, so the network's server cannot be verified".into());
    }

    let client = children(auth, "ClientSideCredential").next();
    let field = |tag| {
        client
            .and_then(|c| children(c, tag).next())
            .map(|n| text(n).trim().to_string())
            .filter(|s| !s.is_empty())
    };
    let client_p12 = match client_cert(auth) {
        Some(n) => {
            Some(base64_decode(text(n)).ok_or("the client certificate is not valid base64")?)
        }
        None => None,
    };

    let ssids = provider
        .descendants()
        .filter(|n| is(n, "CredentialApplicability"))
        .flat_map(|c| c.descendants().filter(|n| is(n, "IEEE80211")))
        .flat_map(|n| children(n, "SSID"))
        .map(|n| text(n).to_string())
        .filter(|s| !s.is_empty())
        .collect();

    Ok(Profile {
        method,
        ca_pem,
        server_names,
        anonymous_identity: field("OuterIdentity"),
        identity: field("UserName"),
        client_p12,
        passphrase: field("Passphrase"),
        ssids,
    })
}

fn supported(auth: Node) -> Option<Method> {
    let outer = eap_type(auth)?;
    let inner = children(auth, "InnerAuthenticationMethod").next();
    let inner_eap = inner.and_then(eap_type);
    let inner_non_eap = inner
        .and_then(|i| children(i, "NonEAPAuthMethod").next())
        .and_then(|m| children(m, "Type").next())
        .and_then(|t| text(t).trim().parse::<u32>().ok());
    match (outer, inner_eap, inner_non_eap) {
        (13, _, _) if client_cert(auth).is_some() => Some(Method::Tls),
        (25, Some(26), _) => Some(Method::Peap),
        (21, _, Some(1)) => Some(Method::TtlsPap),
        (21, Some(26), _) => Some(Method::TtlsEapMschapv2),
        (21, _, Some(3)) => Some(Method::TtlsMschapv2),
        _ => None,
    }
}

fn client_cert<'a, 'i>(auth: Node<'a, 'i>) -> Option<Node<'a, 'i>> {
    children(auth, "ClientSideCredential")
        .next()?
        .children()
        .find(|n| {
            is(n, "ClientCertificate")
                && attr_is(*n, "format", "PKCS12")
                && attr_is(*n, "encoding", "base64")
                && !text(*n).trim().is_empty()
        })
}

fn eap_type(node: Node) -> Option<u32> {
    let method = children(node, "EAPMethod").next()?;
    text(children(method, "Type").next()?).trim().parse().ok()
}

fn is(node: &Node, tag: &str) -> bool {
    node.is_element() && node.tag_name().name() == tag
}

fn children<'a, 'i>(node: Node<'a, 'i>, tag: &'static str) -> impl Iterator<Item = Node<'a, 'i>> {
    node.children().filter(move |n| is(n, tag))
}

fn text<'a>(node: Node<'a, '_>) -> &'a str {
    node.text().unwrap_or("")
}

/// Missing attributes count as matching: the defaults are X.509 / base64 in practice.
fn attr_is(node: Node, name: &str, value: &str) -> bool {
    node.attribute(name)
        .is_none_or(|v| v.trim().eq_ignore_ascii_case(value))
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard base64, ignoring whitespace; padding optional.
fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let (mut acc, mut bits) = (0u32, 0);
    let mut padded = false;
    for c in s.bytes().filter(|c| !c.is_ascii_whitespace()) {
        if c == b'=' {
            padded = true;
            continue;
        }
        if padded {
            return None;
        }
        let v = B64.iter().position(|&b| b == c)? as u32;
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    // Leftover bits must be zero padding, never a whole extra character.
    (bits < 6 && acc & ((1 << bits) - 1) == 0).then_some(out)
}

fn base64_encode(data: &[u8]) -> String {
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, &b)| n | ((b as u32) << (16 - 8 * i)));
        for i in 0..4 {
            out.push(if i <= chunk.len() {
                B64[(n >> (18 - 6 * i)) as usize & 63] as char
            } else {
                '='
            });
        }
    }
    out
}

fn pem(label: &str, der: &[u8]) -> String {
    let b64 = base64_encode(der);
    let mut out = format!("-----BEGIN {label}-----\n");
    for line in b64.as_bytes().chunks(64) {
        out.push_str(std::str::from_utf8(line).unwrap());
        out.push('\n');
    }
    out.push_str(&format!("-----END {label}-----\n"));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // Any bytes will do: nothing here parses the DER or the PKCS#12.
    const CA1: &str = "MIIBszCCAVmgAwIBAgIUQ2F0Q0ExAAAAAAAAAAAAAAAAAAAwCgYIKoZIzj0EAwIw\n  GjEYMBYGA1UEAwwPRXhhbXBsZSBSb290IENBMB4XDTI2MDEwMTAwMDAwMFoXDTM2\n  MDEwMTAwMDAwMFowGjEYMBYGA1UEAwwPRXhhbXBsZSBSb290IENB";
    const CA2: &str = "MIIBAjCBqaADAgECAgEBMAoGCCqGSM49BAMCMA8xDTALBgNVBAMMBENBIDI=";

    fn cat(methods: &str) -> String {
        format!(
            r#"<?xml version="1.0" encoding="utf-8"?>
<EAPIdentityProviderList xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance" xsi:noNamespaceSchemaLocation="eap-metadata.xsd">
  <EAPIdentityProvider ID="example.edu" namespace="urn:RFC4282:realm" lang="en" version="1">
    <ValidUntil>2030-01-01T00:00:00Z</ValidUntil>
    <AuthenticationMethods>{methods}
    </AuthenticationMethods>
    <CredentialApplicability>
      <IEEE80211><SSID>eduroam</SSID><MinRSNProto>CCMP</MinRSNProto></IEEE80211>
      <IEEE80211><ConsortiumOID>001bc50460</ConsortiumOID></IEEE80211>
      <IEEE80211><SSID>example-secure</SSID><MinRSNProto>CCMP</MinRSNProto></IEEE80211>
    </CredentialApplicability>
    <ProviderInfo><DisplayName>Example University</DisplayName></ProviderInfo>
  </EAPIdentityProvider>
</EAPIdentityProviderList>"#
        )
    }

    fn server() -> String {
        format!(
            r#"<ServerSideCredential>
          <CA format="X.509" encoding="base64">{CA1}</CA>
          <CA format="X.509" encoding="base64">{CA2}</CA>
          <ServerID>radius1.example.edu</ServerID>
          <ServerID>radius2.example.edu</ServerID>
        </ServerSideCredential>"#
        )
    }

    fn peap() -> String {
        format!(
            r#"
      <AuthenticationMethod>
        <EAPMethod><Type>25</Type></EAPMethod>
        {}
        <ClientSideCredential><OuterIdentity>anonymous@example.edu</OuterIdentity></ClientSideCredential>
        <InnerAuthenticationMethod><EAPMethod><Type>26</Type></EAPMethod></InnerAuthenticationMethod>
      </AuthenticationMethod>"#,
            server()
        )
    }

    #[test]
    fn cat_peap_with_two_cas_and_two_server_ids() {
        let p = parse(&cat(&peap())).unwrap();
        assert_eq!(p.method, Method::Peap);
        assert_eq!(p.ca_pem.matches("-----BEGIN CERTIFICATE-----\n").count(), 2);
        assert_eq!(p.ca_pem.matches("-----END CERTIFICATE-----\n").count(), 2);
        assert!(p.ca_pem.lines().all(|l| l.len() <= 64));
        let bodies: String = p
            .ca_pem
            .lines()
            .filter(|l| !l.starts_with("-----"))
            .collect();
        let expected: String = [CA1, CA2].concat().split_whitespace().collect();
        assert_eq!(bodies, expected);
        assert_eq!(
            p.server_names,
            ["radius1.example.edu", "radius2.example.edu"]
        );
        assert_eq!(
            p.anonymous_identity.as_deref(),
            Some("anonymous@example.edu")
        );
        assert_eq!(p.identity, None);
        assert_eq!(p.client_p12, None);
        assert_eq!(p.ssids, ["eduroam", "example-secure"]);
    }

    #[test]
    fn ttls_pap_and_mschapv2() {
        let ttls = |inner: &str| {
            cat(&format!(
                r#"<AuthenticationMethod><EAPMethod><Type>21</Type></EAPMethod>{}
                <InnerAuthenticationMethod>{inner}</InnerAuthenticationMethod></AuthenticationMethod>"#,
                server()
            ))
        };
        let pap = ttls("<NonEAPAuthMethod><Type>1</Type></NonEAPAuthMethod>");
        assert_eq!(parse(&pap).unwrap().method, Method::TtlsPap);
        let mschap = ttls("<NonEAPAuthMethod><Type>3</Type></NonEAPAuthMethod>");
        assert_eq!(parse(&mschap).unwrap().method, Method::TtlsMschapv2);
        let eap_mschap = ttls("<EAPMethod><Type>26</Type></EAPMethod>");
        assert_eq!(parse(&eap_mschap).unwrap().method, Method::TtlsEapMschapv2);
        assert!(parse(&ttls("<NonEAPAuthMethod><Type>2</Type></NonEAPAuthMethod>")).is_err());
    }

    #[test]
    fn geteduroam_tls_with_namespace_and_p12() {
        let p12 = (0..=255u8).collect::<Vec<_>>();
        let encoded = base64_encode(&p12);
        let wrapped = encoded
            .as_bytes()
            .chunks(76)
            .map(|c| std::str::from_utf8(c).unwrap())
            .collect::<Vec<_>>()
            .join("\n\t");
        let xml = format!(
            r#"<?xml version="1.0"?>
<eap:EAPIdentityProviderList xmlns:eap="urn:example:eap-metadata">
 <eap:EAPIdentityProvider ID="example.edu" namespace="urn:RFC4282:realm" version="1">
  <eap:AuthenticationMethods>
   <eap:AuthenticationMethod>
    <eap:EAPMethod><eap:Type>13</eap:Type></eap:EAPMethod>
    <eap:ServerSideCredential>
     <eap:CA format="X.509" encoding="base64">{CA2}</eap:CA>
     <eap:ServerID>radius.example.edu</eap:ServerID>
    </eap:ServerSideCredential>
    <eap:ClientSideCredential>
     <eap:OuterIdentity>anon@example.edu</eap:OuterIdentity>
     <eap:UserName>device-1234@example.edu</eap:UserName>
     <eap:ClientCertificate format="PKCS12" encoding="base64">
	{wrapped}
     </eap:ClientCertificate>
     <eap:Passphrase>s3cret</eap:Passphrase>
    </eap:ClientSideCredential>
   </eap:AuthenticationMethod>
  </eap:AuthenticationMethods>
  <eap:CredentialApplicability><eap:IEEE80211><eap:SSID>eduroam</eap:SSID></eap:IEEE80211></eap:CredentialApplicability>
 </eap:EAPIdentityProvider>
</eap:EAPIdentityProviderList>"#
        );
        let p = parse(&xml).unwrap();
        assert_eq!(p.method, Method::Tls);
        assert_eq!(p.client_p12, Some(p12));
        assert_eq!(p.passphrase.as_deref(), Some("s3cret"));
        assert_eq!(p.identity.as_deref(), Some("device-1234@example.edu"));
        assert_eq!(p.anonymous_identity.as_deref(), Some("anon@example.edu"));
        assert_eq!(p.server_names, ["radius.example.edu"]);
        assert_eq!(p.ssids, ["eduroam"]);
    }

    #[test]
    fn skips_unsupported_first_method() {
        let pwd = r#"
      <AuthenticationMethod>
        <EAPMethod><Type>52</Type></EAPMethod>
        <ServerSideCredential><ServerID>pwd.example.edu</ServerID></ServerSideCredential>
      </AuthenticationMethod>"#;
        let p = parse(&cat(&format!("{pwd}{}", peap()))).unwrap();
        assert_eq!(p.method, Method::Peap);
        assert_eq!(
            p.server_names,
            ["radius1.example.edu", "radius2.example.edu"]
        );
    }

    #[test]
    fn rejections() {
        let no_ca = peap()
            .replace(
                &format!(r#"<CA format="X.509" encoding="base64">{CA1}</CA>"#),
                "",
            )
            .replace(
                &format!(r#"<CA format="X.509" encoding="base64">{CA2}</CA>"#),
                "",
            );
        assert!(parse(&cat(&no_ca)).unwrap_err().contains("no CA"));

        let no_id = peap()
            .replace("<ServerID>radius1.example.edu</ServerID>", "")
            .replace("<ServerID>radius2.example.edu</ServerID>", "");
        assert!(parse(&cat(&no_id)).unwrap_err().contains("no ServerID"));

        let tls = format!(
            "<AuthenticationMethod><EAPMethod><Type>13</Type></EAPMethod>{}</AuthenticationMethod>",
            server()
        );
        assert!(
            parse(&cat(&tls))
                .unwrap_err()
                .contains("no supported method")
        );
        // EAP-TLS without a certificate in the file is skipped, not fatal.
        let p = parse(&cat(&format!("{tls}{}", peap()))).unwrap();
        assert_eq!(p.method, Method::Peap);
        assert_eq!(p.client_p12, None);

        let pwd_only =
            "<AuthenticationMethod><EAPMethod><Type>52</Type></EAPMethod></AuthenticationMethod>";
        assert!(
            parse(&cat(pwd_only))
                .unwrap_err()
                .contains("no supported method")
        );

        assert!(
            parse("ssid=eduroam\npassword=hunter2")
                .unwrap_err()
                .contains("not an eap-config")
        );
        assert!(parse("<gpx/>").unwrap_err().contains("not an eap-config"));
        let bad = peap().replace(CA2, "not*base64");
        assert!(parse(&cat(&bad)).unwrap_err().contains("base64"));
    }

    #[test]
    fn base64_round_trip() {
        for data in [&b""[..], b"f", b"fo", b"foo", b"foob", b"fooba", b"foobar"] {
            let e = base64_encode(data);
            assert_eq!(base64_decode(&e).as_deref(), Some(data));
        }
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(
            base64_decode(" Zm9v\n YmE= ").as_deref(),
            Some(&b"fooba"[..])
        );
        assert_eq!(base64_decode("Zm9vY"), None);
        assert_eq!(base64_decode("Zm=9v"), None);
    }
}
