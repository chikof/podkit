//! Writes the clone's configuration: every resource row, in the caller's transaction.
//!
//! Nothing here starts a container. Ids are fresh, names and slugs are the source's with
//! the new environment's slug as a suffix, and what belongs to the source alone is left
//! out: custom domains, webhooks and git hooks, backup schedules, public database ports.

use std::collections::BTreeMap;

use sqlx::PgConnection;

use super::plan::{Snapshot, suffixed};
use super::{Kind, Options, Pair, Secrets};
use crate::applications::{self, NewApplication};
use crate::databases::{self, Credentials, NewDatabase};
use crate::environments::{self, Environment};
use crate::error::{AppResult, ServerError};
use crate::ids::{ApplicationId, DatabaseId, ModelId, ProjectId, StackId, TeamId, UserId};
use crate::models::{self, ModelStatus, NewModel};
use crate::networks::{self, ResourceKind};
use crate::shared_env::{self, Scope};
use crate::stacks::{self, NewStack};
use crate::state::AppState;
use crate::{env_vars, volumes};

const MAX_LEN: usize = 60;

fn internal(what: &str, e: impl std::fmt::Display) -> ServerError {
	tracing::error!("environment clone: {what}: {e}");
	ServerError::Internal
}

/// Copies `snap` into the new environment `name` / `slug` of `project`. Returns the
/// environment and which new resource came from which.
///
/// # Errors
/// A unique violation (a name or slug is taken), or a database or encryption failure;
/// the caller rolls the transaction back.
#[allow(
	clippy::too_many_lines,
	clippy::too_many_arguments,
	reason = "one block per kind of resource, and everything the copy needs is passed in"
)]
pub async fn apply(
	s: &AppState,
	conn: &mut PgConnection,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	snap: &Snapshot,
	name: &str,
	slug: &str,
	options: &Options,
) -> AppResult<(Environment, Vec<Pair>)> {
	let blank = options.secrets == Secrets::Blank;
	let empty = if blank {
		Some(s.secrets.encrypt("").map_err(|e| internal("encrypt", e))?)
	} else {
		None
	};
	let env = environments::insert(&mut *conn, project, name, slug).await?;
	let mut pairs = Vec::new();

	for a in &snap.apps {
		let old = &a.app;
		let new = applications::insert(
			&mut *conn,
			NewApplication {
				id: ApplicationId::generate(),
				project_id: project,
				environment_id: env.id,
				server_id: old.server_id,
				name: suffixed(&old.name, slug, MAX_LEN),
				slug: suffixed(&old.slug, slug, MAX_LEN),
				repo_url: old.repo_url.clone(),
				git_ref: old.git_ref.clone(),
				source_kind: old.source_kind,
				image: old.image.clone(),
				registry_auth: if blank {
					None
				} else {
					old.registry_auth.clone()
				},
				deploy_key_id: old.deploy_key_id,
				registry_id: old.registry_id,
				git_token: if blank { None } else { old.git_token.clone() },
				build_strategy: old.build_strategy,
				dockerfile_path: old.dockerfile_path.clone(),
				container_port: old.container_port,
				// The source's webhook would deploy the clone too; a clone starts without one.
				webhook_secret: None,
				memory_limit_mb: old.memory_limit_mb,
				cpu_limit: old.cpu_limit,
				git_provider_id: old.git_provider_id,
				git_repo: old.git_repo.clone(),
			},
		)
		.await?;
		for v in &a.env_vars {
			let value = empty.as_deref().unwrap_or(&v.value);
			env_vars::upsert(&mut *conn, new.id, &v.key, value).await?;
		}
		for v in &a.volumes {
			volumes::insert(&mut *conn, new.id, &v.mount_path).await?;
		}
		for n in &a.networks {
			networks::upsert_member(&mut *conn, *n, ResourceKind::Application, new.id.0, &[])
				.await?;
		}
		pairs.push(Pair {
			kind: Kind::Application,
			source: old.id.0,
			id: new.id.0,
		});
	}

	for d in &snap.databases {
		let old = &d.db;
		let credentials = serde_json::to_string(&Credentials::generate())
			.map_err(|e| internal("credentials", e))?;
		let new = databases::insert(
			&mut *conn,
			NewDatabase {
				id: DatabaseId::generate(),
				project_id: project,
				environment_id: env.id,
				server_id: old.server_id,
				name: suffixed(&old.name, slug, MAX_LEN),
				engine: old.engine,
				version: old.version.clone(),
				credentials: s
					.secrets
					.encrypt(&credentials)
					.map_err(|e| internal("encrypt", e))?,
				// A public port is unique per server and belongs to the source.
				public_port: None,
				memory_limit_mb: old.memory_limit_mb,
				cpu_limit: old.cpu_limit,
			},
		)
		.await?;
		for n in &d.networks {
			networks::upsert_member(&mut *conn, *n, ResourceKind::Database, new.id.0, &[]).await?;
		}
		pairs.push(Pair {
			kind: Kind::Database,
			source: old.id.0,
			id: new.id.0,
		});
	}

	for st in &snap.stacks {
		let old = &st.stack;
		let env_blob = if blank {
			let vars =
				stacks::decrypt_env(&s.secrets, old).map_err(|e| internal("stack env", e))?;
			let blanked: BTreeMap<String, String> =
				vars.into_keys().map(|k| (k, String::new())).collect();
			stacks::encrypt_env(&s.secrets, &blanked)?
		} else {
			old.env.clone()
		};
		let new = stacks::insert(
			&mut *conn,
			NewStack {
				id: StackId::generate(),
				project_id: project,
				environment_id: env.id,
				server_id: old.server_id,
				name: suffixed(&old.name, slug, MAX_LEN),
				slug: suffixed(&old.slug, slug, MAX_LEN),
				compose_yaml: old.compose_yaml.clone(),
				env: env_blob,
				files: old.files.clone(),
				source: old.source.clone(),
				registry_id: old.registry_id,
			},
		)
		.await?;
		for n in &st.networks {
			networks::upsert_member(&mut *conn, *n, ResourceKind::Stack, new.id.0, &[]).await?;
		}
		pairs.push(Pair {
			kind: Kind::Stack,
			source: old.id.0,
			id: new.id.0,
		});
	}

	for m in &snap.models {
		let old = &m.model;
		let new = models::insert(
			&mut *conn,
			NewModel {
				id: ModelId::generate(),
				project_id: project,
				environment_id: env.id,
				server_id: old.server_id,
				model_files_id: old.model_files_id,
				name: suffixed(&old.name, slug, MAX_LEN),
				slug: suffixed(&old.slug, slug, MAX_LEN),
				engine: old.engine,
				image: old.image.clone(),
				port: old.port,
				model_path: old.model_path.clone(),
				served_name: old.served_name.clone(),
				context_size: old.context_size,
				gpu: old.gpu,
				extra_args: old.extra_args.0.clone(),
				memory_limit_mb: old.memory_limit_mb,
			},
		)
		.await?;
		// Nothing is loading yet: it waits, stopped, until a deploy or a start.
		if !options.deploy {
			models::set_status(&mut *conn, new.id, ModelStatus::Stopped, None).await?;
		}
		for n in &m.networks {
			networks::upsert_member(&mut *conn, *n, ResourceKind::Model, new.id.0, &[]).await?;
		}
		pairs.push(Pair {
			kind: Kind::Model,
			source: old.id.0,
			id: new.id.0,
		});
	}

	// Links follow the models: one to a model of the source environment goes to its clone.
	for a in &snap.apps {
		let Some(new_app) = pair_of(&pairs, Kind::Application, a.app.id.0) else {
			continue;
		};
		for link in &a.links {
			let target = pair_of(&pairs, Kind::Model, link.model_id.0).unwrap_or(link.model_id.0);
			sqlx::query(
				"INSERT INTO application_models (application_id, model_id, is_default)
				VALUES ($1, $2, $3) ON CONFLICT DO NOTHING",
			)
			.bind(new_app)
			.bind(target)
			.bind(link.is_default)
			.execute(&mut *conn)
			.await?;
		}
	}

	// Shared variables of the environment's own scope.
	for v in &snap.shared {
		let value = match (&empty, v.is_secret) {
			(Some(empty), true) => empty.as_slice(),
			_ => v.value.as_slice(),
		};
		shared_env::upsert(
			&mut *conn,
			Scope::Environment(team, project, env.id),
			&v.key,
			value,
			v.is_secret,
			user,
		)
		.await?;
	}

	// The private network of the environment, as private as the source's.
	for n in &snap.env_networks {
		let made = networks::ensure_environment(&mut *conn, env.id, n.server_id).await?;
		if n.internal {
			sqlx::query("UPDATE networks SET internal = TRUE WHERE id = $1")
				.bind(made.id)
				.execute(&mut *conn)
				.await?;
		}
	}
	Ok((env, pairs))
}

/// The id made from the source resource `source` of `kind`.
#[must_use]
pub fn pair_of(pairs: &[Pair], kind: Kind, source: i64) -> Option<i64> {
	pairs
		.iter()
		.find(|p| p.kind == kind && p.source == source)
		.map(|p| p.id)
}
