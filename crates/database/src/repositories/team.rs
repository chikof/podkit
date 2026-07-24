use async_trait::async_trait;
use podkit_core::domain::shared::errors::{DomainError, DomainResult};
use podkit_core::domain::shared::ids::TeamId;
use podkit_core::domain::team::Team;
use podkit_core::domain::team::repository::TeamRepository;

use crate::PgPool;
use crate::models::team::{NewTeam, TeamModel};

pub struct PgTeamRepository(pub &'static PgPool);

fn to_infra(e: &crate::DatabaseError) -> DomainError {
	DomainError::Infrastructure(e.to_string())
}

fn map_team(row: TeamModel) -> Team {
	Team {
		id: TeamId(row.id),
		name: row.name,
		slug: row.slug,
		logo: row.logo,
		created_at: row.created_at,
		updated_at: row.updated_at,
	}
}

#[async_trait]
impl TeamRepository for PgTeamRepository {
	async fn find_by_id(&self, id: TeamId) -> DomainResult<Option<Team>> {
		Ok(TeamModel::find_by_id(self.0, id.0)
			.await
			.map_err(|e| to_infra(&e))?
			.map(map_team))
	}

	async fn find_by_slug(&self, slug: &str) -> DomainResult<Option<Team>> {
		Ok(TeamModel::find_by_slug(self.0, slug)
			.await
			.map_err(|e| to_infra(&e))?
			.map(map_team))
	}

	async fn save(&self, team: &Team) -> DomainResult<()> {
		TeamModel::create(
			self.0,
			NewTeam {
				id: team.id.0,
				name: team.name.clone(),
				slug: team.slug.clone(),
				logo: team.logo.clone(),
			},
		)
		.await
		.map_err(|e| to_infra(&e))?;
		Ok(())
	}

	async fn delete(&self, id: TeamId) -> DomainResult<()> {
		TeamModel::delete(self.0, id.0)
			.await
			.map_err(|e| to_infra(&e))
	}
}
