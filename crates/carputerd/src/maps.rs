//! Offline maps. A map region is one PMTiles archive in `maps_dir`; the
//! browser reads tiles straight out of it with HTTP range requests, so serving
//! maps is just serving files. Downloading a region is delegated to the
//! `pmtiles` CLI, which can cut a bounding box out of a remote planet-sized
//! archive in a handful of requests.

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{delete, get},
};
use serde::{Deserialize, Serialize};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
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

pub fn router(dir: PathBuf, source: Option<String>) -> Router {
    if let Err(e) = std::fs::create_dir_all(&dir) {
        tracing::warn!("maps: cannot create {}: {e}", dir.display());
    }
    Router::new()
        .route("/api/maps", get(list).post(download))
        .route("/api/maps/{name}", delete(remove))
        .nest_service("/maps", ServeDir::new(&dir))
        .with_state(Maps { dir, source, job: Arc::default() })
}

impl Maps {
    fn archive(&self, name: &str) -> PathBuf {
        self.dir.join(format!("{name}.pmtiles"))
    }

    fn partial(&self, name: &str) -> PathBuf {
        self.dir.join(format!("{name}.part"))
    }
}

/// Names become file names and command arguments, so keep them boring.
fn valid_name(name: &str) -> bool {
    (1..=64).contains(&name.len()) && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
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
    /// Remote archive to cut from; ignored if the daemon has `map_source` set.
    source: Option<String>,
}

async fn download(State(maps): State<Maps>, Json(region): Json<Region>) -> Response {
    let [w, s, e, n] = region.bbox;
    let sane = valid_name(&region.name) && w < e && s < n && w >= -180.0 && e <= 180.0 && s >= -90.0 && n <= 90.0 && region.maxzoom <= 15;
    let source = maps.source.clone().or(region.source).filter(|s| s.starts_with("https://"));
    let (true, Some(source)) = (sane, source) else {
        return StatusCode::BAD_REQUEST.into_response();
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
        let result = Command::new("pmtiles")
            .arg("extract")
            .arg(&source)
            .arg(&partial)
            .arg(format!("--bbox={w},{s},{e},{n}"))
            .arg(format!("--maxzoom={}", region.maxzoom))
            .arg("--quiet")
            .output()
            .await;
        let error = match result {
            Ok(out) if out.status.success() => std::fs::rename(&partial, maps.archive(&region.name)).err().map(|e| e.to_string()),
            Ok(out) => Some(String::from_utf8_lossy(&out.stderr).lines().last().unwrap_or("pmtiles failed").to_owned()),
            Err(e) => Some(format!("cannot run pmtiles: {e}")),
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
