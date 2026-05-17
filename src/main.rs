use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::Result;
use tokio::sync::mpsc;
use tracing_subscriber::EnvFilter;

mod camera;
mod config;
mod immich;
mod job;
mod notifications;
mod pipeline;
mod stack_tracker;

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_env("LOG_LEVEL").unwrap_or_else(|_| "info".into()))
        .init();

    let cfg = config::Config::from_env()?;
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        immich = %cfg.immich_url,
        tz = %cfg.camera_tz,
        stack = cfg.stack_jpeg_raf,
        "gphoto2immich starting"
    );

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    rt.block_on(async move { run(cfg).await })
}

async fn run(cfg: config::Config) -> Result<()> {
    let shutdown = Arc::new(AtomicBool::new(false));
    install_signal_handler(shutdown.clone());

    let immich = Arc::new(immich::ImmichClient::new(
        &cfg.immich_url,
        &cfg.immich_api_key,
    )?);
    let stats = notifications::SyncStats::new();
    let pipeline = pipeline::Pipeline::new(immich.clone(), &cfg, stats.clone());

    let (tx, rx) = mpsc::channel::<job::PipelineMessage>(64);

    let pipeline_handle = tokio::spawn(pipeline.run(rx));

    let camera_deps = camera::CameraDeps { immich, stats };
    let camera_shutdown = shutdown.clone();
    let camera_handle = tokio::spawn(async move {
        if let Err(e) = camera::run(camera_deps, tx, camera_shutdown).await {
            tracing::error!(error = ?e, "camera task failed");
        }
    });

    if let Err(e) = camera_handle.await {
        tracing::error!("camera task panicked: {e:?}");
    }
    if let Err(e) = pipeline_handle.await {
        tracing::error!("pipeline task panicked: {e:?}");
    }
    Ok(())
}

fn install_signal_handler(shutdown: Arc<AtomicBool>) {
    tokio::spawn(async move {
        let mut sigterm =
            match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
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
