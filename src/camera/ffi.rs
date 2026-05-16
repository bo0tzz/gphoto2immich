//! Raw FFI bindings to libfuji / libpict.
//!
//! The `sys` submodule holds the bindgen-generated unsafe bindings verbatim.
//! Higher-level lifecycle types live in `super::mod`.

#![allow(non_upper_case_globals, non_camel_case_types, non_snake_case, dead_code)]

pub mod sys {
    include!(concat!(env!("OUT_DIR"), "/bindings.rs"));
}
