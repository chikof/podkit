use time::OffsetDateTime;
use crate::domain::shared::ids::{TeamId, UserId, WorkspaceId};

#[derive(Debug, Clone)]
pub struct Team {
    pub id: TeamId,
    pub workspace_id: WorkspaceId,
    pub name: String,
    pub members: Vec<UserId>,
    pub created_at: OffsetDateTime,
}

impl Team {
    pub fn new(id: TeamId, workspace_id: WorkspaceId, name: String) -> Self {
        Self {
            id,
            workspace_id,
            name,
            members: Vec::new(),
            created_at: OffsetDateTime::now_utc(),
        }
    }

    pub fn add_member(
        &mut self,
        user_id: UserId,
    ) -> Result<(), crate::domain::shared::errors::DomainError> {
        use crate::domain::shared::errors::DomainError;
        if self.members.contains(&user_id) {
            return Err(DomainError::AlreadyAssigned);
        }
        self.members.push(user_id);
        Ok(())
    }

    pub fn remove_member(&mut self, user_id: UserId) {
        self.members.retain(|u| u != &user_id);
    }
}