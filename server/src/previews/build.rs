//! Making a preview's environment: the application and a fresh database for each engine it
//! uses, in a new environment of its project, in one transaction.
//!
//! This is the environment clone ([`crate::environments::clone`]) with nothing of the data:
//! the clone's copy writes the rows from a snapshot, and the snapshot here holds only the
//! application and the databases it uses, with the variables [`plan`](super::plan) lets
//! through, no user network and no model link. Databases get new credentials from the
//! copy, and start empty.

use std::collections::{HashMap, HashSet};

use super::plan::{self, DbIn, Fate, Input, Plan, VarIn};
use super::{Preview, Settings};
use crate::applications::{self, Application, SourceKind, public_hostname};
use crate::databases::engines;
use crate::databases::{self, Database};
use crate::env_vars;
use crate::environments::clone::copy;
use crate::environments::clone::plan::{AppSnap, DbSnap, Snapshot};
use crate::environments::clone::{Options, Secrets};
use crate::error::{AppResult, ServerError};
use crate::ids::{ApplicationId, DatabaseId, UserId};
use crate::scope;
use crate::servers::Server;
use crate::state::AppState;
use crate::volumes;

fn internal(what: &str, e: impl std::fmt::Display) -> ServerError {
	tracing::error!("preview: {what}: {e}");
	ServerError::Internal
}

fn is_unique(e: &ServerError) -> bool {
	matches!(e, ServerError::Sqlx(e)
		if e.as_database_error().is_some_and(|d| d.code().as_deref() == Some("23505")))
}

/// How a database of the environment is reached from an application, with the credentials
/// in the URL: what an application's variable holds when it was wired to the database.
pub(crate) fn connection(s: &AppState, db: &Database) -> AppResult<DbIn> {
	let credentials =
		databases::decrypt_credentials(&s.secrets, db).map_err(|e| internal("credentials", e))?;
	let spec = engines::spec(db.engine());
	let host = format!("podkit-db-{}", db.id);
	Ok(DbIn {
		id: db.id,
		name: db.name.clone(),
		url: spec.url(&host, spec.port, &credentials),
		host,
		memory_limit_mb: db.memory_limit_mb,
		cpu_limit: db.cpu_limit,
	})
}

/// The address a preview's application answers at.
#[must_use]
pub fn url_of(s: &AppState, server: &Server, slug: &str) -> String {
	let host = public_hostname(slug, server);
	match s.config.ingress_port {
		80 => format!("http://{host}"),
		port => format!("http://{host}:{port}"),
	}
}

/// The databases of `app`'s environment and the application's variables, read for a plan.
async fn load(
	s: &AppState,
	app: &Application,
) -> AppResult<(Vec<VarIn>, Vec<Database>, Vec<DbIn>)> {
	let marked: HashSet<String> = super::preview_keys(&s.db, app.id).await?;
	let mut vars = Vec::new();
	for row in env_vars::list(&s.db, app.id).await? {
		let value = s
			.secrets
			.decrypt(&row.value)
			.map_err(|e| internal("variable", e))?;
		vars.push(VarIn {
			marked: marked.contains(&row.key),
			key: row.key,
			value,
		});
	}
	let dbs = databases::list_by_environment(&s.db, app.environment_id).await?;
	let mut inputs = Vec::new();
	for d in &dbs {
		inputs.push(connection(s, d)?);
	}
	Ok((vars, dbs, inputs))
}

