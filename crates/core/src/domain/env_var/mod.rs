/// The `EnvVar` entity.
pub mod entity;
/// Persistence contract for env vars.
pub mod repository;

pub use entity::EnvVar;
pub use repository::EnvVarRepository;
