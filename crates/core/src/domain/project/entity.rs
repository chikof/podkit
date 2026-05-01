use time::OffsetDateTime;
use crate::domain::shared::ids::{ProjectId, TeamId, WorkspaceId};

#[derive(Debug, Clone)]
pub struct Project {
    pub id: ProjectId,
    pub workspace_id: WorkspaceId,
    pub team_id: Option<TeamId>,
    pub name: String,
    pub status: ProjectStatus,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectStatus {
    Active,
    Stopped,
    Deploying,
    Error,
}

impl Project {
    pub fn new(
        id: ProjectId,
        workspace_id: WorkspaceId,
        name: String,
        team_id: Option<TeamId>,
    ) -> Self {
        let now = OffsetDateTime::now_utc();
        Self {
            id,
            workspace_id,
            team_id,
            name,
            status: ProjectStatus::Active,
            created_at: now,
            updated_at: now,
        }
    }
}