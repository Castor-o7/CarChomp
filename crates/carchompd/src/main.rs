mod api;
mod maps;
mod recorder;
mod sources;
mod system;
mod update;
mod wifi;

use carchomp_core::{Observation, beacon};
use serde::{Deserialize, Serialize};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use std::{io, net::SocketAddr, path::PathBuf, sync::Arc, time::Duration};
use tokio::{
    net::TcpListener,
    sync::{broadcast, watch},
};

/// What flows over the bus and out of the WebSocket: an observation and
/// the id of the source that made it.
#[derive(Debug, Clone, Serialize)]
pub struct Event {
    pub source: i16,
    pub obs: Observation,
}

pub type Bus = broadcast::Sender<Event>;

/// What the recorder is doing. Unlike events this is state, not a stream:
/// every WebSocket client gets it on connect and again whenever it changes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize)]
pub struct Status {
    /// The track being recorded, if any.
    pub track: Option<i64>,
    /// Whether the road under us has never been driven. `None` until known.
    pub road_new: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub database_url: String,
    /// One address or a list of them. One not assigned yet (the hotspot's,
    /// while it is down) is bound once it appears.
    #[serde(deserialize_with = "one_or_many")]
    pub listen: Vec<SocketAddr>,
    /// Directory of the built UI, served at `/`.
    pub ui_dir: Option<PathBuf>,
    /// Where offline map archives (`*.pmtiles`) and their fonts live.
    pub maps_dir: PathBuf,
    /// Remote PMTiles archive that regions are cut from. When unset, the UI
    /// looks up the latest Protomaps daily build.
    pub map_source: Option<String>,
    /// `host:port` of gpsd. Unset disables the source.
    pub gpsd: Option<String>,
    /// `host:port` of a KISS TNC such as Direwolf. Unset disables the source.
    pub aprs_kiss: Option<String>,
    pub beacon: beacon::Params,
    /// A track ends after this many seconds without movement.
    pub track_idle: f64,
    /// A road is "known" if an earlier track passes within this many metres.
    pub road_radius: f64,
    /// ...and runs within this many degrees of our heading, either direction.
    pub road_heading: f64,
    /// Networks (CIDR) allowed to use the web UI and API, besides loopback,
    /// which always is. The default is NetworkManager's hotspot subnet.
    pub trusted_networks: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            database_url: "postgres://carchomp@localhost/carchomp".into(),
            listen: vec![([0, 0, 0, 0], 8000).into()],
            ui_dir: None,
            maps_dir: "/var/lib/carchomp/maps".into(),
            map_source: None,
            gpsd: None,
            aprs_kiss: None,
            beacon: beacon::Params::default(),
            track_idle: 300.0,
            road_radius: 25.0,
            road_heading: 35.0,
            trusted_networks: vec!["10.42.0.0/24".into()],
        }
    }
}

fn one_or_many<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<SocketAddr>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged, expecting = "an address such as \"127.0.0.1:80\" or a list of them")]
    enum Listen {
        One(SocketAddr),
        Many(Vec<SocketAddr>),
    }
    match Listen::deserialize(d)? {
        Listen::One(addr) => Ok(vec![addr]),
        Listen::Many(list) if list.is_empty() => Err(serde::de::Error::custom("listen: no addresses")),
        Listen::Many(list) => Ok(list),
    }
}

/// A wildcard address already takes its port on every address of its family,
/// and Linux refuses to bind another address of that family and port beside
/// it, so those are left to the wildcard.
fn to_bind(listen: &[SocketAddr]) -> Vec<SocketAddr> {
    let covered = |a: &SocketAddr| {
        !a.ip().is_unspecified()
            && listen.iter().any(|w| w.ip().is_unspecified() && w.port() == a.port() && w.is_ipv4() == a.is_ipv4())
    };
    listen.iter().filter(|a| !covered(a)).copied().collect()
}

/// How often an address that is not assigned yet is tried again.
const REBIND: Duration = Duration::from_secs(5);

fn not_assigned(e: &io::Error) -> bool {
    rustix::io::Errno::from_io_error(e) == Some(rustix::io::Errno::ADDRNOTAVAIL)
}

/// Serve `app` on `addr` until `stop` changes. Without a listener, the address
/// was not assigned at startup: wait for it in the background.
async fn serve(addr: SocketAddr, listener: Option<TcpListener>, app: axum::Router, mut stop: watch::Receiver<()>) {
    let listener = match listener {
        Some(listener) => listener,
        None => {
            tracing::info!("{addr} is not assigned yet; will listen there once it is");
            loop {
                tokio::select! {
                    _ = tokio::time::sleep(REBIND) => {}
                    _ = stop.changed() => return,
                }
                match TcpListener::bind(addr).await {
                    Ok(listener) => break listener,
                    Err(e) if not_assigned(&e) => {}
                    Err(e) => return tracing::error!("cannot listen on {addr}: {e}"),
                }
            }
        }
    };
    tracing::info!("listening on {addr}");
    let served = axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())
        .with_graceful_shutdown(async move {
            stop.changed().await.ok();
        })
        .await;
    if let Err(e) = served {
        tracing::error!("serving {addr}: {e}");
    }
}

