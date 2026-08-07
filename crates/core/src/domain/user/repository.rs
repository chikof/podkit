use crate::domain::shared::errors::DomainResult;
use crate::domain::shared::ids::UserId;
use crate::domain::user::entity::User;
use async_trait::async_trait;

/// Persistence contract for [`User`], implemented by the storage adapter.
#[async_trait]
pub trait UserRepository: Send + Sync {
	/// Looks up a user by id.
	async fn find_by_id(&self, id: UserId) -> DomainResult<Option<User>>;

	/// Looks up a user by email.
	async fn find_by_email(&self, email: &str) -> DomainResult<Option<User>>;

	// I did not use my Email type because... what if someone just manually inserted an email directly into the database? :sob:
	/// Checks whether any user is registered with this email, without loading the full row.
	async fn exists_by_email(&self, email: &str) -> DomainResult<bool>;

	/// Inserts or updates a user.
	async fn save(&self, user: &User) -> DomainResult<()>;

	/// Deletes a user by id.
	async fn delete(&self, id: UserId) -> DomainResult<()>;
}
