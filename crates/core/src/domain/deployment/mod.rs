//! Deployment aggregate: one attempt to build and run an application at a
//! given commit, plus its persistence contract.

/// The `Deployment` entity and its `DeploymentStatus`.
pub mod entity;
/// Persistence contract for deployments.
pub mod repository;

pub use entity::{Deployment, DeploymentStatus};
pub use repository::DeploymentRepository;
