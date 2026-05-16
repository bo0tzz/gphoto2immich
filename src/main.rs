use anyhow::Result;
use tracing_subscriber::EnvFilter;

mod camera;

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_env("LOG_LEVEL").unwrap_or_else(|_| "info".into()))
        .init();

    tracing::info!("fujimmich {} starting", env!("CARGO_PKG_VERSION"));

    // Wire client name through to libfuji's app_get_client_name override.
    // This also pins our weak-symbol overrides via FORCE_KEEP so the linker
    // doesn't GC them.
    camera::set_client_name("fujimmich");

    // Phase 3+ wires up the camera connection loop here.
    tracing::info!("phase 2 skeleton: FFI + overrides ready");

    Ok(())
}
