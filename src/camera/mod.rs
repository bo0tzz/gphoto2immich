//! Camera-side glue: FFI bindings to libfuji, weak-symbol overrides, helpers
//! for `PtpObjectInfo`, and the connection / event-poll loop.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
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

/// Sent from the camera thread for each new asset detected. In Phase 3 these
/// jobs are only logged; later phases wire them through Immich dedup +
/// download + upload + stack.
#[derive(Debug)]
pub struct PendingPhoto {
    pub handle: i32,
    pub info: ObjectInfo,
}

const POLL_INTERVAL: Duration = Duration::from_millis(1000);
const RECONNECT_DELAY: Duration = Duration::from_secs(2);

/// Entry point for the dedicated camera thread. Runs forever (or until
/// `shutdown` is set), reconnecting on IO errors with backoff.
pub fn run(
    config: Config,
    tx: mpsc::Sender<PendingPhoto>,
    shutdown: Arc<AtomicBool>,
) -> Result<()> {
    let mut runtime = Runtime::new()?;
    let is_shutdown = || shutdown.load(Ordering::Relaxed);

    while !is_shutdown() {
        if let Err(e) =
            connection::connect_with_backoff(&mut runtime, &config.camera_ip, &is_shutdown)
        {
            warn!(error = %e, "connect aborted");
            break;
        }

        match drive_session(&mut runtime, &config, &tx, &shutdown) {
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

/// One connected session: run backfill, then poll events.
fn drive_session(
    runtime: &mut Runtime,
    config: &Config,
    tx: &mpsc::Sender<PendingPhoto>,
    shutdown: &Arc<AtomicBool>,
) -> Result<()> {
    let cutoff = BackfillCutoff { not_before: None };

    let initial = runtime.num_objects()?;
    info!(num_objects = initial, "starting backfill");
    let last_seen = run_backfill(runtime, config, tx, cutoff, initial, shutdown)?;
    info!(handles_processed = last_seen, "backfill complete");

    poll_events_loop(runtime, config, tx, last_seen, shutdown)
}

/// Backfill: walk handles `1..=num_objects`, emit each one that passes the
/// cutoff filter.
fn run_backfill(
    runtime: &mut Runtime,
    config: &Config,
    tx: &mpsc::Sender<PendingPhoto>,
    cutoff: BackfillCutoff,
    num_objects: i32,
    shutdown: &Arc<AtomicBool>,
) -> Result<i32> {
    for handle in 1..=num_objects {
        if shutdown.load(Ordering::Relaxed) {
            return Ok(handle - 1);
        }
        match runtime.object_info(handle, config.camera_tz) {
            Ok(info) => {
                if !cutoff.accept(&info) {
                    debug!(handle, filename = %info.filename, "before cutoff, skipping");
                    continue;
                }
                emit(tx, PendingPhoto { handle, info });
            }
            Err(e) => {
                warn!(handle, error = %e, "failed to fetch object info during backfill");
            }
        }
    }
    Ok(num_objects)
}

/// Event poll: every `POLL_INTERVAL`, query `num_objects` and enqueue any
/// handles past `last_seen`.
fn poll_events_loop(
    runtime: &mut Runtime,
    config: &Config,
    tx: &mpsc::Sender<PendingPhoto>,
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
            match runtime.object_info(handle, config.camera_tz) {
                Ok(info) => emit(tx, PendingPhoto { handle, info }),
                Err(e) => warn!(handle, error = %e, "failed to fetch object info"),
            }
        }
        last_seen = current;
    }
}

fn emit(tx: &mpsc::Sender<PendingPhoto>, job: PendingPhoto) {
    // We're inside libfuji's mutex on the camera thread; using
    // `blocking_send` keeps that ownership coherent. If the tokio side has
    // shut down its receiver, drop the job and log — losing a job here is
    // better than panicking on the camera thread.
    if let Err(e) = tx.blocking_send(job) {
        error!(error = %e, "failed to enqueue photo (receiver gone)");
    }
}
