//! Messages flowing from the camera thread into the upload pipeline.

use tempfile::NamedTempFile;

use crate::camera::{AssetKind, ObjectInfo};

#[derive(Debug)]
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