/// Wait for SIGINT (Ctrl-C) or SIGTERM (how systemd stops us).
async fn stopped() {
    use tokio::signal::unix::{SignalKind, signal};
    match signal(SignalKind::terminate()) {
        Ok(mut term) => tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        },
        Err(e) => {
            tracing::warn!("cannot catch SIGTERM: {e}");
            tokio::signal::ctrl_c().await.ok();
        }
    }
    tracing::info!("stopping");
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    // usage: carchompd [config.toml]; DATABASE_URL overrides the file.
    let mut config: Config = match std::env::args().nth(1) {
        Some(path) => toml::from_str(&std::fs::read_to_string(path)?)?,
        None => Config::default(),
    };
    if let Ok(url) = std::env::var("DATABASE_URL") {
        config.database_url = url;
    }

    let networks = api::networks(&config.trusted_networks).map_err(anyhow::Error::msg)?;

    // The recorder gets connections of its own, so however slow the
    // queries web clients ask for, they cannot hold up recording.
    let recording = PgPoolOptions::new()
        .max_connections(2)
        .connect(&config.database_url)
        .await?;
    sqlx::migrate!().run(&recording).await?;
    let db = PgPoolOptions::new()
        .max_connections(4)
        .connect_with(config.database_url.parse::<PgConnectOptions>()?.options([("statement_timeout", "10s")]))
        .await?;

    let (bus, _) = broadcast::channel(256);

    if let Some(addr) = config.gpsd.clone() {
        let source = recorder::source_id(&recording, "gps", &addr).await?;
        tokio::spawn(sources::gpsd(addr, source, bus.clone()));
    }
    if let Some(addr) = config.aprs_kiss.clone() {
        let source = recorder::source_id(&recording, "aprs", &addr).await?;
        tokio::spawn(sources::aprs_kiss(addr, source, bus.clone()));
    }
    let (status, _) = watch::channel(Status::default());
    tokio::spawn(recorder::run(recording, bus.clone(), status.clone(), &config));

    let mut listeners = Vec::new();
    for addr in to_bind(&config.listen) {
        match TcpListener::bind(addr).await {
            Ok(listener) => listeners.push((addr, Some(listener))),
            Err(e) if not_assigned(&e) => listeners.push((addr, None)),
            Err(e) => return Err(anyhow::Error::new(e).context(format!("listen on {addr}"))),
        }
    }
    let app = api::router(db, bus, status, &config)
        .merge(maps::router(config.maps_dir, config.map_source))
        .merge(wifi::router())
        .merge(update::router())
        .layer(axum::middleware::from_fn_with_state(Arc::new(networks), api::trusted_peer));
    let (stop, _) = watch::channel(());
    let mut servers = tokio::task::JoinSet::new();
    for (addr, listener) in listeners {
        servers.spawn(serve(addr, listener, app.clone(), stop.subscribe()));
    }
    stopped().await;
    stop.send_replace(());
    while servers.join_next().await.transpose()?.is_some() {}
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listen(toml: &str) -> Result<Vec<SocketAddr>, toml::de::Error> {
        toml::from_str::<Config>(toml).map(|c| c.listen)
    }

    fn addrs(list: &[&str]) -> Vec<SocketAddr> {
        list.iter().map(|a| a.parse().unwrap()).collect()
    }

    #[test]
    fn listen_is_one_address_or_a_list() {
        assert_eq!(listen("").unwrap(), addrs(&["0.0.0.0:8000"]));
        assert_eq!(listen(r#"listen = "0.0.0.0:80""#).unwrap(), addrs(&["0.0.0.0:80"]));
        assert_eq!(
            listen(r#"listen = ["127.0.0.1:80", "10.42.0.1:80", "[::1]:80"]"#).unwrap(),
            addrs(&["127.0.0.1:80", "10.42.0.1:80", "[::1]:80"])
        );
        for bad in [r#"listen = "localhost:80""#, r#"listen = ["127.0.0.1:80", "nope"]"#, "listen = []", "listen = 80"] {
            assert!(listen(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_wildcard_takes_its_port_for_its_family() {
        let list = addrs(&["127.0.0.1:80", "10.42.0.1:80", "0.0.0.0:80", "10.42.0.1:8080", "[::1]:80"]);
        assert_eq!(to_bind(&list), addrs(&["0.0.0.0:80", "10.42.0.1:8080", "[::1]:80"]));
        let list = addrs(&["127.0.0.1:80", "10.42.0.1:80"]);
        assert_eq!(to_bind(&list), list);
    }
}
