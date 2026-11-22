//! Import use cases, shared by the JSON API and the web UI: read a source, show what it would
//! make, make it.
//!
//! A preview reads the source once and keeps the result (the file and the secrets, encrypted
//! with the box that encrypts every other secret) for a quarter of an hour, under a random
//! id that only the person who asked, in the team they asked in, can use. Applying a preview
//! therefore does not need the source's credential again, and the credential is never held
//! at all after the read.
//!
//! Everything is authorized as the same action anywhere else in podkit: reading the source
//! needs `create project` (the team's import door), and the plan and the apply are
//! [`crate::iac::service`]'s, which check every change against the caller's permissions.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;

use super::http::Client;
use super::{Credentials, Imported, Options, Report, Source, caprover, coolify, dokploy};
use crate::audit::{self, Diff};
use crate::auth::permission::{Action, Resource, authorize};
use crate::error::{AppResult, ServerError};
use crate::iac::doc;
use crate::iac::objects::Secrets;
use crate::iac::service::{self as iac, ImportOutcome, Input, ProjectPlan};
use crate::ids::{TeamId, UserId};
use crate::notifications::emit;
use crate::notifications::event::{Event, EventKind, clip};
use crate::state::AppState;

/// How long a preview can be applied.
const KEEP: Duration = Duration::from_mins(15);
/// How many previews are kept at once, all teams together.
const MAX_KEPT: usize = 32;

/// What to read, and where it goes.
#[derive(Clone)]
pub struct Request {
	pub source: Source,
	pub credentials: Credentials,
	/// The name of the podkit server everything is put on.
	pub server: String,
}

/// What a read of a source made.
#[derive(Debug, Clone, Serialize)]
pub struct Preview {
	/// Applies this preview.
	pub id: String,
	pub source: Source,
	/// The `podkit.toml` it made: placeholders where there are secrets, never a value.
	pub file: String,
	pub report: Report,
	pub plans: Vec<ProjectPlan>,
}

impl Preview {
	/// Whether anything in the plans stops an apply.
	#[must_use]
	pub fn has_issues(&self) -> bool {
		self.plans.iter().any(|p| p.plan.has_issues())
	}
}

struct Kept {
	user: UserId,
	team: TeamId,
	at: Instant,
	source: Source,
	file: String,
	/// The secrets as JSON, encrypted.
	secrets: Vec<u8>,
}

static KEPT: LazyLock<Mutex<HashMap<String, Kept>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

fn random_id() -> String {
	use std::fmt::Write as _;
	let mut bytes = [0u8; 16];
	getrandom::fill(&mut bytes).expect("OS RNG unavailable");
	bytes.iter().fold(String::new(), |mut s, b| {
		let _ = write!(s, "{b:02x}");
		s
	})
}

fn lock() -> std::sync::MutexGuard<'static, HashMap<String, Kept>> {
	KEPT.lock()
		.unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn keep(
	state: &AppState,
	user: UserId,
	team: TeamId,
	imported: &Imported,
	file: &str,
) -> AppResult<String> {
	let json = serde_json::to_string(&imported.secrets.0).map_err(|_| ServerError::Internal)?;
	let secrets = state
		.secrets
		.encrypt(&json)
		.map_err(|_| ServerError::Internal)?;
	let id = random_id();
	let mut kept = lock();
	kept.retain(|_, k| k.at.elapsed() < KEEP);
	if kept.len() >= MAX_KEPT
		&& let Some(oldest) = kept
			.iter()
			.min_by_key(|(_, k)| k.at)
			.map(|(id, _)| id.clone())
	{
		kept.remove(&oldest);
	}
	kept.insert(
		id.clone(),
		Kept {
			user,
			team,
			at: Instant::now(),
			source: imported.source,
			file: file.to_owned(),
			secrets,
		},
	);
	Ok(id)
}

/// Takes a preview out of the keep. Another person's, another team's and an expired one are
/// all the same answer.
fn take(
	state: &AppState,
	user: UserId,
	team: TeamId,
	source: Source,
	id: &str,
) -> AppResult<Input> {
	let mut kept = lock();
	let usable = kept.get(id).is_some_and(|k| {
		k.user == user && k.team == team && k.source == source && k.at.elapsed() < KEEP
	});
	if !usable {
		return Err(ServerError::NotFound);
	}
	let Some(k) = kept.remove(id) else {
		return Err(ServerError::NotFound);
	};
	drop(kept);
	let json = state
		.secrets
		.decrypt(&k.secrets)
		.map_err(|_| ServerError::Internal)?;
	let secrets: std::collections::BTreeMap<String, String> =
		serde_json::from_str(&json).map_err(|_| ServerError::Internal)?;
	Ok(Input {
		text: k.file,
		secrets: Secrets(secrets),
		prune: true,
		deploy: false,
	})
}

