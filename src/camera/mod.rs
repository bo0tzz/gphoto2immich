//! Camera-side of the daemon. Polls libgphoto2 for a connected camera; once
//! one appears, walks its filesystem (backfill) and then watches for
//! `NewFile` events for the remainder of the session.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::config::Config;
use crate::immich::ImmichClient;
use crate::job::PipelineMessage;
use crate::notifications::{self, SyncStats};

mod gphoto;
pub mod object_info;
mod session;

pub use object_info::{AssetKind, ObjectInfo};

const DETECT_POLL_INTERVAL: Duration = Duration::from_secs(3);
const SESSION_ERROR_BACKOFF: Duration = Duration::from_secs(5);

#[derive(Clone)]
pub struct CameraDeps {
    pub config: Config,
    pub immich: Arc<ImmichClient>,
    pub stats: SyncStats,
}

/// Run forever, polling libgphoto2 for a connected camera and driving a
/// session when one appears. Returns only on shutdown.
pub async fn run(
    deps: CameraDeps,
    tx: mpsc::Sender<PipelineMessage>,
    shutdown: Arc<AtomicBool>,
) -> Result<()> {
    let ctx = ::gphoto2::Context::new()?;

    while !shutdown.load(Ordering::Relaxed) {
        let descriptors: Vec<_> = match ctx.list_cameras().await {
            Ok(iter) => iter.collect(),
            Err(e) => {
                warn!(error = ?e, "list_cameras failed");
                tokio::time::sleep(DETECT_POLL_INTERVAL).await;
                continue;
            }
        };

        let descriptor = match descriptors.into_iter().next() {
            Some(d) => d,
            None => {
                debug!("no camera detected");
                tokio::time::sleep(DETECT_POLL_INTERVAL).await;
                continue;
            }
        };

        info!(model = %descriptor.model, port = %descriptor.port, "camera detected");
        let camera = match ctx.get_camera(&descriptor).await {
            Ok(c) => c,
            Err(e) => {
                warn!(error = ?e, "failed to open camera");
                tokio::time::sleep(DETECT_POLL_INTERVAL).await;
                continue;
            }
        };

        notifications::notify_session_start(&descriptor.model);
        let session_result = session::run(&deps, &tx, &ctx, &camera, &shutdown).await;
        notifications::notify_session_end(deps.stats.take_count());
        drop(camera);
        match session_result {
            Ok(()) => info!("camera session ended cleanly"),
            Err(e) => {
                warn!(error = ?e, "camera session ended with error");
                tokio::time::sleep(SESSION_ERROR_BACKOFF).await;
            }
        }
    }

    info!("camera task exiting");
    Ok(())
}
