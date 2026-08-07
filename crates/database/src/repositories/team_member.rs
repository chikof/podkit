use async_trait::async_trait;
use podkit_core::domain::shared::errors::{DomainError, DomainResult};
use podkit_core::domain::shared::ids::{RoleId, TeamId, TeamMemberId, UserId};
use podkit_core::domain::team_member::TeamMember;
use podkit_core::domain::team_member::repository::TeamMemberRepository;

use crate::PgPool;
use crate::models::team_members::{NewTeamMember, TeamMemberModel};

/// Postgres-backed `TeamMemberRepository`.
pub struct PgTeamMemberRepository(pub &'static PgPool);

fn to_infra(e: &crate::DatabaseError) -> DomainError {
	DomainError::Infrastructure(e.to_string())
}

fn map_member(row: &TeamMemberModel) -> TeamMember {
	TeamMember {
		id: TeamMemberId(row.id),
		team_id: TeamId(row.team_id),
		user_id: UserId(row.user_id),
		role_id: RoleId(row.role_id),
		joined_at: row.joined_at,
	}
}

#[async_trait]
impl TeamMemberRepository for PgTeamMemberRepository {
	async fn find_by_team_and_user(
		&self,
		team_id: TeamId,
		user_id: UserId,
	) -> DomainResult<Option<TeamMember>> {
		Ok(
			TeamMemberModel::find_by_team_and_user(self.0, team_id.0, user_id.0)
				.await
				.map_err(|e| to_infra(&e))?
				.map(|e| map_member(&e)),
		)
	}

	async fn list_by_team(&self, team_id: TeamId) -> DomainResult<Vec<TeamMember>> {
		Ok(TeamMemberModel::list_by_team(self.0, team_id.0)
			.await
			.map_err(|e| to_infra(&e))?
			.into_iter()
			.map(|e| map_member(&e))
			.collect())
	}

	async fn save(&self, member: &TeamMember) -> DomainResult<()> {
		TeamMemberModel::create(
			self.0,
			NewTeamMember {
				id: member.id.0,
				team_id: member.team_id.0,
				user_id: member.user_id.0,
				role_id: member.role_id.0,
			},
		)
		.await
		.map_err(|e| to_infra(&e))?;
		Ok(())
	}

	async fn delete(&self, team_id: TeamId, user_id: UserId) -> DomainResult<()> {
		TeamMemberModel::delete(self.0, team_id.0, user_id.0)
			.await
			.map_err(|e| to_infra(&e))
	}
}
