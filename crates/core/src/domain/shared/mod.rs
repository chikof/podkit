//! Cross-cutting types used by every domain module: ids, errors, and the
//! authorization contract.

/// Authorization contract used to check whether a user can act on a team's resources.
pub mod authorization;
/// Shared domain error type and result alias.
pub mod errors;
/// Newtype ids for every entity in the domain.
pub mod ids;
