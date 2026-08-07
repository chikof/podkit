//! Core domain crate: entities, repository traits, and validation shared by
//! every podkit service.

/// The domain model: entities, repository traits, and value objects.
pub mod domain;
/// Standalone input validation (email, etc), not tied to any entity.
pub mod validation; // temporary here!
// extra comment xd
