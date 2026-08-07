//! Application aggregate: a deployable unit backed by a git repo, plus its
//! persistence contract.

/// The `Application` entity and its `BuildStrategy`.
pub mod entity;
/// Persistence contract for applications.
pub mod repository;

pub use entity::{Application, BuildStrategy};
pub use repository::ApplicationRepository;
