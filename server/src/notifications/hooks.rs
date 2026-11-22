//! Where the rest of podkit raises events. Each function here takes what the caller already
//! has (a job that finished, a server that changed state) and turns it into one [`Event`]; the
//! callers stay one line long and a failure here is only ever logged.

use std::collections::HashSet;

use proto::ScanResult;
use sqlx::PgPool;

use super::event::{Event, EventKind, Severity, clip};
use crate::ids::{EnvironmentId, ProjectId, ScanId, TeamId};
use crate::jobs::{Job, JobFailure, JobKind};
use crate::scanning::{Severity as VulnSeverity, is_act_now};
use crate::state::AppState;

/// A resource an event is about: what to call it, where it lives and where to link to.
#[derive(Debug, Clone)]
pub struct Target {
	pub kind: &'static str,
	pub id: i64,
	pub name: String,
	pub team: TeamId,
	pub project: ProjectId,
	pub environment: EnvironmentId,
	pub link: String,
}

impl Target {
	pub fn subject(&self) -> String {
		format!("{}:{}", self.kind, self.id)
	}

	/// An event about this resource, scoped to its project and environment.
	pub fn event(&self, kind: EventKind, title: impl Into<String>) -> Event {
		Event::new(self.team, kind, self.subject(), title)
			.scope(self.project, Some(self.environment))
			.link(self.link.clone())
	}
}

async fn target(
	db: &PgPool,
	kind: &'static str,
	path: &'static str,
	sql: &'static str,
	id: i64,
) -> Option<Target> {
	let row = sqlx::query_as::<_, (String, TeamId, ProjectId, EnvironmentId)>(sql)
		.bind(id)
		.fetch_optional(db)
		.await;
	match row {
		Ok(Some((name, team, project, environment))) => Some(Target {
			kind,
			id,
			link: format!("/teams/{team}/projects/{project}/{path}/{id}"),
			name,
			team,
			project,
			environment,
		}),
		Ok(None) => None,
		Err(e) => {
			tracing::warn!("cannot look up the {kind} {id} for a notification: {e}");
			None
		}
	}
}

pub async fn application(db: &PgPool, id: i64) -> Option<Target> {
	target(
		db,
		"application",
		"apps",
		"SELECT a.name, p.team_id, a.project_id, a.environment_id FROM applications a
		JOIN projects p ON p.id = a.project_id WHERE a.id = $1",
		id,
	)
	.await
}

pub async fn database(db: &PgPool, id: i64) -> Option<Target> {
	target(
		db,
		"database",
		"databases",
		"SELECT d.name, p.team_id, d.project_id, d.environment_id FROM databases d
		JOIN projects p ON p.id = d.project_id WHERE d.id = $1",
		id,
	)
	.await
}

pub async fn model(db: &PgPool, id: i64) -> Option<Target> {
	target(
		db,
		"model",
		"models",
		"SELECT m.name, p.team_id, m.project_id, m.environment_id FROM models m
		JOIN projects p ON p.id = m.project_id WHERE m.id = $1",
		id,
	)
	.await
}

pub async fn stack(db: &PgPool, id: i64) -> Option<Target> {
	target(
		db,
		"stack",
		"stacks",
		"SELECT s.name, p.team_id, s.project_id, s.environment_id FROM stacks s
		JOIN projects p ON p.id = s.project_id WHERE s.id = $1",
		id,
	)
	.await
}

/// The target of a stored resource kind (`application`, `stack_service`, `database`, `model`).
pub async fn by_kind(db: &PgPool, kind: &str, id: i64) -> Option<Target> {
	match kind {
		"application" => application(db, id).await,
		"database" => database(db, id).await,
		"model" => model(db, id).await,
		"stack" => stack(db, id).await,
		"stack_service" => {
			let stack_id =
				sqlx::query_scalar::<_, i64>("SELECT stack_id FROM stack_services WHERE id = $1")
					.bind(id)
					.fetch_optional(db)
					.await
					.ok()
					.flatten()?;
			stack(db, stack_id).await
		}
		_ => None,
	}
}

/// A server's team and name.
pub async fn server(db: &PgPool, id: i64) -> Option<(TeamId, String)> {
	sqlx::query_as::<_, (TeamId, String)>("SELECT team_id, name FROM servers WHERE id = $1")
		.bind(id)
		.fetch_optional(db)
		.await
		.ok()
		.flatten()
}

