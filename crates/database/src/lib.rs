pub mod connection;
pub mod error;
pub mod hashing;
pub mod models;
pub mod repositories;

pub use error::DatabaseError;
pub use sqlx::PgPool;

pub trait DbExecutor<'e>: sqlx::Executor<'e, Database = sqlx::Postgres> {}
impl<'e, T: sqlx::Executor<'e, Database = sqlx::Postgres>> DbExecutor<'e> for T {}
