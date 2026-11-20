//! Which volume a backup is about: an application's volume or a stack's, resolved
//! inside the caller's team so another team's volume is simply not found.

use sqlx::PgPool;

use crate::applications::{self, Application};
use crate::error::{AppResult, ServerError};
use crate::ids::{ApplicationId, ProjectId, ServerId, StackId, TeamId, VolumeId};
use crate::stacks::compose::{self, Inputs};
use crate::stacks::{self, Stack, translate};
use crate::state::AppState;
use crate::{scope, volumes};

/// The volume a request names. An application volume is a row; a stack volume is a
/// name in its compose file.
#[derive(Debug, Clone)]
pub enum Vol {
	App {
		app: ApplicationId,
		volume: VolumeId,
	},
	Stack {
		stack: StackId,
		/// The volume's name in the compose file.
		key: String,
	},
}

/// Who runs on the volume.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Owner {
	App(ApplicationId),
	Stack(StackId),
}

/// A resolved volume.
#[derive(Debug, Clone)]
pub struct Subject {
	pub owner: Owner,
	pub project: ProjectId,
	pub server: ServerId,
	/// Name of the application or stack, for messages.
	pub owner_name: String,
	/// Podman name of the volume.
	pub volume_name: String,
	/// Mount path (application) or compose name (stack).
	pub label: String,
	pub volume_id: Option<VolumeId>,
	pub stack_id: Option<StackId>,
}

impl Subject {
	/// The job lock key that serializes work on the same owner: deploys of an
	/// application and of a stack use the same ones.
	#[must_use]
	pub fn lock_key(&self) -> String {
		match self.owner {
			Owner::App(id) => format!("app:{id}"),
			Owner::Stack(id) => format!("stack:{id}"),
		}
	}

	fn app(app: &Application, volume: &volumes::Volume) -> Self {
		Self {
			owner: Owner::App(app.id),
			project: app.project_id,
			server: app.server_id,
			owner_name: app.name.clone(),
			volume_name: volume.name.clone(),
			label: volume.mount_path.clone(),
			volume_id: Some(volume.id),
			stack_id: None,
		}
	}

	fn stack(stack: &Stack, key: &str) -> Self {
		Self {
			owner: Owner::Stack(stack.id),
			project: stack.project_id,
			server: stack.server_id,
			owner_name: stack.name.clone(),
			volume_name: translate::volume_name(stack.id.0, key),
			label: key.to_owned(),
			volume_id: None,
			stack_id: Some(stack.id),
		}
	}
}

/// The compose names of the volumes a stack uses; none when its file no longer
/// resolves.
#[must_use]
pub fn stack_volume_keys(state: &AppState, stack: &Stack) -> Vec<String> {
	let env = stacks::decrypt_env(&state.secrets, stack).unwrap_or_default();
	let files = stacks::decrypt_files(&state.secrets, stack).unwrap_or_default();
	compose::parse(
		&stack.compose_yaml,
		&Inputs {
			env: &env,
			files: &files,
		},
	)
	.map(|c| c.volumes.into_iter().collect())
	.unwrap_or_default()
}

/// Every volume of an application, each resolved.
///
/// # Errors
/// The application is not in the team's project.
pub async fn of_application(
	db: &PgPool,
	team: TeamId,
	project: ProjectId,
	app: ApplicationId,
) -> AppResult<Vec<Subject>> {
	let app = scope::application(db, team, project, app).await?;
	Ok(volumes::list(db, app.id)
		.await?
		.iter()
		.map(|v| Subject::app(&app, v))
		.collect())
}

/// Every volume of a stack's compose file, each resolved.
///
/// # Errors
/// The stack is not in the team's project.
pub async fn of_stack(
	state: &AppState,
	team: TeamId,
	project: ProjectId,
	stack: StackId,
) -> AppResult<Vec<Subject>> {
	let stack = scope::stack(&state.db, team, project, stack).await?;
	Ok(stack_volume_keys(state, &stack)
		.iter()
		.map(|key| Subject::stack(&stack, key))
		.collect())
}

/// One volume, in the team's project.
///
/// # Errors
/// Not found: the owner is not in the team's project, or has no such volume.
pub async fn resolve(
	state: &AppState,
	team: TeamId,
	project: ProjectId,
	vol: &Vol,
) -> AppResult<Subject> {
	match vol {
		Vol::App { app, volume } => {
			let app = scope::application(&state.db, team, project, *app).await?;
			let volume = volumes::find(&state.db, app.id, *volume)
				.await?
				.ok_or(ServerError::VolumeNotFound)?;
			Ok(Subject::app(&app, &volume))
		}
		Vol::Stack { stack, key } => {
			let stack = scope::stack(&state.db, team, project, *stack).await?;
			if !stack_volume_keys(state, &stack).contains(key) {
				return Err(ServerError::VolumeNotFound);
			}
			Ok(Subject::stack(&stack, key))
		}
	}
}

/// The volume a stored policy is about, for jobs and the scheduler, which run
/// without a team. `None` when its owner or volume is gone.
///
/// # Errors
/// A database error.
pub async fn of_policy(
	state: &AppState,
	policy: &super::Policy,
) -> Result<Option<Subject>, sqlx::Error> {
	if let Some(volume_id) = policy.volume_id {
		let Some(app_id) = sqlx::query_scalar::<_, ApplicationId>(
			"SELECT application_id FROM volumes WHERE id = $1",
		)
		.bind(volume_id)
		.fetch_optional(&state.db)
		.await?
		else {
			return Ok(None);
		};
		let Some(app) = applications::find_by_id(&state.db, app_id).await? else {
			return Ok(None);
		};
		let Some(volume) = volumes::find(&state.db, app.id, volume_id).await? else {
			return Ok(None);
		};
		return Ok(Some(Subject::app(&app, &volume)));
	}
	let Some(stack_id) = policy.stack_id else {
		return Ok(None);
	};
	let Some(stack) = stacks::find_by_id(&state.db, stack_id).await? else {
		return Ok(None);
	};
	Ok(Some(Subject::stack(&stack, &policy.label)))
}

/// `[A-Za-z0-9][A-Za-z0-9_.-]*`, at most 60: what a compose volume may be called, and
/// so what a restore may name a new stack volume.
#[must_use]
pub fn valid_volume_key(key: &str) -> bool {
	!key.is_empty()
		&& key.len() <= 60
		&& key.starts_with(|c: char| c.is_ascii_alphanumeric())
		&& key
			.bytes()
			.all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
}
