use time::OffsetDateTime;
use crate::domain::shared::ids::{RoleId, UserId, WorkspaceId};

#[derive(Debug, Clone)]
pub struct Membership {
    pub user_id: UserId,
    pub workspace_id: WorkspaceId,
    pub roles: Vec<RoleId>,
    pub joined_at: OffsetDateTime,
}

impl Membership {
    pub fn new(user_id: UserId, workspace_id: WorkspaceId) -> Self {
        Self {
            user_id,
            workspace_id,
            roles: Vec::new(),
            joined_at: OffsetDateTime::now_utc(),
        }
    }

    pub fn assign_role(
        &mut self,
        role_id: RoleId,
    ) -> Result<(), crate::domain::shared::errors::DomainError> {
        use crate::domain::shared::errors::DomainError;
        if self.roles.contains(&role_id) {
            return Err(DomainError::AlreadyAssigned);
        }
        self.roles.push(role_id);
        Ok(())
    }

    pub fn revoke_role(&mut self, role_id: RoleId) {
        self.roles.retain(|r| r != &role_id);
    }

    pub fn has_role(&self, role_id: RoleId) -> bool {
        self.roles.contains(&role_id)
    }
}