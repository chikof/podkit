//! Server aggregate: a podman host a team can deploy to, plus its
//! persistence contract.

/// The `Server` entity and its `ServerStatus`.
pub mod entity;
/// Persistence contract for servers.
pub mod repository;

pub use entity::{Server, ServerStatus};
pub use repository::ServerRepository;
