//! Camera-side glue: FFI bindings to libfuji, weak-symbol overrides, helpers
//! for `PtpObjectInfo`, and the connection / event-poll loop.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::Duration as ChronoDuration;
use tokio::runtime::Handle;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

pub mod callbacks;
pub mod connection;
pub mod download;
pub mod ffi;
pub mod object_info;

pub use callbacks::set_client_name;
pub use connection::{BackfillCutoff, Runtime};
pub use object_info::{AssetKind, ObjectInfo};

use crate::config::Config;
use crate::immich::ImmichClient;
use crate::job::{PipelineMessage, UploadJob};

const POLL_INTERVAL: Duration = Duration::from_millis(1000);
const RECONNECT_DELAY: Duration = Duration::from_secs(2);
/// Slop subtracted from "most recent in Immich" to compute the backfill
/// cutoff, so a partially-uploaded batch from a previous run still gets
/// retried.
const BACKFILL_SLOP: ChronoDuration = ChronoDuration::hours(1);

#[derive(Clone)]
pub struct CameraDeps {
    pub config: Config,
    pub immich: Arc<ImmichClient>,
    pub tokio: Handle,
}

/// Entry point for the dedicated camera thread. Reconnects with backoff on
/// IO errors; exits when `shutdown` is set.
pub fn run(
    deps: CameraDeps,
    tx: mpsc::Sender<PipelineMessage>,
    shutdown: Arc<AtomicBool>,
) -> Result<()> {
    let mut runtime = Runtime::new()?;
    let is_shutdown = || shutdown.load(Ordering::Relaxed);

    while !is_shutdown() {
        if let Err(e) = connection::connect_with_backoff(
            &mut runtime,
            &deps.config.camera_ip,
            &is_shutdown,
        ) {
            warn!(error = %e, "connect aborted");
            break;
        }

        match drive_session(&mut runtime, &deps, &tx, &shutdown) {
            Ok(()) => {
                info!("camera session ended cleanly");
                break;
            }
            Err(e) => {
                warn!(error = %e, "session ended with error, will reconnect");
                std::thread::sleep(RECONNECT_DELAY);
            }
        }
    }

    info!("camera thread exiting");
    Ok(())
}

/// One connected session: compute backfill cutoff via Immich, run backfill,
/// then poll events forever.
fn drive_session(
    runtime: &mut Runtime,
    deps: &CameraDeps,
    tx: &mpsc::Sender<PipelineMessage>,
    shutdown: &Arc<AtomicBool>,
) -> Result<()> {
    let cutoff = compute_backfill_cutoff(deps)?;
    let initial = runtime.num_objects()?;
    info!(
        num_objects = initial,
        cutoff = ?cutoff.not_before,
        "starting backfill"
    );
    let last_seen = run_backfill(runtime, deps, tx, cutoff, initial, shutdown)?;
    info!(handles_processed = last_seen, "backfill complete");
    poll_events_loop(runtime, deps, tx, last_seen, shutdown)
}

fn compute_backfill_cutoff(deps: &CameraDeps) -> Result<BackfillCutoff> {
    let recent = deps
        .tokio
        .block_on(deps.immich.most_recent_taken_at(None))
        .context("looking up most-recent Immich asset for backfill cutoff")?;
    let not_before = recent.map(|t| t - BACKFILL_SLOP);
    Ok(BackfillCutoff { not_before })
}

/// Backfill: walk handles `1..=num_objects`, run the full per-photo flow on
/// each one that passes the cutoff.
fn run_backfill(
    runtime: &mut Runtime,
    deps: &CameraDeps,
    tx: &mpsc::Sender<PipelineMessage>,
    cutoff: BackfillCutoff,
    num_objects: i32,
    shutdown: &Arc<AtomicBool>,
) -> Result<i32> {
    for handle in 1..=num_objects {
        if shutdown.load(Ordering::Relaxed) {
            return Ok(handle - 1);
        }
        let info = match runtime.object_info(handle, deps.config.camera_tz) {
            Ok(info) => info,
            Err(e) => {
                warn!(handle, error = %e, "object_info failed during backfill");
                continue;
            }
        };
        if !cutoff.accept(&info) {
            debug!(handle, filename = %info.filename, "before cutoff, skipping");
            continue;
        }
        if let Err(e) = process_one(runtime, deps, tx, handle, info) {
            warn!(handle, error = %e, "per-photo flow failed during backfill");
        }
    }
    Ok(num_objects)
}

fn poll_events_loop(
    runtime: &mut Runtime,
    deps: &CameraDeps,
    tx: &mpsc::Sender<PipelineMessage>,
    mut last_seen: i32,
    shutdown: &Arc<AtomicBool>,
) -> Result<()> {
    info!(last_seen, "entering event poll loop");
    loop {
        if shutdown.load(Ordering::Relaxed) {
            return Ok(());
        }
        std::thread::sleep(POLL_INTERVAL);

        let current = runtime.poll_events()?;
        if current <= last_seen {
            continue;
        }
        info!(prev = last_seen, current, "new objects detected");
        for handle in (last_seen + 1)..=current {
            let info = match runtime.object_info(handle, deps.config.camera_tz) {
                Ok(info) => info,
                Err(e) => {
                    warn!(handle, error = %e, "object_info failed");
                    continue;
                }
            };
            if let Err(e) = process_one(runtime, deps, tx, handle, info) {
                warn!(handle, error = %e, "per-photo flow failed");
            }
        }
        last_seen = current;
    }
}

/// Per-photo flow: dedup pre-check, download (if not duplicate), enqueue.
fn process_one(
    runtime: &mut Runtime,
    deps: &CameraDeps,
    tx: &mpsc::Sender<PipelineMessage>,
    handle: i32,
    info: ObjectInfo,
) -> Result<()> {
    let existing = deps
        .tokio
        .block_on(deps.immich.find_existing(&info.filename, info.date_created_utc))
        .context("dedup pre-check")?;

    if let Some(existing) = existing {
        debug!(
            handle,
            filename = %info.filename,
            asset_id = %existing.id,
            "already in Immich, skipping download"
        );
        let basename = info.basename().to_owned();
        emit(
            tx,
            PipelineMessage::KnownAsset {
                basename,
                kind: info.kind,
                asset_id: existing.id,
            },
        );
        return Ok(());
    }

    info!(handle, filename = %info.filename, size = info.compressed_size, "downloading");
    // SAFETY: we're on the camera thread and hold the libfuji mutex.
    let downloaded = unsafe {
        download::download_to_tempfile(runtime.as_ptr(), handle, info.compressed_size as i32)
    }
    .with_context(|| format!("downloading handle {handle}"))?;

    debug!(
        handle,
        bytes = downloaded.bytes_written,
        sha1 = %downloaded.sha1_hex,
        "download complete"
    );

    emit(
        tx,
        PipelineMessage::Upload(UploadJob {
            info,
            file: downloaded.file,
            sha1_hex: downloaded.sha1_hex,
        }),
    );
    Ok(())
}

fn emit(tx: &mpsc::Sender<PipelineMessage>, msg: PipelineMessage) {
    if let Err(e) = tx.blocking_send(msg) {
        error!(error = %e, "failed to enqueue pipeline message (receiver gone)");
    }
}
