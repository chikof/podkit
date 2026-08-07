use crate::domain::shared::ids::PermissionId;

/// A single `(action, resource)` grant that can be attached to a role.
#[derive(Debug, Clone)]
pub struct Permission {
	/// Unique id of this permission.
	pub id: PermissionId,
	/// The action allowed, e.g. "deploy" or "delete".
	pub action: String,
	/// The resource it applies to, e.g. "application" or "server".
	pub resource: String,
}
