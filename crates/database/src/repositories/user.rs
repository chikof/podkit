use async_trait::async_trait;
use podkit_core::domain::shared::errors::{DomainError, DomainResult};
use podkit_core::domain::shared::ids::UserId;
use podkit_core::domain::user::entity::User;
use podkit_core::domain::user::repository::UserRepository;
use podkit_core::domain::user::value_objects::{Email, PasswordHash, UserSettings};

use crate::PgPool;
use crate::models::user::{NewUser, UserModel};

pub struct PgUserRepository(pub &'static PgPool);

fn to_infra(e: &crate::DatabaseError) -> DomainError {
	DomainError::Infrastructure(e.to_string())
}

fn map_user(row: UserModel) -> DomainResult<User> {
	let email = Email::new(&row.email).map_err(|e| DomainError::Infrastructure(e.to_string()))?;
	Ok(User {
		id: UserId(row.id),
		email,
		password_hash: PasswordHash::new(row.password_hash),
		display_name: row.name,
		settings: UserSettings::default(),
		created_at: row.created_at,
		updated_at: row.updated_at,
	})
}

#[async_trait]
impl UserRepository for PgUserRepository {
	async fn find_by_id(&self, id: UserId) -> DomainResult<Option<User>> {
		UserModel::find_by_id(self.0, id.0)
			.await
			.map_err(|e| to_infra(&e))?
			.map(map_user)
			.transpose()
	}

	async fn find_by_email(&self, email: &str) -> DomainResult<Option<User>> {
		UserModel::find_by_email(self.0, email)
			.await
			.map_err(|e| to_infra(&e))?
			.map(map_user)
			.transpose()
	}

	async fn exists_by_email(&self, email: &str) -> DomainResult<bool> {
		UserModel::exists_by_email(self.0, email)
			.await
			.map_err(|e| to_infra(&e))
	}

	async fn save(&self, user: &User) -> DomainResult<()> {
		UserModel::create(
			self.0,
			NewUser {
				id: user.id.0,
				email: user.email.as_str().to_string(),
				name: user.display_name.clone(),
				password_hash: user.password_hash.as_str().to_string(),
			},
		)
		.await
		.map_err(|e| to_infra(&e))?;
		Ok(())
	}

	async fn delete(&self, id: UserId) -> DomainResult<()> {
		UserModel::delete(self.0, id.0)
			.await
			.map_err(|e| to_infra(&e))
	}
}
