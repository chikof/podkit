use async_trait::async_trait;

use crate::domain::application::entity::Application;
use crate::domain::shared::errors::DomainResult;
use crate::domain::shared::ids::{ApplicationId, ProjectId, ServerId};

/// Persistence contract for [`Application`], implemented by the storage adapter.
#[async_trait]
pub trait ApplicationRepository: Send + Sync {
	/// Looks up an application by id.
	async fn find_by_id(&self, id: ApplicationId) -> DomainResult<Option<Application>>;

	/// Lists all applications belonging to a project.
	async fn list_by_project(&self, project_id: ProjectId) -> DomainResult<Vec<Application>>;

	/// Lists all applications deployed on a server.
	async fn list_by_server(&self, server_id: ServerId) -> DomainResult<Vec<Application>>;

	/// Inserts a new application.
	async fn save(&self, application: &Application) -> DomainResult<()>;

	/// Persists changes to an existing application.
	async fn update(&self, application: &Application) -> DomainResult<()>;

	/// Deletes an application by id.
	async fn delete(&self, id: ApplicationId) -> DomainResult<()>;
}
