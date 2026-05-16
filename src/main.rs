use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::Result;
use tokio::sync::mpsc;
use tracing_subscriber::EnvFilter;

mod camera;
mod config;
mod immich;
mod job;

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_env("LOG_LEVEL").unwrap_or_else(|_| "info".into()))
        .init();

    let cfg = config::Config::from_env()?;
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        camera = %cfg.camera_ip,
        immich = %cfg.immich_url,
        tz = %cfg.camera_tz,
        "fujimmich starting"
    );

    camera::set_client_name(&cfg.client_name);

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    rt.block_on(async move { run(cfg).await })
}

async fn run(cfg: config::Config) -> Result<()> {
    let shutdown = Arc::new(AtomicBool::new(false));
    install_signal_handler(shutdown.clone());

    let (tx, mut rx) = mpsc::channel::<camera::PendingPhoto>(64);

    // Camera thread: blocking libfuji calls, owns the PtpRuntime.
    let camera_shutdown = shutdown.clone();
    let camera_cfg = cfg.clone();
    let camera_thread = std::thread::Builder::new()
        .name("fujimmich-camera".into())
        .spawn(move || {
            if let Err(e) = camera::run(camera_cfg, tx, camera_shutdown) {
                tracing::error!(error = %e, "camera thread failed");
            }
        })?;

    // Phase 3 consumer: just log each candidate photo. Phase 5 replaces this
    // with the Immich-dedup → download → upload → stack pipeline.
    while let Some(pending) = rx.recv().await {
        tracing::info!(
            handle = pending.handle,
            filename = %pending.info.filename,
            size = pending.info.compressed_size,
            taken_at = %pending.info.date_created_utc,
            kind = ?pending.info.kind,
            "candidate photo"
        );
    }

    if let Err(e) = camera_thread.join() {
        tracing::error!("camera thread join panicked: {e:?}");
    }
    Ok(())
}

fn install_signal_handler(shutdown: Arc<AtomicBool>) {
    tokio::spawn(async move {
        let mut sigterm = match tokio::signal::unix::signal(
            tokio::signal::unix::SignalKind::terminate(),
        ) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("failed to register SIGTERM handler: {e}");
                return;
            }
        };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => tracing::info!("received SIGINT, shutting down"),
            _ = sigterm.recv() => tracing::info!("received SIGTERM, shutting down"),
        }
        shutdown.store(true, Ordering::Relaxed);
    });
}
