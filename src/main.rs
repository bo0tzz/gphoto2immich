use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::Result;
use tokio::sync::mpsc;
use tracing_subscriber::EnvFilter;

mod camera;
mod config;
mod immich;
mod job;
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
        stack = cfg.stack_jpeg_raf,
        "fujimmich starting"
    );

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    rt.block_on(async move { run(cfg).await })
}

async fn run(cfg: config::Config) -> Result<()> {
    let shutdown = Arc::new(AtomicBool::new(false));
    install_signal_handler(shutdown.clone());

    let immich = Arc::new(immich::ImmichClient::new(&cfg.immich_url, &cfg.immich_api_key)?);
    let pipeline = pipeline::Pipeline::new(immich.clone(), &cfg);

    let (_tx, rx) = mpsc::channel::<job::PipelineMessage>(64);

    // libgphoto2 side is wired up in the next commit. For now the daemon
    // just spins the pipeline (which drains immediately) so the binary
    // builds and exits cleanly.
    let pipeline_handle = tokio::spawn(pipeline.run(rx));
    pipeline_handle.await.ok();

    let _ = shutdown.load(Ordering::Relaxed);
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
