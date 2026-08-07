//! Row type and queries for the `permissions` table.
//!
//! Mirrors [`podkit_core::domain::permission::entity::Permission`] field for field.
//! These rows are seeded by migration, not created at runtime.

use sqlx::prelude::FromRow;

use crate::{DatabaseError, DbExecutor};

/// Row from the `permissions` table.
#[derive(Debug, Clone, FromRow)]
pub struct PermissionModel {
	/// Primary key, matches the domain entity's id.
	pub id: i64,
	/// The action being permitted, e.g. `"create"`.
	pub action: String,
	/// The resource type it applies to, e.g. `"application"`.
	pub resource: String,
}

impl PermissionModel {
	/// Lists every permission in the system.
	///
	/// # Errors
	/// Returns an error if the query fails.
	pub async fn list_all<'e>(exec: impl DbExecutor<'e>) -> Result<Vec<Self>, DatabaseError> {
		Ok(sqlx::query_as!(Self, "SELECT * FROM permissions")
			.fetch_all(exec)
			.await?)
	}

	/// Looks up a permission by its `(action, resource)` pair.
	///
	/// # Errors
	/// Returns an error if the query fails.
	pub async fn find_by_action_resource<'e>(
		exec: impl DbExecutor<'e>,
		action: &str,
		resource: &str,
	) -> Result<Option<Self>, DatabaseError> {
		Ok(sqlx::query_as!(
			Self,
			"SELECT * FROM permissions WHERE action = $1 AND resource = $2",
			action,
			resource
		)
		.fetch_optional(exec)
		.await?)
	}
}
