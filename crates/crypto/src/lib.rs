//! Crypto primitives podkit needs: encrypting secrets at rest, hashing
//! passwords, generating ids, and generating/comparing bearer tokens.

/// Encrypting secrets at rest with age.
pub mod age;
/// Hashing and verifying user passwords with argon2.
pub mod argon2;
/// Generating snowflake-style ids.
pub mod snowflake;
/// Generating and comparing bearer tokens.
pub mod token;

pub use argon2::{DUMMY_HASH, hash, verify};
pub use snowflake::generate_id;
pub use token::{constant_time_eq, generate_token};
