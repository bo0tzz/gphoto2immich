//! Desktop notifications via libnotify (D-Bus).
//!
//! Intentionally sparse: a notification when a camera is detected and one
//! when the session ends with the count of new uploads. Nothing per-file.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use tracing::debug;

const APP_NAME: &str = "gphoto2immich";

/// Shared counter of new syncs (= files downloaded from the camera and
/// queued for upload). The camera task both increments and drains it.
#[derive(Clone, Default)]
pub struct SyncStats {
    uploaded: Arc<AtomicU32>,
}

impl SyncStats {
    pub fn new() -> Self {
        Self::default()
    }

    /// Count one asset that was downloaded from the camera and queued for
    /// upload. Counted on the camera side rather than after the HTTP POST
    /// completes so the count reflects "what came off the camera" even if
    /// the pipeline is still draining when we read it.
    pub fn record_synced(&self) {
        self.uploaded.fetch_add(1, Ordering::Relaxed);
    }

    /// Atomically read and reset the counter.
    pub fn take_count(&self) -> u32 {
        self.uploaded.swap(0, Ordering::Relaxed)
    }
}

pub fn notify_session_start(camera_model: &str) {
    send(
        "Camera connected",
        &format!("Syncing {camera_model} \u{2192} Immich"),
    );
}

pub fn notify_session_end(uploaded: u32) {
    if uploaded == 0 {
        // Don't spam the tray when there was nothing new — the connect
        // notification already told the user something happened.
        debug!("session ended with 0 new uploads; suppressing notification");
        return;
    }
    let body = if uploaded == 1 {
        "Synced 1 new asset.".to_string()
    } else {
        format!("Synced {uploaded} new assets.")
    };
    send("Sync complete", &body);
}

fn send(summary: &str, body: &str) {
    // Best-effort: a missing D-Bus session (e.g. system-level daemon) just
    // means no popup, not a daemon failure.
    if let Err(e) = notify_rust::Notification::new()
        .appname(APP_NAME)
        .summary(summary)
        .body(body)
        .show()
    {
        debug!(error = ?e, "desktop notification failed (no D-Bus session?)");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn take_count_resets() {
        let s = SyncStats::new();
        s.record_synced();
        s.record_synced();
        s.record_synced();
        assert_eq!(s.take_count(), 3);
        assert_eq!(s.take_count(), 0);
    }

    #[test]
    fn record_synced_is_thread_safe() {
        let s = SyncStats::new();
        std::thread::scope(|scope| {
            for _ in 0..4 {
                let s = s.clone();
                scope.spawn(move || {
                    for _ in 0..25 {
                        s.record_synced();
                    }
                });
            }
        });
        assert_eq!(s.take_count(), 100);
    }
}
