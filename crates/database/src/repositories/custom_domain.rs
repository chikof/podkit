use async_trait::async_trait;
use podkit_core::domain::custom_domain::entity::CustomDomain;
use podkit_core::domain::custom_domain::repository::CustomDomainRepository;
use podkit_core::domain::shared::errors::{DomainError, DomainResult};
use podkit_core::domain::shared::ids::{ApplicationId, CustomDomainId};

use crate::PgPool;
use crate::models::custom_domain::{CustomDomainModel, NewCustomDomain};

/// Postgres-backed `CustomDomainRepository`.
pub struct PgCustomDomainRepository(pub &'static PgPool);

fn to_infra(e: &crate::DatabaseError) -> DomainError {
	if let crate::DatabaseError::ConnectionError(sqlx_err) = e
		&& sqlx_err
			.as_database_error()
			.is_some_and(sqlx::error::DatabaseError::is_unique_violation)
	{
		return DomainError::AlreadyExists;
	}
	DomainError::Infrastructure(e.to_string())
}

fn map_domain(row: CustomDomainModel) -> CustomDomain {
	CustomDomain {
		id: CustomDomainId(row.id),
		application_id: ApplicationId(row.application_id),
		hostname: row.hostname,
		created_at: row.created_at,
		updated_at: row.updated_at,
	}
}

#[async_trait]
impl CustomDomainRepository for PgCustomDomainRepository {
	async fn find_by_id(&self, id: CustomDomainId) -> DomainResult<Option<CustomDomain>> {
		Ok(CustomDomainModel::find_by_id(self.0, id.0)
			.await
			.map_err(|e| to_infra(&e))?
			.map(map_domain))
	}

	async fn list_by_application(
		&self,
		application_id: ApplicationId,
	) -> DomainResult<Vec<CustomDomain>> {
		Ok(
			CustomDomainModel::list_by_application(self.0, application_id.0)
				.await
				.map_err(|e| to_infra(&e))?
				.into_iter()
				.map(map_domain)
				.collect(),
		)
	}

	async fn save(&self, domain: &CustomDomain) -> DomainResult<()> {
		CustomDomainModel::create(
			self.0,
			NewCustomDomain {
				id: domain.id.0,
				application_id: domain.application_id.0,
				hostname: domain.hostname.clone(),
			},
		)
		.await
		.map_err(|e| to_infra(&e))?;
		Ok(())
	}

	async fn delete(&self, id: CustomDomainId) -> DomainResult<()> {
		CustomDomainModel::delete(self.0, id.0)
			.await
			.map_err(|e| to_infra(&e))
	}
}