/// Makes the environment of `preview`, a pull request of `app`, and records it on the
/// preview row. The preview is `queued` when this returns.
///
/// # Errors
/// `Conflict` when a name or slug of the preview is taken (a leftover of an earlier one);
/// otherwise a database or encryption failure. Nothing is made then.
#[allow(
	clippy::too_many_lines,
	reason = "one block per step of the transaction"
)]
pub async fn make(
	s: &AppState,
	preview: &Preview,
	app: &Application,
	settings: &Settings,
) -> AppResult<()> {
	if app.source_kind != SourceKind::Git {
		return Err(ServerError::Validation(
			"Only applications built from a repository have previews.".to_owned(),
		));
	}
	let team = preview.team_id;
	let project = app.project_id;
	let number = i64::from(preview.number);
	let server = scope::server(&s.db, team, app.server_id).await?;
	let environment = scope::environment(&s.db, team, project, app.environment_id).await?;

	let (vars, dbs, inputs) = load(s, app).await?;
	let plan = Plan::build(&Input {
		number,
		app_name: &app.name,
		app_slug: &app.slug,
		memory_limit_mb: app.memory_limit_mb,
		cpu_limit: app.cpu_limit,
		vars: &vars,
		dbs: &inputs,
		from_fork: preview.from_fork,
		settings,
	});

	// The snapshot: this application and the databases it uses, nothing else.
	let mut source = app.clone();
	source.git_ref.clone_from(&preview.head_ref);
	if preview.from_fork && !preview.head_repo.is_empty() {
		source.git_repo = Some(preview.head_repo.clone());
	}
	if plan.strip_logins {
		source.git_token = None;
		source.registry_auth = None;
	}
	let kept: HashSet<&str> = plan.kept().map(|(k, _)| k.as_str()).collect();
	let mut app_vars = env_vars::list(&s.db, app.id).await?;
	app_vars.retain(|v| {
		plan.vars
			.iter()
			.any(|(k, f)| *k == v.key && matches!(f, Fate::Keep))
			&& kept.contains(v.key.as_str())
	});
	let snap = Snapshot {
		environment,
		servers: HashMap::new(),
		apps: vec![AppSnap {
			volumes: volumes::list(&s.db, app.id).await?,
			app: source,
			env_vars: app_vars,
			custom_domains: 0,
			links: Vec::new(),
			networks: Vec::new(),
		}],
		databases: plan
			.databases
			.iter()
			.filter_map(|p| dbs.iter().find(|d| d.id == p.source))
			.map(|d| DbSnap {
				db: d.clone(),
				networks: Vec::new(),
			})
			.collect(),
		stacks: Vec::new(),
		models: Vec::new(),
		cron: Vec::new(),
		shared: Vec::new(),
		env_networks: Vec::new(),
	};

	let options = Options {
		secrets: Secrets::Copy,
		data: false,
		backup_target_id: None,
		deploy: false,
	};
	let mut tx = s.db.begin().await?;
	// The user is only written into shared variables, and there are none to copy.
	let (env, pairs) = copy::apply(
		s,
		&mut tx,
		UserId(0),
		team,
		project,
		&snap,
		&plan.env_name,
		&plan.env_slug,
		&options,
	)
	.await
	.map_err(|e| {
		if is_unique(&e) {
			ServerError::Conflict(
				"A name or slug the preview needs is taken: an earlier preview of this pull request may still be going away."
					.to_owned(),
			)
		} else {
			e
		}
	})?;
	let new_app = copy::pair_of(
		&pairs,
		crate::environments::clone::Kind::Application,
		app.id.0,
	)
	.map(ApplicationId)
	.ok_or(ServerError::Internal)?;

	// The address is `pr-<n>-<app>`, and the container is capped below production's.
	sqlx::query(
		"UPDATE applications SET name = $2, slug = $3, memory_limit_mb = $4, cpu_limit = $5 WHERE id = $1",
	)
	.bind(new_app)
	.bind(&plan.app_name)
	.bind(&plan.app_slug)
	.bind(plan.memory_mb)
	.bind(plan.cpu)
	.execute(&mut *tx)
	.await
	.map_err(|e| {
		let e = ServerError::from(e);
		if is_unique(&e) {
			ServerError::Conflict("The preview's address is taken.".to_owned())
		} else {
			e
		}
	})?;

	// Each new database, with the URL an application reaches it by.
	let mut urls: HashMap<DatabaseId, String> = HashMap::new();
	for d in &plan.databases {
		let new = copy::pair_of(
			&pairs,
			crate::environments::clone::Kind::Database,
			d.source.0,
		)
		.map(DatabaseId)
		.ok_or(ServerError::Internal)?;
		sqlx::query("UPDATE databases SET memory_limit_mb = $2, cpu_limit = $3 WHERE id = $1")
			.bind(new)
			.bind(d.memory_mb)
			.bind(d.cpu)
			.execute(&mut *tx)
			.await?;
		let row = databases::find_by_id(&mut *tx, new)
			.await?
			.ok_or(ServerError::Internal)?;
		urls.insert(d.source, connection(s, &row)?.url);
	}

	let url = url_of(s, &server, &plan.app_slug);
	let mut extra: Vec<(String, String)> = plan
		.vars
		.iter()
		.filter_map(|(k, f)| match f {
			Fate::Rewrite { template } => Some((k.clone(), plan::fill(template, &urls))),
			_ => None,
		})
		.collect();
	extra.push(("PODKIT_PREVIEW".to_owned(), "true".to_owned()));
	extra.push(("PODKIT_PREVIEW_URL".to_owned(), url.clone()));
	extra.push(("PODKIT_PREVIEW_PR".to_owned(), number.to_string()));
	for (key, value) in extra {
		let sealed = s
			.secrets
			.encrypt(&value)
			.map_err(|e| internal("encrypt", e))?;
		env_vars::upsert(&mut *tx, new_app, &key, &sealed).await?;
	}

	sqlx::query(
		"UPDATE previews SET environment_id = $2, preview_application_id = $3, url = $4,
			state = 'queued', detail = '' WHERE id = $1",
	)
	.bind(preview.id)
	.bind(env.id)
	.bind(new_app)
	.bind(&url)
	.execute(&mut *tx)
	.await?;
	tx.commit().await?;
	Ok(())
}

/// The application a preview runs, if it is still there.
///
/// # Errors
/// The database fails.
pub async fn preview_app(s: &AppState, preview: &Preview) -> AppResult<Option<Application>> {
	match preview.preview_application_id {
		Some(id) => Ok(applications::find_by_id(&s.db, id).await?),
		None => Ok(None),
	}
}
