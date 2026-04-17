pub mod connection;
pub mod models;

pub trait DbExecutor<'e>: sqlx::Executor<'e, Database = sqlx::Postgres> {}
impl<'e, T: sqlx::Executor<'e, Database = sqlx::Postgres>> DbExecutor<'e> for T {}
