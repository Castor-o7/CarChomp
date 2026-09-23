//! Offline maps. A map region is one PMTiles archive in `maps_dir`; the
//! browser reads tiles straight out of it with HTTP range requests, so serving
//! maps is just serving files. Downloading a region is delegated to the
//! `pmtiles` CLI, which can cut a bounding box out of a remote planet-sized
//! archive in a handful of requests.

use axum::{
    Json, Router,
    extract::{Path, Request, State},
    http::StatusCode,
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{delete, get},
};
use serde::{Deserialize, Serialize};
use std::{
    path::{Path as FsPath, PathBuf},
    process::Stdio,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::process::Command;
use tower_http::services::ServeDir;

#[derive(Clone)]
struct Maps {
    dir: PathBuf,
    source: Option<String>,
    /// The running download, or the last one if it failed.
    job: Arc<Mutex<Option<Job>>>,
}

#[derive(Clone, Serialize)]
struct Job {
    name: String,
    error: Option<String>,
}

/// Downloads are not allowed to leave less than this free on the disk the
/// database lives on.
const RESERVE_BYTES: u64 = 2 << 30;
/// Roughly a US state at street detail.
const MAX_TILES: f64 = 1e6;

pub fn router(dir: PathBuf, source: Option<String>) -> Router {
    // No download can be running yet, so any partial one was cut off.
    let _ = std::fs::remove_dir_all(dir.join(PARTIAL));
    if let Err(e) = std::fs::create_dir_all(dir.join(PARTIAL)) {
        tracing::warn!("maps: cannot create {}: {e}", dir.display());
    }
    // Older versions left `<name>.part` beside the archives.
    for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
        if entry.path().extension().is_some_and(|x| x == "part") {
            let _ = std::fs::remove_file(entry.path());
        }
    }
    if let Some(s) = source.as_ref().filter(|s| !(s.starts_with("https://") || s.starts_with("http://") || s.starts_with('/'))) {
        tracing::warn!("maps: map_source {s:?} is neither a URL nor an absolute path; pmtiles will likely reject it");
    }
    Router::new()
        .route("/api/maps", get(list).post(download))
        .route("/api/maps/{name}", delete(remove))
        .route_layer(middleware::from_fn(crate::api::same_origin))
        .nest_service("/maps", ServeDir::new(&dir))
        .layer(middleware::from_fn(hide_partial))
        .with_state(Maps { dir, source, job: Arc::default() })
}

/// Where downloads are written until complete: inside `maps_dir`, so the
/// final rename stays on one filesystem, but not served.
const PARTIAL: &str = ".partial";

async fn hide_partial(req: Request, next: Next) -> Response {
    let path = req.uri().path().to_ascii_lowercase();
    if path.contains("/.") || path.contains("/%2e") {
        return StatusCode::NOT_FOUND.into_response();
    }
    next.run(req).await
}

impl Maps {
    fn archive(&self, name: &str) -> PathBuf {
        self.dir.join(format!("{name}.pmtiles"))
    }

    fn partial(&self, name: &str) -> PathBuf {
        self.dir.join(PARTIAL).join(format!("{name}.part"))
    }
}

/// Names become file names and command arguments, so keep them boring.
fn valid_name(name: &str) -> bool {
    (1..=64).contains(&name.len()) && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Client-supplied sources may only be a Protomaps daily build, so a client
/// cannot make the car fetch from anywhere else.
fn protomaps_build(url: &str) -> bool {
    url.strip_prefix("https://build.protomaps.com/")
        .and_then(|key| key.strip_suffix(".pmtiles"))
        .is_some_and(valid_name)
}

/// How many tiles a region holds, zoom 0 to `maxzoom` inclusive.
fn tiles([w, s, e, n]: [f64; 4], maxzoom: u8) -> f64 {
    let y = |lat: f64| {
        let lat = lat.clamp(-85.0511, 85.0511).to_radians();
        (1.0 - lat.tan().asinh() / std::f64::consts::PI) / 2.0
    };
    (0..=maxzoom)
        .map(|z| {
            let count = f64::from(1u32 << z);
            let span = |a: f64, b: f64| ((b * count).floor().min(count - 1.0) - (a * count).floor()).max(0.0) + 1.0;
            span((w + 180.0) / 360.0, (e + 180.0) / 360.0) * span(y(n), y(s))
        })
        .sum()
}

fn free_bytes(dir: &FsPath) -> Option<u64> {
    rustix::fs::statvfs(dir).ok().map(|d| d.f_bavail * d.f_frsize)
}

fn size(path: PathBuf) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

async fn list(State(maps): State<Maps>) -> Response {
    let mut archives: Vec<_> = std::fs::read_dir(&maps.dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            let name = path.file_stem()?.to_str()?.to_owned();
            (path.extension()? == "pmtiles").then(|| serde_json::json!({ "name": name, "bytes": size(path) }))
        })
        .collect();
    archives.sort_by_key(|a| a["name"].as_str().map(str::to_owned));

    let job = maps.job.lock().unwrap().clone().map(|job| {
        let bytes = size(maps.partial(&job.name));
        serde_json::json!({ "name": job.name, "error": job.error, "bytes": bytes })
    });
    Json(serde_json::json!({ "archives": archives, "job": job, "source": maps.source })).into_response()
}

