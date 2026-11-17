//! Snowflake id newtypes. Each wraps the `BIGINT` primary key of one table so
//! handlers and store fns cannot mix ids of different resources.

macro_rules! id {
	($($name:ident),+ $(,)?) => {$(
		#[derive(
			Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize, sqlx::Type,
		)]
		#[serde(transparent)]
		#[sqlx(transparent)]
		pub struct $name(pub i64);

		impl $name {
			#[allow(dead_code)]
			pub fn generate() -> Self {
				Self(crypto::generate_id())
			}
		}

		impl std::fmt::Display for $name {
			fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
				self.0.fmt(f)
			}
		}
	)+};
}

id!(
	UserId,
	TeamId,
	TeamMemberId,
	RoleId,
	ProjectId,
	ServerId,
	ApplicationId,
	DeploymentId,
	EnvVarId,
	DomainId,
	VolumeId,
	DatabaseId,
	BackupTargetId,
	BackupId,
	ModelFileId,
	JobId,
);
