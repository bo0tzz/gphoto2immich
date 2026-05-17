//! tokio side of the data pipeline. Receives `PipelineMessage`s from the
//! camera thread, runs uploads concurrently against Immich, and routes
//! observed JPEG/RAF asset ids through the `StackTracker` to decide when to
//! create a stack.

use std::sync::Arc;

use anyhow::Result;
use tokio::sync::{mpsc, Mutex};
use tokio::task::JoinSet;
use tracing::{debug, error, info, warn};

use crate::camera::AssetKind;
use crate::config::Config;
use crate::immich::{ImmichClient, UploadOutcome, UploadRequest};
use crate::job::{PipelineMessage, UploadJob};
use crate::stack_tracker::{Decision, StackTracker};

#[derive(Clone)]
pub struct Pipeline {
    client: Arc<ImmichClient>,
    stack_tracker: Arc<Mutex<StackTracker>>,
    stack_enabled: bool,
}

impl Pipeline {
    pub fn new(client: Arc<ImmichClient>, config: &Config) -> Self {
        Pipeline {
            client,
            stack_tracker: Arc::new(Mutex::new(StackTracker::new())),
            stack_enabled: config.stack_jpeg_raf,
        }
    }

    /// Drive the pipeline: read messages off `rx`, spawn each as an
    /// independent task so uploads run concurrently. Returns when the channel
    /// is closed and all in-flight tasks complete.
    pub async fn run(self, mut rx: mpsc::Receiver<PipelineMessage>) {
        let mut tasks = JoinSet::new();
        while let Some(msg) = rx.recv().await {
            let pipeline = self.clone();
            tasks.spawn(async move {
                if let Err(e) = pipeline.handle(msg).await {
                    error!(error = ?e, "pipeline message failed");
                }
            });
        }
        // Drain in-flight uploads before returning.
        while tasks.join_next().await.is_some() {}
        info!("pipeline drained");
    }

    async fn handle(&self, msg: PipelineMessage) -> Result<()> {
        match msg {
            PipelineMessage::Upload(job) => self.upload(job).await,
            PipelineMessage::KnownAsset {
                basename,
                kind,
                asset_id,
            } => {
                if self.stack_enabled {
                    self.consider_stack(&basename, kind, asset_id).await?;
                }
                Ok(())
            }
        }
    }

    async fn upload(&self, job: UploadJob) -> Result<()> {
        let UploadJob {
            info,
            file,
            sha1_hex,
        } = job;
        let path = file.path().to_owned();
        let req = UploadRequest {
            file_path: &path,
            filename: &info.filename,
            file_created_at: info.date_created_utc,
            sha1_hex: &sha1_hex,
        };
        let result = self.client.upload(req).await?;
        match result.outcome {
            UploadOutcome::Created => info!(
                asset_id = %result.asset_id,
                filename = %info.filename,
                "uploaded"
            ),
            UploadOutcome::Duplicate => debug!(
                asset_id = %result.asset_id,
                filename = %info.filename,
                "server reported duplicate on upload"
            ),
        }
        // tempfile is dropped here, unlinking the local copy.
        drop(file);

        if self.stack_enabled {
            self.consider_stack(info.basename(), info.kind, result.asset_id)
                .await?;
        }
        Ok(())
    }

    async fn consider_stack(
        &self,
        basename: &str,
        kind: AssetKind,
        asset_id: String,
    ) -> Result<()> {
        let decision = {
            let mut tracker = self.stack_tracker.lock().await;
            tracker.observe(basename, kind, asset_id)
        };
        match decision {
            Decision::Wait | Decision::Ignore => Ok(()),
            Decision::Stack { jpeg_id, raf_id } => {
                if self.client.asset_has_stack(&jpeg_id).await? {
                    debug!(jpeg_id, raf_id, "JPEG already stacked, skipping");
                    return Ok(());
                }
                match self
                    .client
                    .create_stack(&[jpeg_id.clone(), raf_id.clone()])
                    .await
                {
                    Ok(()) => info!(jpeg_id, raf_id, "stack created"),
                    Err(e) => warn!(error = ?e, jpeg_id, raf_id, "stack create failed"),
                }
                Ok(())
            }
        }
    }
}
