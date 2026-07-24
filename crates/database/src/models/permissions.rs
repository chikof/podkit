use sqlx::prelude::FromRow;

use crate::{DatabaseError, DbExecutor};

#[derive(Debug, Clone, FromRow)]
pub struct PermissionModel {
	pub id: i64,
	pub action: String,
	pub resource: String,
}

impl PermissionModel {
	pub async fn list_all<'e>(exec: impl DbExecutor<'e>) -> Result<Vec<Self>, DatabaseError> {
		Ok(sqlx::query_as!(Self, "SELECT * FROM permissions")
			.fetch_all(exec)
			.await?)
	}

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
