use time::OffsetDateTime;
use crate::domain::shared::ids::{WorkspaceId, UserId};

#[derive(Debug, Clone)]
pub struct Workspace {
    pub id: WorkspaceId,
    pub name: String,
    pub slug: String,
    pub owner_id: UserId,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime
}

impl Workspace {
    pub fn new(
        id: WorkspaceId,
        name: String,
        slug: String,
        owner_id: UserId
    ) -> Self {
        let now = OffsetDateTime::now_utc();
        Self { id, name, slug, owner_id, created_at: now, updated_at: now }
    }
}