/// Reads the source and maps it, without touching the team.
///
/// # Errors
/// `Validation` for an unusable address, credential or server name; `Import` when the
/// source cannot be read.
pub async fn read(request: &Request) -> AppResult<Imported> {
	if request.server.trim().is_empty() {
		return Err(ServerError::Validation(
			"Choose the server the applications go on.".to_owned(),
		));
	}
	let client = Client::connect(request.source, &request.credentials).await?;
	let snapshot = match request.source {
		Source::Coolify => coolify::fetch(&client).await?,
		Source::Dokploy => dokploy::fetch(&client).await?,
		Source::Caprover => caprover::fetch(&client).await?,
	};
	let options = Options {
		server: request.server.trim().to_owned(),
	};
	Ok(match request.source {
		Source::Coolify => coolify::map(&snapshot, &options),
		Source::Dokploy => dokploy::map(&snapshot, &options),
		Source::Caprover => caprover::map(&snapshot, &options),
	})
}

/// Reads the source and plans what it makes against the team, without changing anything.
///
/// # Errors
/// `Forbidden` without `create project`; those of [`read`].
pub async fn preview(
	state: &AppState,
	user: UserId,
	team: TeamId,
	request: &Request,
) -> AppResult<Preview> {
	authorize(&state.db, user, team, Action::Create, Resource::Project).await?;
	let imported = read(request).await?;
	let file = doc::render(&imported.doc).map_err(|e| ServerError::Validation(e.to_string()))?;
	let plans = if imported.doc.projects.is_empty() {
		Vec::new()
	} else {
		let input = Input {
			text: file.clone(),
			secrets: imported.secrets.clone(),
			prune: true,
			deploy: false,
		};
		iac::import_plan(state, user, team, &input).await?
	};
	let id = keep(state, user, team, &imported, &file)?;
	Ok(Preview {
		id,
		source: imported.source,
		file,
		report: imported.report,
		plans,
	})
}

/// What an apply of a preview did.
#[derive(Debug)]
pub struct Applied {
	pub outcome: ImportOutcome,
}

/// Makes what a preview planned.
///
/// # Errors
/// `NotFound` for a preview that is not the caller's or has expired; `Forbidden` and the
/// rest as [`iac::import_apply`].
pub async fn apply(
	state: &AppState,
	user: UserId,
	team: TeamId,
	source: Source,
	id: &str,
	deploy: bool,
) -> AppResult<Applied> {
	authorize(&state.db, user, team, Action::Create, Resource::Project).await?;
	let mut input = take(state, user, team, source, id)?;
	input.deploy = deploy;
	let outcome = iac::import_apply(state, user, team, &input).await?;
	let made: Vec<String> = match &outcome {
		ImportOutcome::Done(done) => done.iter().map(|d| d.slug.clone()).collect(),
		ImportOutcome::Refused(_) => Vec::new(),
	};
	audit::log(
		&state.db,
		user,
		team,
		"import.run",
		audit::Target::none("project"),
		Diff::new()
			.set("source", source.as_str())
			.set("projects", made)
			.set("deploy", deploy),
	)
	.await;
	if let Some(message) = failure(&outcome) {
		emit(
			state,
			Event::new(
				team,
				EventKind::ImportFailed,
				format!("import:{}", source.as_str()),
				format!("The import from {} stopped", source.label()),
			)
			.body(clip(&message, 500))
			.link(format!("/teams/{team}/settings/import")),
		)
		.await;
	}
	Ok(Applied { outcome })
}

/// What went wrong in an apply, in a sentence; `None` when it went well.
fn failure(outcome: &ImportOutcome) -> Option<String> {
	match outcome {
		ImportOutcome::Refused(plans) => plans
			.iter()
			.find_map(|p| p.plan.issues.first())
			.map(|i| format!("{}: {}", i.path, i.message)),
		ImportOutcome::Done(done) => done.iter().find_map(|d| {
			d.applied.report.failed.as_ref().map(|_| {
				format!(
					"a change of project \"{}\" failed; the projects after it were not made",
					d.slug
				)
			})
		}),
	}
}

/// Reads, plans and applies in one go, for the API and the command line.
///
/// # Errors
/// Those of [`preview`] and [`apply`].
pub async fn run(
	state: &AppState,
	user: UserId,
	team: TeamId,
	request: &Request,
	deploy: bool,
) -> AppResult<(Preview, Option<Applied>)> {
	let preview = preview(state, user, team, request).await?;
	if preview.plans.is_empty() || preview.has_issues() {
		return Ok((preview, None));
	}
	let applied = apply(state, user, team, preview.source, &preview.id, deploy).await?;
	Ok((preview, Some(applied)))
}