fn failure_text(failure: &JobFailure) -> &str {
	match failure {
		JobFailure::Failed(message) => message,
		JobFailure::Cancelled => "cancelled",
	}
}

/// Called once for every job that ends: a deploy or a stack deploy that succeeded or failed, a
/// backup, a model that would not start. Cancelled jobs are the user's own doing and stay
/// quiet.
pub async fn job_finished(state: &AppState, job: &Job, result: &Result<(), JobFailure>) {
	if matches!(result, Err(JobFailure::Cancelled)) {
		return;
	}
	let db = &state.db;
	let event = match job.kind {
		JobKind::Deploy => {
			let app = sqlx::query_scalar::<_, i64>(
				"SELECT application_id FROM deployments WHERE id = $1",
			)
			.bind(job.target_id)
			.fetch_optional(db)
			.await
			.ok()
			.flatten();
			let Some(target) = application(db, app.unwrap_or_default()).await else {
				return;
			};
			let link = format!("{}/deployments", target.link);
			match result {
				Ok(()) => target
					.event(
						EventKind::DeploySucceeded,
						format!("{} deployed", target.name),
					)
					.link(link),
				Err(e) => target
					.event(
						EventKind::DeployFailed,
						format!("Deploy of {} failed", target.name),
					)
					.body(clip(failure_text(e), 500))
					.link(link),
			}
		}
		JobKind::StackDeploy => {
			let Some(target) = stack(db, job.target_id).await else {
				return;
			};
			match result {
				Ok(()) => target.event(
					EventKind::DeploySucceeded,
					format!("{} deployed", target.name),
				),
				Err(e) => target
					.event(
						EventKind::DeployFailed,
						format!("Deploy of {} failed", target.name),
					)
					.body(clip(failure_text(e), 500)),
			}
		}
		JobKind::Backup => {
			let database_id =
				sqlx::query_scalar::<_, i64>("SELECT database_id FROM backups WHERE id = $1")
					.bind(job.target_id)
					.fetch_optional(db)
					.await
					.ok()
					.flatten();
			let Some(target) = database(db, database_id.unwrap_or_default()).await else {
				return;
			};
			let link = format!("{}/backups", target.link);
			match result {
				Ok(()) => target
					.event(
						EventKind::BackupSucceeded,
						format!("Backup of {} finished", target.name),
					)
					.link(link),
				Err(e) => target
					.event(
						EventKind::BackupFailed,
						format!("Backup of {} failed", target.name),
					)
					.body(clip(failure_text(e), 500))
					.link(link),
			}
		}
		JobKind::ModelStart => {
			let Err(e) = result else { return };
			let Some(target) = model(db, job.target_id).await else {
				return;
			};
			target
				.event(
					EventKind::ModelCrashed,
					format!("Model {} failed to start", target.name),
				)
				.body(clip(failure_text(e), 500))
		}
		JobKind::CronRun => {
			// Retries, `cron.failed` and recovery are the cron slice's to decide.
			crate::cron::run::finished(state, crate::ids::CronRunId(job.target_id), result).await;
			return;
		}
		_ => return,
	};
	super::emit(state, event).await;
}

/// A server's health check failed after it had been active.
pub async fn server_unreachable(state: &AppState, id: i64, message: &str) {
	let Some((team, name)) = server(&state.db, id).await else {
		return;
	};
	super::emit(
		state,
		Event::new(
			team,
			EventKind::ServerUnreachable,
			format!("server:{id}"),
			format!("Server {name} is unreachable"),
		)
		.body(clip(message, 300))
		.link(format!("/teams/{team}/servers/{id}")),
	)
	.await;
}

/// A firewall change was not confirmed and the helper took it back.
pub async fn firewall_reverted(state: &AppState, id: i64, why: &str) {
	let Some((team, name)) = server(&state.db, id).await else {
		return;
	};
	super::emit(
		state,
		Event::new(
			team,
			EventKind::FirewallReverted,
			format!("firewall:{id}"),
			format!("Firewall change on {name} was reverted"),
		)
		.body(clip(why, 300))
		.link(format!("/teams/{team}/servers/{id}/firewall")),
	)
	.await;
}

