//! Camera side of the daemon. The libgphoto2 wrapper module will be added
//! in the next commit; for now this just re-exports the shared types used by
//! the pipeline and stack tracker.

pub mod object_info;

pub use object_info::{AssetKind, ObjectInfo};
