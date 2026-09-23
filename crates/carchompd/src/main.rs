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
use std::{net::SocketAddr, path::PathBuf, sync::Arc};
use tokio::sync::{broadcast, watch};

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
    pub listen: SocketAddr,
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
            listen: ([0, 0, 0, 0], 8000).into(),
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

    let listener = tokio::net::TcpListener::bind(config.listen).await?;
    tracing::info!("listening on {}", config.listen);
    let app = api::router(db, bus, status, &config)
        .merge(maps::router(config.maps_dir, config.map_source))
        .merge(wifi::router())
        .merge(update::router())
        .layer(axum::middleware::from_fn_with_state(Arc::new(networks), api::trusted_peer));
    axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())
        .with_graceful_shutdown(async {
            tokio::signal::ctrl_c().await.ok();
        })
        .await?;
    Ok(())
}
