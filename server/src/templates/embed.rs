//! The files `build.rs` found under `server/templates` and
//! `server/static/logos`, compiled into the binary.

include!(concat!(env!("OUT_DIR"), "/embedded.rs"));
