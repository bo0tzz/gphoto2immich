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

    // Port string of the camera we just successfully synced. Cleared when
    // the camera disappears. Prevents hot-looping reopens after a session
    // returns Ok while the camera is still plugged in.
    let mut already_synced: Option<String> = None;
    // Port string of the camera we've already notified the user about
    // (either start or failure). Suppresses repeat notifications when a
    // session keeps failing on retry — the user sees one popup per
    // plug-in cycle, not one per 5-second backoff iteration.
    let mut notified_for: Option<String> = None;

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
                if already_synced.take().is_some() || notified_for.take().is_some() {
                    info!("camera disconnected; ready for next sync");
                }
                debug!("no camera detected");
                tokio::time::sleep(DETECT_POLL_INTERVAL).await;
                continue;
            }
        };

        if already_synced.as_deref() == Some(&descriptor.port) {
            // Same camera still plugged in after a completed sync. Wait
            // for it to be unplugged before doing anything else.
            debug!(port = %descriptor.port, "already synced; waiting for unplug");
            tokio::time::sleep(DETECT_POLL_INTERVAL).await;
            continue;
        }

        let first_attempt_this_cycle = notified_for.as_deref() != Some(&descriptor.port);
        if first_attempt_this_cycle {
            info!(model = %descriptor.model, port = %descriptor.port, "camera detected");
        }
        let camera = match ctx.get_camera(&descriptor).await {
            Ok(c) => c,
            Err(e) => {
                warn!(error = ?e, "failed to open camera");
                tokio::time::sleep(DETECT_POLL_INTERVAL).await;
                continue;
            }
        };

        if first_attempt_this_cycle {
            notifications::notify_session_start(&descriptor.model);
            notified_for = Some(descriptor.port.clone());
        }
        let session_result = session::run(&deps, &tx, &ctx, &camera, &shutdown).await;
        notifications::notify_session_end(deps.stats.take_count());
        drop(camera);
        match session_result {
            Ok(()) => {
                info!("camera session ended cleanly");
                already_synced = Some(descriptor.port);
            }
            Err(e) => {
                let summary = format!("{e:#}");
                warn!(error = ?e, "camera session ended with error");
                if first_attempt_this_cycle {
                    notifications::notify_session_failure(&summary);
                }
                tokio::time::sleep(SESSION_ERROR_BACKOFF).await;
            }
        }
    }

    info!("camera task exiting");
    Ok(())
}
