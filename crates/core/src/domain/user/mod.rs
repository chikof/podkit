/// The `User` entity.
pub mod entity;
/// The `PasswordHasher` trait.
pub mod password_hasher;
/// Persistence contract for users.
pub mod repository;
/// Account-level value objects: email, password hash, settings.
pub mod value_objects;

// entity
pub use entity::User;

// services?
pub use password_hasher::PasswordHasher;
pub use repository::UserRepository;
