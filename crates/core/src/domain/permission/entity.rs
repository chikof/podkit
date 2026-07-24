use crate::domain::shared::ids::PermissionId;

#[derive(Debug, Clone)]
pub struct Permission {
	pub id: PermissionId,
	pub action: String,
	pub resource: String,
}
