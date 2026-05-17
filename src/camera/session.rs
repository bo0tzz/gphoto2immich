//! One connected camera session: enumerate the filesystem (backfill),
//! then watch for `NewFile` events for the rest of the session.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::{Context as _, Result};
use chrono::{DateTime, Utc};
use gphoto2::{Camera, Context};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use super::gphoto::{digest_info, spool_to_tempfile};
use super::object_info::{AssetKind, ObjectInfo};
use super::CameraDeps;
use crate::job::{PipelineMessage, UploadJob};

const BACKFILL_SLOP: chrono::Duration = chrono::Duration::hours(1);

/// One sync of one camera: run a backfill against the current filesystem
/// state, then return. The camera is left untouched on the card; the
/// outer loop in `camera::run` parks until the camera is unplugged before
/// it would even consider another session against the same port.
pub async fn run(
    deps: &CameraDeps,
    tx: &mpsc::Sender<PipelineMessage>,
    ctx: &Context,
    camera: &Camera,
    shutdown: &Arc<AtomicBool>,
) -> Result<()> {
    let cutoff = compute_cutoff(deps).await?;
    info!(cutoff = ?cutoff, "starting backfill");
    backfill(deps, tx, ctx, camera, cutoff, shutdown).await?;
    info!("backfill complete");
    Ok(())
}

async fn compute_cutoff(deps: &CameraDeps) -> Result<Option<DateTime<Utc>>> {
    let newest = deps
        .immich
        .most_recent_taken_at(None)
        .await
        .context("looking up most-recent Immich asset for backfill cutoff")?;
    Ok(newest.map(|t| t - BACKFILL_SLOP))
}

async fn backfill(
    deps: &CameraDeps,
    tx: &mpsc::Sender<PipelineMessage>,
    ctx: &Context,
    camera: &Camera,
    cutoff: Option<DateTime<Utc>>,
    shutdown: &Arc<AtomicBool>,
) -> Result<()> {
    let all = enumerate_files(camera).await?;
    info!(count = all.len(), "enumerated files on camera");
    for (folder, name) in all {
        if shutdown.load(Ordering::Relaxed) {
            return Ok(());
        }
        if !is_real_asset_name(&name) {
            debug!(folder = %folder, name = %name, "skipping non-asset filename");
            continue;
        }
        match prefetch_and_filter(camera, &folder, &name, deps, cutoff).await {
            Ok(Some(info)) => {
                if let Err(e) =
                    process_one_with_info(deps, tx, ctx, camera, &folder, &name, info).await
                {
                    warn!(folder = %folder, name = %name, error = ?e, "process_one failed");
                }
            }
            Ok(None) => {
                debug!(folder = %folder, name = %name, "before cutoff, skipping");
            }
            Err(e) => {
                warn!(folder = %folder, name = %name, error = ?e, "info failed");
            }
        }
    }
    Ok(())
}

/// Recursively list every file under `/`, returning `(folder, filename)`.
async fn enumerate_files(camera: &Camera) -> Result<Vec<(String, String)>> {
    let fs = camera.fs();
    let mut out = Vec::new();
    let mut stack: Vec<String> = vec!["/".into()];
    while let Some(folder) = stack.pop() {
        match fs.list_folders(&folder).await {
            Ok(folders) => {
                for sub in folders {
                    stack.push(join_path(&folder, &sub));
                }
            }
            Err(e) => warn!(folder = %folder, error = ?e, "list_folders failed"),
        }
        match fs.list_files(&folder).await {
            Ok(files) => {
                for name in files {
                    out.push((folder.clone(), name));
                }
            }
            Err(e) => warn!(folder = %folder, error = ?e, "list_files failed"),
        }
    }
    Ok(out)
}

/// Filter out filenames libgphoto2 sometimes hands back that aren't real
/// assets — e.g. ".JPG" (hidden Mac sidecar, AVI thumbnails, etc.). A real
/// asset has both a non-empty basename and a recognisable extension.
fn is_real_asset_name(name: &str) -> bool {
    let Some((base, ext)) = name.rsplit_once('.') else {
        return false;
    };
    !base.is_empty() && !ext.is_empty() && !name.starts_with('.')
}

fn join_path(parent: &str, child: &str) -> String {
    if parent.ends_with('/') {
        format!("{parent}{child}")
    } else {
        format!("{parent}/{child}")
    }
}

async fn prefetch_and_filter(
    camera: &Camera,
    folder: &str,
    name: &str,
    deps: &CameraDeps,
    cutoff: Option<DateTime<Utc>>,
) -> Result<Option<ObjectInfo>> {
    let info = camera.fs().file_info(folder, name).await?;
    let object_info = digest_info(&info, name, deps.config.camera_tz)?;
    if matches!(object_info.kind, AssetKind::Other) {
        debug!(name = %name, "skipping non-photo/video");
        return Ok(None);
    }
    if let Some(c) = cutoff {
        if object_info.date_created_utc < c {
            return Ok(None);
        }
    }
    Ok(Some(object_info))
}

async fn process_one_with_info(
    deps: &CameraDeps,
    tx: &mpsc::Sender<PipelineMessage>,
    ctx: &Context,
    camera: &Camera,
    folder: &str,
    name: &str,
    info: ObjectInfo,
) -> Result<()> {
    let existing = deps
        .immich
        .find_existing(&info.filename, info.date_created_utc)
        .await
        .context("dedup pre-check")?;

    if let Some(existing) = existing {
        debug!(
            filename = %info.filename,
            asset_id = %existing.id,
            "already in Immich, skipping download"
        );
        emit(
            tx,
            PipelineMessage::KnownAsset {
                basename: info.basename().to_owned(),
                kind: info.kind,
                asset_id: existing.id,
            },
        )
        .await;
        return Ok(());
    }

    info!(filename = %info.filename, size = info.size, "downloading");
    let cf = camera
        .fs()
        .download(folder, name)
        .await
        .with_context(|| format!("downloading {folder}/{name}"))?;
    let data = cf
        .get_data(ctx)
        .await
        .with_context(|| format!("reading bytes of {folder}/{name}"))?;
    let downloaded = spool_to_tempfile(&data)?;
    debug!(
        filename = %info.filename,
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
    )
    .await;
    Ok(())
}

async fn emit(tx: &mpsc::Sender<PipelineMessage>, msg: PipelineMessage) {
    if let Err(e) = tx.send(msg).await {
        warn!(error = ?e, "failed to enqueue pipeline message (receiver gone)");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn join_path_with_trailing_slash() {
        assert_eq!(join_path("/", "DCIM"), "/DCIM");
        assert_eq!(join_path("/DCIM", "100_FUJI"), "/DCIM/100_FUJI");
    }

    #[test]
    fn filters_dotfiles_and_extensionless() {
        assert!(is_real_asset_name("DSCF4109.JPG"));
        assert!(is_real_asset_name("DSCF4109.RAF"));
        assert!(!is_real_asset_name(".JPG"));
        assert!(!is_real_asset_name(".DS_Store"));
        assert!(!is_real_asset_name("no_extension"));
        assert!(!is_real_asset_name("trailing."));
    }
}
