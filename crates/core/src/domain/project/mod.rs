//! Project aggregate: groups a team's applications, plus its persistence contract.

/// The `Project` entity.
pub mod entity;
/// Persistence contract for projects.
pub mod repository;
pub use entity::Project;
