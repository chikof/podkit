use async_trait::async_trait;

use crate::domain::deployment::entity::Deployment;
use crate::domain::shared::errors::DomainResult;
use crate::domain::shared::ids::{ApplicationId, DeploymentId};

/// Persistence contract for [`Deployment`], implemented by the storage adapter.
#[async_trait]
pub trait DeploymentRepository: Send + Sync {
	/// Looks up a deployment by id.
	async fn find_by_id(&self, id: DeploymentId) -> DomainResult<Option<Deployment>>;

	/// Lists all deployment attempts for an application, newest and oldest alike.
	async fn list_by_application(
		&self,
		application_id: ApplicationId,
	) -> DomainResult<Vec<Deployment>>;

	/// System-wide, every application. Used by the health monitor to find
	/// containers it needs to watch. Small enough to list wholesale at
	/// podkit's expected scale; revisit if that stops being true.
	async fn list_running(&self) -> DomainResult<Vec<Deployment>>;

	/// Inserts a new deployment row. Each retry is a fresh attempt: this
	/// always creates a new row, it never mutates an old one.
	async fn save(&self, deployment: &Deployment) -> DomainResult<()>;

	/// Persists an in-place update to an existing row's mutable fields
	/// (status + the fields that change alongside it) as one attempt
	/// progresses through its own lifecycle.
	async fn update(&self, deployment: &Deployment) -> DomainResult<()>;
}
