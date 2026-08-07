//! Postgres persistence layer for podkit: connection setup, sqlx row types,
//! and the repository trait implementations that back `podkit_core`'s
//! domain repositories.

/// Connection pool setup and migrations.
pub mod connection;
/// The crate's error type.
pub mod error;
/// Argon2 password hasher implementing `podkit_core`'s `PasswordHasher` trait.
pub mod hashing;
/// sqlx row types, one module per table.
pub mod models;
/// Postgres implementations of the domain repository traits.
pub mod repositories;

pub use error::DatabaseError;
pub use sqlx::PgPool;

/// Anything sqlx can run a query against: a pool, a connection, or a
/// transaction. Lets query helpers accept whichever the caller has on hand.
pub trait DbExecutor<'e>: sqlx::Executor<'e, Database = sqlx::Postgres> {}
impl<'e, T: sqlx::Executor<'e, Database = sqlx::Postgres>> DbExecutor<'e> for T {}
