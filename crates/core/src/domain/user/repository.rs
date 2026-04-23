use crate::domain::shared::errors::DomainResult;
use crate::domain::shared::ids::UserId;
use crate::domain::user::entity::User;
use async_trait::async_trait;

#[async_trait]
pub trait UserRepository: Send + Sync {
	async fn find_by_id(&self, id: UserId) -> DomainResult<Option<User>>;

	async fn find_by_email(&self, email: &str) -> DomainResult<Option<User>>;

	// I did not use my Email type because... what if someone just manually inserted an email directly into the database? :sob:
	async fn exists_by_email(&self, email: &str) -> DomainResult<bool>;

	async fn save(&self, user: &User) -> DomainResult<()>;

	async fn delete(&self, id: UserId) -> DomainResult<()>;
}
