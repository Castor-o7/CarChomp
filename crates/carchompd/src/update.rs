//! Updating from the web UI: the upload lands where the root-owned
//! carchomp-update.service expects it, and that unit does the rest
//! (deploy/update.sh). The daemon never runs the installer itself; it is
//! restarted by it.

use axum::{
    Router,
    body::{Body, HttpBody},
    http::{StatusCode, header},
    middleware,
    response::{IntoResponse, Response},
    routing::get,
};
use std::{path::Path, pin::Pin, sync::Arc, time::Duration};
use tokio::{io::AsyncWriteExt, process::Command, sync::Semaphore};

const DIR: &str = "/var/lib/carchomp/update";
const LIMIT: u64 = 300 << 20;
/// An upload that stalls this long is abandoned.
const READ_TIMEOUT: Duration = Duration::from_secs(60);
const UNIT: &str = "carchomp-update.service";

pub fn router() -> Router {
    // One upload at a time: both would write the same file.
    let uploading = Arc::new(Semaphore::new(1));
    Router::new()
        .route("/api/system/update", get(status).post(move |body| upload(uploading, body)))
        .route_layer(middleware::from_fn(crate::api::same_origin))
}

/// The updater's status.json, or idle if it has never run.
async fn status() -> Response {
    let body = tokio::fs::read_to_string(Path::new(DIR).join("status.json"))
        .await
        .unwrap_or_else(|_| r#"{"state":"idle"}"#.into());
    ([(header::CONTENT_TYPE, "application/json")], body).into_response()
}

/// Whether an update is under way: status.json says so and the unit agrees.
/// The unit is asked too because a power cut mid-update leaves "running"
/// in the file for good.
async fn running() -> bool {
    let Ok(text) = tokio::fs::read_to_string(Path::new(DIR).join("status.json")).await else {
        return false;
    };
    if !says_running(&text) {
        return false;
    }
    let active = Command::new("systemctl").args(["is-active", "--quiet", UNIT]).status().await;
    active.is_ok_and(|s| s.success())
}

fn says_running(status_json: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(status_json).is_ok_and(|v| v["state"] == "running")
}

async fn upload(uploading: Arc<Semaphore>, body: Body) -> Response {
    let Ok(_permit) = uploading.try_acquire() else {
        return (StatusCode::CONFLICT, "another upload is in progress").into_response();
    };
    if running().await {
        return (StatusCode::CONFLICT, "an update is already running").into_response();
    }
    let dir = Path::new(DIR);
    let (part, bundle) = (dir.join("bundle.tar.gz.part"), dir.join("bundle.tar.gz"));
    let saved = receive(body, dir, &part).await;
    if saved.is_err() {
        let _ = tokio::fs::remove_file(&part).await;
    }
    match saved {
        Ok(()) => {}
        Err(Upload::Client(status, why)) => return (status, why).into_response(),
        Err(Upload::Io(e)) => {
            tracing::error!("update: saving the upload: {e}");
            return (StatusCode::INTERNAL_SERVER_ERROR, format!("could not save the upload: {e}")).into_response();
        }
    }
    if let Err(e) = tokio::fs::rename(&part, &bundle).await {
        tracing::error!("update: {e}");
        let _ = tokio::fs::remove_file(&part).await;
        return (StatusCode::INTERNAL_SERVER_ERROR, format!("could not save the upload: {e}")).into_response();
    }
    let started = Command::new("systemctl").args(["start", "--no-block", UNIT]).output().await;
    match started {
        Ok(out) if out.status.success() => {
            tracing::info!("update: bundle received, {UNIT} started");
            StatusCode::ACCEPTED.into_response()
        }
        failed => {
            let why = match failed {
                Ok(out) => String::from_utf8_lossy(&out.stderr).trim().to_owned(),
                Err(e) => e.to_string(),
            };
            tracing::error!("update: starting {UNIT}: {why}");
            let _ = tokio::fs::remove_file(&bundle).await;
            (StatusCode::INTERNAL_SERVER_ERROR, format!("could not start the updater: {why}")).into_response()
        }
    }
}

enum Upload {
    Client(StatusCode, &'static str),
    Io(std::io::Error),
}

impl From<std::io::Error> for Upload {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

/// Stream the request body to `part`, never holding more than a frame of it.
async fn receive(mut body: Body, dir: &Path, part: &Path) -> Result<(), Upload> {
    tokio::fs::create_dir_all(dir).await?;
    let mut file = tokio::fs::File::create(part).await?;
    let mut size = 0u64;
    let mut first = true;
    loop {
        let frame = std::future::poll_fn(|cx| Pin::new(&mut body).poll_frame(cx));
        let frame = match tokio::time::timeout(READ_TIMEOUT, frame).await {
            Err(_) => return Err(Upload::Client(StatusCode::REQUEST_TIMEOUT, "upload stalled")),
            Ok(None) => break,
            Ok(Some(Err(_))) => return Err(Upload::Client(StatusCode::BAD_REQUEST, "upload interrupted")),
            Ok(Some(Ok(frame))) => frame,
        };
        let Ok(data) = frame.into_data() else { continue }; // trailers
        if first && !data.is_empty() {
            // Every gzip file starts 1f 8b; anything else is the wrong file.
            if !data.starts_with(&[0x1f, 0x8b][..data.len().min(2)]) {
                return Err(Upload::Client(StatusCode::BAD_REQUEST, "not a gzip file: upload a carchomp-*.tar.gz release bundle"));
            }
            first = false;
        }
        size += data.len() as u64;
        if size > LIMIT {
            return Err(Upload::Client(StatusCode::PAYLOAD_TOO_LARGE, "a release bundle is at most 300 MB"));
        }
        file.write_all(&data).await?;
    }
    if size < 2 {
        return Err(Upload::Client(StatusCode::BAD_REQUEST, "empty upload"));
    }
    file.sync_all().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn running_only_when_the_file_says_so() {
        assert!(says_running(r#"{"state":"running","version":"v1","message":"","log":""}"#));
        assert!(!says_running(r#"{"state":"done"}"#));
        assert!(!says_running(r#"{"state":"failed"}"#));
        assert!(!says_running("{\"state\":\"runn"));
        assert!(!says_running(""));
    }
}