/// A server that was unreachable answers again.
pub async fn server_recovered(state: &AppState, id: i64) {
	let Some((team, name)) = server(&state.db, id).await else {
		return;
	};
	super::emit(
		state,
		Event::new(
			team,
			EventKind::ServerRecovered,
			format!("server:{id}"),
			format!("Server {name} is back"),
		)
		.link(format!("/teams/{team}/servers/{id}")),
	)
	.await;
}

/// A container podkit started is gone or dead and its supervisor gave up on it.
pub async fn crashed(state: &AppState, target: Option<Target>, message: &str) {
	let Some(target) = target else { return };
	super::emit(
		state,
		target
			.event(
				EventKind::CrashLoop,
				format!("{} keeps crashing", target.name),
			)
			.body(clip(message, 500)),
	)
	.await;
}

/// A deployment of `application` was started again from an earlier build.
pub async fn rollback(state: &AppState, application_id: i64, who: Option<&str>) {
	let Some(target) = application(&state.db, application_id).await else {
		return;
	};
	let by = who.map(|w| format!("Started by {w}.")).unwrap_or_default();
	let link = format!("{}/deployments", target.link);
	super::emit(
		state,
		target
			.event(
				EventKind::Rollback,
				format!("{} is rolling back", target.name),
			)
			.body(by)
			.link(link),
	)
	.await;
}

/// Keys of the findings that already count as critical or act-now, to compare with a new scan.
pub async fn severe_findings(db: &PgPool, scan: ScanId) -> HashSet<(String, String, String)> {
	let rows = sqlx::query_as::<_, (String, String, String, String, bool, Option<f64>, Option<f64>)>(
		"SELECT vuln_id, package, version, severity, kev, epss, cvss FROM scan_findings WHERE scan_id = $1",
	)
	.bind(scan)
	.fetch_all(db)
	.await
	.unwrap_or_default();
	rows.into_iter()
		.filter(|(_, _, _, severity, kev, epss, cvss)| is_severe(severity, *kev, *epss, *cvss))
		.map(|(id, package, version, ..)| (id, package, version))
		.collect()
}

fn is_severe(severity: &str, kev: bool, epss: Option<f64>, cvss: Option<f64>) -> bool {
	VulnSeverity::parse(severity) == Some(VulnSeverity::Critical) || is_act_now(kev, epss, cvss)
}

/// Raises "new critical vulnerability" for findings in `result` that are critical or act-now
/// and were not in `before`.
pub async fn vulnerabilities(
	state: &AppState,
	scan: ScanId,
	team: TeamId,
	image: &str,
	before: &HashSet<(String, String, String)>,
	result: &ScanResult,
) {
	let fresh: Vec<_> = result
		.findings
		.iter()
		.filter(|f| is_severe(&f.severity, f.kev, f.epss, f.cvss))
		.filter(|f| !before.contains(&(f.vuln_id.clone(), f.package.clone(), f.version.clone())))
		.collect();
	if fresh.is_empty() {
		return;
	}
	let mut ids: Vec<&str> = fresh.iter().map(|f| f.vuln_id.as_str()).collect();
	ids.sort_unstable();
	ids.dedup();
	let listed = ids.iter().take(5).copied().collect::<Vec<_>>().join(", ");
	let more = ids.len().saturating_sub(5);
	let body = if more > 0 {
		format!("{listed} and {more} more")
	} else {
		listed
	};
	let linked = sqlx::query_as::<_, (String, i64)>(
		"SELECT resource_kind, resource_id FROM scan_links WHERE scan_id = $1 LIMIT 1",
	)
	.bind(scan)
	.fetch_optional(&state.db)
	.await
	.ok()
	.flatten();
	let resource = match linked {
		Some((kind, id)) => by_kind(&state.db, &kind, id).await,
		None => None,
	};
	let mut event = Event::new(
		team,
		EventKind::Vulnerability,
		format!("image:{image}"),
		format!(
			"{} new critical {} in {}",
			ids.len(),
			if ids.len() == 1 {
				"vulnerability"
			} else {
				"vulnerabilities"
			},
			clip(image, 120)
		),
	)
	.body(body)
	.severity(Severity::Critical)
	.link(format!("/teams/{team}/security"));
	if let Some(resource) = resource {
		event = event.scope(resource.project, Some(resource.environment));
	}
	super::emit(state, event).await;
}
