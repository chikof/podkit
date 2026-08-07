use async_trait::async_trait;
use podkit_core::domain::env_var::entity::EnvVar;
use podkit_core::domain::env_var::repository::EnvVarRepository;
use podkit_core::domain::shared::errors::{DomainError, DomainResult};
use podkit_core::domain::shared::ids::{ApplicationId, EnvVarId};

use crate::PgPool;
use crate::models::env_var::{EnvVarModel, NewEnvVar};

/// Postgres-backed `EnvVarRepository`.
pub struct PgEnvVarRepository(pub &'static PgPool);

fn to_infra(e: &crate::DatabaseError) -> DomainError {
	DomainError::Infrastructure(e.to_string())
}

fn map_env_var(row: EnvVarModel) -> EnvVar {
	EnvVar {
		id: EnvVarId(row.id),
		application_id: ApplicationId(row.application_id),
		key: row.key,
		value: row.value,
		created_at: row.created_at,
		updated_at: row.updated_at,
	}
}

#[async_trait]
impl EnvVarRepository for PgEnvVarRepository {
	async fn list_by_application(
		&self,
		application_id: ApplicationId,
	) -> DomainResult<Vec<EnvVar>> {
		Ok(EnvVarModel::list_by_application(self.0, application_id.0)
			.await
			.map_err(|e| to_infra(&e))?
			.into_iter()
			.map(map_env_var)
			.collect())
	}

	async fn find_by_id(&self, id: EnvVarId) -> DomainResult<Option<EnvVar>> {
		Ok(EnvVarModel::find_by_id(self.0, id.0)
			.await
			.map_err(|e| to_infra(&e))?
			.map(map_env_var))
	}

	async fn upsert(&self, env_var: &EnvVar) -> DomainResult<EnvVar> {
		let row = EnvVarModel::upsert(
			self.0,
			NewEnvVar {
				id: env_var.id.0,
				application_id: env_var.application_id.0,
				key: env_var.key.clone(),
				value: env_var.value.clone(),
			},
		)
		.await
		.map_err(|e| to_infra(&e))?;
		Ok(map_env_var(row))
	}

	async fn delete(&self, id: EnvVarId) -> DomainResult<()> {
		EnvVarModel::delete(self.0, id.0)
			.await
			.map_err(|e| to_infra(&e))
	}
}
