//! Messages flowing from the camera thread into the upload pipeline.

use tempfile::NamedTempFile;
use tokio::sync::oneshot;

use crate::camera::{AssetKind, ObjectInfo};

pub enum PipelineMessage {
    /// A freshly downloaded photo the pipeline must POST to Immich.
    Upload(UploadJob),
    /// A photo Immich already had — the pre-check returned an existing
    /// `asset_id`. We still want the stack tracker to see it so a partial
    /// JPEG+RAF pair (one new, one pre-existing) can be stacked.
    KnownAsset {
        basename: String,
        kind: AssetKind,
        asset_id: String,
    },
    /// Synchronisation point. The pipeline drains every in-flight upload
    /// task before acking the oneshot. Used by the camera task at session
    /// end to read a truthful upload count for the notification.
    Barrier(oneshot::Sender<()>),
}

impl std::fmt::Debug for PipelineMessage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PipelineMessage::Upload(job) => f.debug_tuple("Upload").field(job).finish(),
            PipelineMessage::KnownAsset {
                basename,
                kind,
                asset_id,
            } => f
                .debug_struct("KnownAsset")
                .field("basename", basename)
                .field("kind", kind)
                .field("asset_id", asset_id)
                .finish(),
            PipelineMessage::Barrier(_) => f.debug_tuple("Barrier").finish(),
        }
    }
}

/// A photo that the camera thread has finished downloading. Owns the local
/// tempfile (dropping the job cleans up the bytes on disk) and the
/// pre-computed SHA1 (hex-encoded) for Immich's `x-immich-checksum` header.
#[derive(Debug)]
pub struct UploadJob {
    pub info: ObjectInfo,
    pub file: NamedTempFile,
    pub sha1_hex: String,
}
