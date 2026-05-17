//! One connected camera session: enumerate the camera filesystem,
//! compare against an Immich cache built up front, and download +
//! enqueue anything missing. Returns once the walk is complete.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::{Context as _, Result};
use chrono::{DateTime, Utc};
use gphoto2::{Camera, Context};
use tokio::sync::mpsc;
use tracing::{debug, info, trace, warn};

use super::gphoto::{digest_info, spool_to_tempfile};
use super::object_info::{AssetKind, ObjectInfo};
use super::CameraDeps;
use crate::immich::ImmichCache;
use crate::job::{PipelineMessage, UploadJob};

const BACKFILL_SLOP: chrono::Duration = chrono::Duration::hours(1);
/// Bail out of the backfill loop after this many consecutive per-file
/// errors. Cheap defence against the "camera was unplugged mid-sync"
/// case, where every subsequent libgphoto2 call returns an IO error and
/// we'd otherwise log hundreds of warnings before giving up.
const MAX_CONSECUTIVE_FAILURES: u32 = 3;

/// One sync of one camera: build a fresh Immich cache, walk the camera
/// filesystem against it, and return. The camera is left untouched on
/// the card; the outer loop in `camera::run` parks until the camera is
/// unplugged before it would consider another session against the same
/// port.
pub async fn run(
    deps: &CameraDeps,
    tx: &mpsc::Sender<PipelineMessage>,
    ctx: &Context,
    camera: &Camera,
    shutdown: &Arc<AtomicBool>,
) -> Result<()> {
    let cache = ImmichCache::load(&deps.immich)
        .await
        .context("loading Immich asset cache")?;
    let cutoff = cache.max_taken_at().map(|t| t - BACKFILL_SLOP);
    info!(
        cached_assets = cache.asset_count(),
        cutoff = ?cutoff,
        "starting backfill"
    );
    backfill(deps, tx, ctx, camera, &cache, cutoff, shutdown).await?;
    info!("backfill complete");
    Ok(())
}

#[derive(Debug, Default)]
struct BackfillStats {
    total_files: usize,
    skipped_non_asset: u32,
    skipped_before_cutoff: u32,
    already_in_immich: u32,
    downloaded: u32,
    failed: u32,
    aborted_early: bool,
}

async fn backfill(
    deps: &CameraDeps,
    tx: &mpsc::Sender<PipelineMessage>,
    ctx: &Context,
    camera: &Camera,
    cache: &ImmichCache,
    cutoff: Option<DateTime<Utc>>,
    shutdown: &Arc<AtomicBool>,
) -> Result<()> {
    let all = enumerate_files(camera).await?;
    let mut stats = BackfillStats {
        total_files: all.len(),
        ..Default::default()
    };
    info!(count = stats.total_files, "enumerated files on camera");
    let mut consecutive_failures: u32 = 0;
    for (folder, name) in all {
        if shutdown.load(Ordering::Relaxed) {
            break;
        }
        if !is_real_asset_name(&name) {
            trace!(folder = %folder, name = %name, "skipping non-asset filename");
            stats.skipped_non_asset += 1;
            continue;
        }
        let result = match prefetch_and_filter(camera, &folder, &name, deps, cutoff).await {
            Ok(Some(info)) => {
                let outcome =
                    process_one_with_info(deps, tx, ctx, camera, cache, &folder, &name, info).await;
                if let Ok(o) = &outcome {
                    match o {
                        FileOutcome::Downloaded => stats.downloaded += 1,
                        FileOutcome::AlreadyInImmich => stats.already_in_immich += 1,
                    }
                }
                outcome.map(|_| ())
            }
            Ok(None) => {
                trace!(folder = %folder, name = %name, "before cutoff or non-asset kind, skipping");
                stats.skipped_before_cutoff += 1;
                Ok(())
            }
            Err(e) => Err(e.context("file_info")),
        };
        match result {
            Ok(()) => consecutive_failures = 0,
            Err(e) => {
                warn!(folder = %folder, name = %name, error = ?e, "file failed");
                stats.failed += 1;
                consecutive_failures += 1;
                if consecutive_failures >= MAX_CONSECUTIVE_FAILURES {
                    warn!(
                        consecutive = consecutive_failures,
                        "too many consecutive failures, ending backfill early"
                    );
                    stats.aborted_early = true;
                    break;
                }
            }
        }
    }
    info!(
        total = stats.total_files,
        downloaded = stats.downloaded,
        already_in_immich = stats.already_in_immich,
        before_cutoff = stats.skipped_before_cutoff,
        non_asset = stats.skipped_non_asset,
        failed = stats.failed,
        aborted_early = stats.aborted_early,
        "backfill summary"
    );
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
        trace!(name = %name, "skipping non-photo/video");
        return Ok(None);
    }
    if let Some(c) = cutoff {
        if object_info.date_created_utc < c {
            return Ok(None);
        }
    }
    Ok(Some(object_info))
}

enum FileOutcome {
    Downloaded,
    AlreadyInImmich,
}

#[allow(clippy::too_many_arguments)] // session-wide context is genuinely needed here
async fn process_one_with_info(
    deps: &CameraDeps,
    tx: &mpsc::Sender<PipelineMessage>,
    ctx: &Context,
    camera: &Camera,
    cache: &ImmichCache,
    folder: &str,
    name: &str,
    info: ObjectInfo,
) -> Result<FileOutcome> {
    if let Some(existing_id) = cache.find_existing(&info.filename, info.date_created_utc) {
        debug!(
            filename = %info.filename,
            asset_id = %existing_id,
            "already in Immich, skipping download"
        );
        emit(
            tx,
            PipelineMessage::KnownAsset {
                basename: info.basename().to_owned(),
                kind: info.kind,
                asset_id: existing_id.to_owned(),
            },
        )
        .await;
        return Ok(FileOutcome::AlreadyInImmich);
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
    deps.stats.record_synced();
    Ok(FileOutcome::Downloaded)
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
