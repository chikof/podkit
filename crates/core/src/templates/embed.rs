//! The files `build.rs` found under `crates/core/templates` and
//! `crates/core/static/logos`, compiled into the binary.

include!(concat!(env!("OUT_DIR"), "/embedded.rs"));
