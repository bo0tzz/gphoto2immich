//! The unit of work the camera thread emits to the upload pipeline.

use tempfile::NamedTempFile;

use crate::camera::ObjectInfo;

/// A photo that the camera thread has finished downloading. Owns the local
/// tempfile (so dropping the job cleans up the bytes on disk) and the
/// pre-computed SHA1 (hex-encoded) for Immich's `x-immich-checksum` header.
#[derive(Debug)]
pub struct UploadJob {
    pub info: ObjectInfo,
    pub file: NamedTempFile,
    pub sha1_hex: String,
}