#[derive(Deserialize)]
struct Region {
    name: String,
    /// west, south, east, north
    bbox: [f64; 4],
    maxzoom: u8,
    /// Remote archive to cut from; ignored if the daemon has `map_source` set,
    /// otherwise it must be a build.protomaps.com archive.
    source: Option<String>,
}

async fn download(State(maps): State<Maps>, Json(region): Json<Region>) -> Response {
    let [w, s, e, n] = region.bbox;
    let sane = valid_name(&region.name) && w < e && s < n && w >= -180.0 && e <= 180.0 && s >= -90.0 && n <= 90.0 && region.maxzoom <= 15;
    if !sane {
        return (StatusCode::BAD_REQUEST, "invalid name, bounds or zoom").into_response();
    }
    // The operator's own setting is trusted as is: pmtiles also reads http:// and local files.
    let Some(source) = maps.source.clone().or(region.source.filter(|s| protomaps_build(s))) else {
        return (StatusCode::BAD_REQUEST, "source must be a build.protomaps.com archive").into_response();
    };
    if tiles(region.bbox, region.maxzoom) > MAX_TILES {
        return (StatusCode::BAD_REQUEST, "region too large at this detail: zoom in or pick less detail").into_response();
    }
    let Some(budget) = free_bytes(&maps.dir).and_then(|free| free.checked_sub(RESERVE_BYTES)) else {
        return (StatusCode::INSUFFICIENT_STORAGE, "less than 2 GB free").into_response();
    };
    {
        let mut job = maps.job.lock().unwrap();
        if job.as_ref().is_some_and(|j| j.error.is_none()) {
            return (StatusCode::CONFLICT, "a download is already running").into_response();
        }
        *job = Some(Job { name: region.name.clone(), error: None });
    }

    tokio::spawn(async move {
        let partial = maps.partial(&region.name);
        let _ = std::fs::create_dir_all(maps.dir.join(PARTIAL));
        let child = Command::new("pmtiles")
            .arg("extract")
            .arg(&source)
            .arg(&partial)
            .arg(format!("--bbox={w},{s},{e},{n}"))
            .arg(format!("--maxzoom={}", region.maxzoom))
            .arg("--quiet")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn();
        // Stops (and so kills) pmtiles once the file outgrows the free space.
        let outgrown = async {
            while size(partial.clone()) <= budget {
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        };
        let error = match child {
            Err(e) => Some(format!("cannot run pmtiles: {e}")),
            Ok(child) => tokio::select! {
                result = child.wait_with_output() => match result {
                    Ok(out) if out.status.success() => std::fs::rename(&partial, maps.archive(&region.name)).err().map(|e| e.to_string()),
                    Ok(out) => Some(String::from_utf8_lossy(&out.stderr).lines().last().unwrap_or("pmtiles failed").to_owned()),
                    Err(e) => Some(format!("pmtiles: {e}")),
                },
                () = outgrown => Some("stopped: the region would leave less than 2 GB free".to_owned()),
            },
        };
        if let Some(error) = &error {
            tracing::warn!("maps: {}: {error}", region.name);
            let _ = std::fs::remove_file(&partial);
        }
        *maps.job.lock().unwrap() = error.map(|error| Job { name: region.name, error: Some(error) });
    });
    StatusCode::ACCEPTED.into_response()
}

async fn remove(State(maps): State<Maps>, Path(name): Path<String>) -> StatusCode {
    if valid_name(&name) && std::fs::remove_file(maps.archive(&name)).is_ok() {
        StatusCode::NO_CONTENT
    } else {
        StatusCode::NOT_FOUND
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_protomaps_builds_from_clients() {
        assert!(protomaps_build("https://build.protomaps.com/20260922.pmtiles"));
        for url in [
            "https://192.168.1.1/admin",
            "https://build.protomaps.com.evil.example/20260922.pmtiles",
            "https://build.protomaps.com/../x.pmtiles",
            "https://build.protomaps.com/a/b.pmtiles",
            "https://build.protomaps.com/20260922.pmtiles?x=1",
            "http://build.protomaps.com/20260922.pmtiles",
        ] {
            assert!(!protomaps_build(url), "{url}");
        }
    }

    #[test]
    fn counts_tiles() {
        assert_eq!(tiles([-180.0, -90.0, 180.0, 90.0], 0), 1.0);
        assert_eq!(tiles([-180.0, -90.0, 180.0, 90.0], 2), 1.0 + 4.0 + 16.0);
        // A city at street detail is fine; the planet is not.
        assert!(tiles([-122.8, 45.4, -122.5, 45.6], 15) < 1e4);
        assert!(tiles([-180.0, -90.0, 180.0, 90.0], 15) > MAX_TILES);
    }
}
