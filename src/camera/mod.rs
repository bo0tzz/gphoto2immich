//! Camera-side glue: FFI bindings to libfuji, weak-symbol overrides, helpers
//! for `PtpObjectInfo`, and (in later phases) the connection / event-poll loop.

pub mod callbacks;
pub mod ffi;
pub mod object_info;

pub use callbacks::set_client_name;
pub use object_info::{AssetKind, ObjectInfo};
