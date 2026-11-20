//! Infrastructure as code: a project, or a whole team's setup, as a `podkit.toml` that lives in
//! git. Export it, edit it, plan it against a project to see what would change, apply it.
//!
//! - [`doc`] is the file: versioned, keyed by names, deterministic, with no secrets in it.
//! - [`objects`] flattens a file (or the project as it is) into comparable fields.
//! - [`plan`] compares the two: create, update, delete or unchanged per object, with the
//!   fields, and whether applying replaces something that runs.
//! - [`snapshot`] reads the project as it is.
//! - [`apply`] runs a plan through the same services the pages and the API use.
//! - [`service`] authorizes all of it, for the JSON API and the web UI alike.

pub mod apply;
pub mod doc;
pub mod objects;
pub mod plan;
mod routes;
pub mod service;
pub mod snapshot;

pub use routes::{import_router, router};

#[cfg(test)]
mod tests;
