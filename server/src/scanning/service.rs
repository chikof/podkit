//! What a person may see and do with scans. Each fn checks the permission
//! first and then resolves the resource (or the scan) inside the team, so
//! another team's id is a 404, exactly like a missing one.

use std::collections::HashMap;

use sqlx::PgPool;
use time::{Duration, OffsetDateTime};

use super::store::{self, FindingFilter, Scan, StoredFinding, StoredIgnore};
use super::{
	Badge, BadgeState, Counts, MAX_IGNORE_DAYS, ResourceKind, Subject, Target, decompress,
	queue_fresh,
};
use crate::audit::{self, Diff};
use crate::auth::permission::{Action, Resource, authorize};
use crate::error::{AppResult, ServerError};
use crate::ids::{DeploymentId, ProjectId, ScanId, ScanIgnoreId, ServerId, TeamId, UserId};
use crate::scope;
use crate::state::AppState;

/// A scan with the team's ignores taken into account.
#[derive(Debug, Clone)]
pub struct ScanView {
	pub scan: Scan,
	/// Counts without the findings the team has ignored.
	pub counts: Counts,
	pub badge: Badge,
	pub has_sbom: bool,
}

/// One image of a subject: the resource itself, or one service of a stack.
#[derive(Debug, Clone)]
pub struct Entry {
	/// The service name for a stack; `None` for a single-image resource.
	pub label: Option<String>,
	pub kind: ResourceKind,
	pub resource_id: i64,
	pub view: Option<ScanView>,
}

/// Everything the Security tab of one resource shows.
#[derive(Debug, Clone)]
pub struct SubjectScans {
	pub entries: Vec<Entry>,
	/// The sum over the entries.
	pub badge: Badge,
	/// Whether the resource's server scans at all.
	pub scanning_on: bool,
}

fn views(
	team: TeamId,
	scans: HashMap<i64, Scan>,
	effective: &HashMap<ScanId, Counts>,
) -> HashMap<i64, ScanView> {
	scans
		.into_iter()
		.filter(|(_, s)| s.team_id == team)
		.map(|(id, scan)| {
			let badge = store::badge_of(Some(&scan), effective);
			let counts = effective.get(&scan.id).copied().unwrap_or_default();
			let has_sbom = scan.sbom_bytes.is_some();
			(
				id,
				ScanView {
					scan,
					counts,
					badge,
					has_sbom,
				},
			)
		})
		.collect()
}

async fn views_for(
	db: &PgPool,
	team: TeamId,
	kind: ResourceKind,
	ids: &[i64],
) -> AppResult<HashMap<i64, ScanView>> {
	let scans = store::latest(db, kind, ids).await?;
	let ids: Vec<ScanId> = scans.values().map(|s| s.id).collect();
	let effective = store::effective_counts(db, team, &ids).await?;
	Ok(views(team, scans, &effective))
}

/// Folds entries into the one badge a header shows: the worst state wins.
fn fold(entries: &[Entry]) -> Badge {
	let mut counts = Counts::default();
	let (mut scanning, mut failed, mut found) = (false, false, false);
	for entry in entries {
		let Some(view) = &entry.view else { continue };
		match view.badge.state {
			BadgeState::Scanning => scanning = true,
			BadgeState::Failed => failed = true,
			BadgeState::Clean | BadgeState::Found(_) => {
				found = true;
				counts = counts.plus(view.counts);
			}
			BadgeState::None => {}
		}
	}
	let state = if found {
		counts.worst().map_or(BadgeState::Clean, BadgeState::Found)
	} else if scanning {
		BadgeState::Scanning
	} else if failed {
		BadgeState::Failed
	} else {
		BadgeState::None
	};
	// A failed or running scan next to a finished one still shows the finished result.
	Badge { state, counts }
}

async fn stack_services(db: &PgPool, stack: i64) -> Result<Vec<(i64, String)>, sqlx::Error> {
	sqlx::query_as("SELECT id, name FROM stack_services WHERE stack_id = $1 ORDER BY name")
		.bind(stack)
		.fetch_all(db)
		.await
}

/// The Security tab of one resource.
///
/// # Errors
/// `Forbidden` without `read scan`; the resource's own not-found for another team's.
pub async fn subject_scans(
	db: &PgPool,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	subject: Subject,
) -> AppResult<SubjectScans> {
	authorize(db, user, team, Action::Read, Resource::Scan).await?;
	let (server, targets): (ServerId, Vec<(Option<String>, ResourceKind, i64)>) = match subject {
		Subject::Application(id) => {
			let app = scope::application(db, team, project, id).await?;
			(
				app.server_id,
				vec![(None, ResourceKind::Application, app.id.0)],
			)
		}
		Subject::Database(id) => {
			let database = scope::database(db, team, project, id).await?;
			(
				database.server_id,
				vec![(None, ResourceKind::Database, database.id.0)],
			)
		}
		Subject::Model(id) => {
			let model = scope::model(db, team, project, id).await?;
			(
				model.server_id,
				vec![(None, ResourceKind::Model, model.id.0)],
			)
		}
		Subject::Stack(id) => {
			let stack = scope::stack(db, team, project, id).await?;
			let services = stack_services(db, stack.id.0).await?;
			(
				stack.server_id,
				services
					.into_iter()
					.map(|(id, name)| (Some(name), ResourceKind::StackService, id))
					.collect(),
			)
		}
	};
	let scanning_on = store::server_settings(db, server)
		.await?
		.is_some_and(|s| s.scan_enabled);
	let mut entries = Vec::new();
	for (label, kind, id) in targets {
		let mut found = views_for(db, team, kind, &[id]).await?;
		entries.push(Entry {
			label,
			kind,
			resource_id: id,
			view: found.remove(&id),
		});
	}
	let badge = fold(&entries);
	Ok(SubjectScans {
		entries,
		badge,
		scanning_on,
	})
}

/// Badges for the resources a page lists. Subjects are taken as the page's own
/// (already team-scoped) rows; scans of another team are dropped regardless.
///
/// # Errors
/// `Forbidden` without `read scan`.
pub async fn badges(
	db: &PgPool,
	user: UserId,
	team: TeamId,
	subjects: &[Subject],
) -> AppResult<HashMap<Subject, Badge>> {
	authorize(db, user, team, Action::Read, Resource::Scan).await?;
	let ids = |pick: fn(&Subject) -> Option<i64>| -> Vec<i64> {
		subjects.iter().filter_map(pick).collect()
	};
	let apps = ids(|s| match s {
		Subject::Application(id) => Some(id.0),
		_ => None,
	});
	let databases = ids(|s| match s {
		Subject::Database(id) => Some(id.0),
		_ => None,
	});
	let models = ids(|s| match s {
		Subject::Model(id) => Some(id.0),
		_ => None,
	});
	let stacks = ids(|s| match s {
		Subject::Stack(id) => Some(id.0),
		_ => None,
	});

	let app_views = views_for(db, team, ResourceKind::Application, &apps).await?;
	let db_views = views_for(db, team, ResourceKind::Database, &databases).await?;
	let model_views = views_for(db, team, ResourceKind::Model, &models).await?;
	let services: Vec<(i64, i64)> =
		sqlx::query_as("SELECT id, stack_id FROM stack_services WHERE stack_id = ANY($1)")
			.bind(&stacks)
			.fetch_all(db)
			.await?;
	let service_ids: Vec<i64> = services.iter().map(|(id, _)| *id).collect();
	let mut service_views = views_for(db, team, ResourceKind::StackService, &service_ids).await?;

	let one = |kind: ResourceKind, id: i64, view: Option<&ScanView>| Entry {
		label: None,
		kind,
		resource_id: id,
		view: view.cloned(),
	};
	let mut out = HashMap::new();
	for subject in subjects {
		let badge = match subject {
			Subject::Application(id) => {
				fold(&[one(ResourceKind::Application, id.0, app_views.get(&id.0))])
			}
			Subject::Database(id) => {
				fold(&[one(ResourceKind::Database, id.0, db_views.get(&id.0))])
			}
			Subject::Model(id) => fold(&[one(ResourceKind::Model, id.0, model_views.get(&id.0))]),
			Subject::Stack(id) => {
				let entries: Vec<Entry> = services
					.iter()
					.filter(|(_, stack)| *stack == id.0)
					.map(|(service, _)| Entry {
						label: None,
						kind: ResourceKind::StackService,
						resource_id: *service,
						view: service_views.remove(service),
					})
					.collect();
				fold(&entries)
			}
		};
		out.insert(*subject, badge);
	}
	Ok(out)
}

/// What the deploy page says about the scan a deployment queued.
///
/// # Errors
/// `Forbidden` without `read scan`.
pub async fn deployment_scans(
	db: &PgPool,
	user: UserId,
	team: TeamId,
	deployments: &[DeploymentId],
) -> AppResult<HashMap<DeploymentId, ScanView>> {
	authorize(db, user, team, Action::Read, Resource::Scan).await?;
	let scans =
		store::for_deployments(db, &deployments.iter().map(|d| d.0).collect::<Vec<_>>()).await?;
	let ids: Vec<ScanId> = scans.values().map(|s| s.id).collect();
	let effective = store::effective_counts(db, team, &ids).await?;
	Ok(views(team, scans, &effective)
		.into_iter()
		.map(|(id, view)| (DeploymentId(id), view))
		.collect())
}

/// A scan and its findings.
///
/// # Errors
/// `Forbidden` without `read scan`; `NotFound` for another team's scan.
pub async fn findings(
	db: &PgPool,
	user: UserId,
	team: TeamId,
	id: ScanId,
	filter: FindingFilter,
) -> AppResult<(ScanView, Vec<StoredFinding>)> {
	authorize(db, user, team, Action::Read, Resource::Scan).await?;
	let scan = store::find_in_team(db, team, id)
		.await?
		.ok_or(ServerError::NotFound)?;
	let effective = store::effective_counts(db, team, &[scan.id]).await?;
	let found = store::findings(db, team, scan.id, filter).await?;
	let badge = store::badge_of(Some(&scan), &effective);
	let counts = effective.get(&scan.id).copied().unwrap_or_default();
	let has_sbom = scan.sbom_bytes.is_some();
	Ok((
		ScanView {
			scan,
			counts,
			badge,
			has_sbom,
		},
		found,
	))
}

/// The SBOM of a scan as `CycloneDX` JSON, with a name for the download.
///
/// # Errors
/// `Forbidden` without `read scan`; `NotFound` for another team's scan or one
/// without an SBOM.
pub async fn sbom(
	db: &PgPool,
	user: UserId,
	team: TeamId,
	id: ScanId,
) -> AppResult<(String, String)> {
	authorize(db, user, team, Action::Read, Resource::Scan).await?;
	let packed = store::sbom(db, team, id)
		.await?
		.ok_or(ServerError::NotFound)?;
	let json = decompress(&packed).map_err(|e| {
		tracing::error!(scan = id.0, "stored SBOM is unreadable: {e}");
		ServerError::Internal
	})?;
	Ok((format!("sbom-{id}.cdx.json"), json))
}

/// A person's ignore, as the form sends it.
#[derive(Debug, Clone)]
pub struct NewIgnore {
	pub vuln_id: String,
	pub package: Option<String>,
	pub reason: String,
	pub days: i64,
}

/// Ignores a finding for the team until `days` from now.
///
/// # Errors
/// `Forbidden` without `update scan`; `Validation` for an empty id or reason or a
/// period outside 1 to 365 days.
pub async fn ignore(
	db: &PgPool,
	user: UserId,
	team: TeamId,
	new: NewIgnore,
) -> AppResult<ScanIgnoreId> {
	authorize(db, user, team, Action::Update, Resource::Scan).await?;
	let vuln_id = new.vuln_id.trim();
	if vuln_id.is_empty() || vuln_id.len() > 100 {
		return Err(ServerError::Validation(
			"vulnerability id must be 1 to 100 characters".to_owned(),
		));
	}
	let reason = new.reason.trim();
	if reason.is_empty() || reason.chars().count() > 500 {
		return Err(ServerError::Validation(
			"say why in 1 to 500 characters".to_owned(),
		));
	}
	if !(1..=MAX_IGNORE_DAYS).contains(&new.days) {
		return Err(ServerError::Validation(format!(
			"ignore for 1 to {MAX_IGNORE_DAYS} days"
		)));
	}
	let package = new
		.package
		.as_deref()
		.map(str::trim)
		.filter(|p| !p.is_empty());
	let id = store::add_ignore(
		db,
		team,
		user,
		vuln_id,
		package,
		reason,
		OffsetDateTime::now_utc() + Duration::days(new.days),
	)
	.await?;
	audit::log(
		db,
		user,
		team,
		"scan.ignore",
		audit::Target::new("scan_ignore", id).named(vuln_id),
		Diff::new()
			.set("vuln_id", vuln_id)
			.set("package", package)
			.set("days", new.days),
	)
	.await;
	Ok(id)
}

/// # Errors
/// `Forbidden` without `delete scan`; `NotFound` for another team's ignore.
pub async fn remove_ignore(
	db: &PgPool,
	user: UserId,
	team: TeamId,
	id: ScanIgnoreId,
) -> AppResult<()> {
	authorize(db, user, team, Action::Delete, Resource::Scan).await?;
	if store::remove_ignore(db, team, id).await? {
		audit::log(
			db,
			user,
			team,
			"scan.unignore",
			audit::Target::new("scan_ignore", id),
			Diff::new(),
		)
		.await;
		Ok(())
	} else {
		Err(ServerError::NotFound)
	}
}

/// # Errors
/// `Forbidden` without `read scan`.
pub async fn ignores(db: &PgPool, user: UserId, team: TeamId) -> AppResult<Vec<StoredIgnore>> {
	authorize(db, user, team, Action::Read, Resource::Scan).await?;
	Ok(store::ignores(db, team).await?)
}

/// Scans the images of a resource again, skipping the 24 h reuse. Returns how many
/// scans were queued.
///
/// # Errors
/// `Forbidden` without `create scan`; `Validation` when nothing was scanned yet
/// (there is no image to look at until a deploy has run) or scanning is off.
pub async fn scan_again(
	state: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	subject: Subject,
) -> AppResult<usize> {
	authorize(&state.db, user, team, Action::Create, Resource::Scan).await?;
	let found = subject_scans(&state.db, user, team, project, subject).await?;
	if !found.scanning_on {
		return Err(ServerError::Validation(
			"scanning is off for this server".to_owned(),
		));
	}
	let mut queued = 0;
	for entry in found.entries {
		let Some(view) = entry.view else { continue };
		if view.scan.status.in_progress() {
			continue;
		}
		let target = Target {
			kind: entry.kind,
			resource_id: entry.resource_id,
			image_ref: &view.scan.image_ref,
			deployment: None,
		};
		if queue_fresh(state, view.scan.server_id, target)
			.await?
			.is_some()
		{
			queued += 1;
		}
	}
	if queued == 0 {
		return Err(ServerError::Validation(
			"nothing to scan yet: deploy first".to_owned(),
		));
	}
	let (kind, id) = match subject {
		Subject::Application(id) => ("application", id.0),
		Subject::Database(id) => ("database", id.0),
		Subject::Model(id) => ("model", id.0),
		Subject::Stack(id) => ("stack", id.0),
	};
	audit::log(
		&state.db,
		user,
		team,
		"scan.run",
		audit::Target::new(kind, id).in_project(project),
		Diff::new().set("queued", queued),
	)
	.await;
	Ok(queued)
}

/// One row of the team's Security overview.
#[derive(Debug, Clone)]
pub struct OverviewRow {
	pub subject: Subject,
	pub name: String,
	pub project_id: ProjectId,
	pub project_name: String,
	pub kind_label: &'static str,
	pub badge: Badge,
}

/// Every resource of the team that can be scanned, riskiest first.
///
/// # Errors
/// `Forbidden` without `read scan`.
pub async fn overview(db: &PgPool, user: UserId, team: TeamId) -> AppResult<Vec<OverviewRow>> {
	authorize(db, user, team, Action::Read, Resource::Scan).await?;
	let mut rows = Vec::new();
	let mut subjects = Vec::new();
	for (table, label) in [
		("applications", "Application"),
		("databases", "Database"),
		("models", "Model"),
		("stacks", "Stack"),
	] {
		let listed: Vec<(i64, String, ProjectId, String)> =
			sqlx::query_as(sqlx::AssertSqlSafe(format!(
				"SELECT r.id, r.name, p.id, p.name FROM {table} r
				JOIN projects p ON p.id = r.project_id WHERE p.team_id = $1"
			)))
			.bind(team)
			.fetch_all(db)
			.await?;
		for (id, name, project_id, project_name) in listed {
			let subject = match label {
				"Application" => Subject::Application(crate::ids::ApplicationId(id)),
				"Database" => Subject::Database(crate::ids::DatabaseId(id)),
				"Model" => Subject::Model(crate::ids::ModelId(id)),
				_ => Subject::Stack(crate::ids::StackId(id)),
			};
			subjects.push(subject);
			rows.push(OverviewRow {
				subject,
				name,
				project_id,
				project_name,
				kind_label: label,
				badge: Badge::NONE,
			});
		}
	}
	let badges = badges(db, user, team, &subjects).await?;
	for row in &mut rows {
		row.badge = badges.get(&row.subject).copied().unwrap_or(Badge::NONE);
	}
	rows.sort_by(|a, b| {
		b.badge
			.risk()
			.cmp(&a.badge.risk())
			.then_with(|| a.name.cmp(&b.name))
	});
	Ok(rows)
}

/// A server's scan settings, for its settings page.
#[derive(Debug, Clone, Copy)]
pub struct Settings {
	pub enabled: bool,
	pub cpus: f64,
	pub memory_mb: i32,
}

/// # Errors
/// `Forbidden` without `read scan`; `ServerNotFound` for another team's server.
pub async fn settings(
	db: &PgPool,
	user: UserId,
	team: TeamId,
	server: ServerId,
) -> AppResult<Settings> {
	authorize(db, user, team, Action::Read, Resource::Scan).await?;
	let server = scope::server(db, team, server).await?;
	let s = store::server_settings(db, server.id)
		.await?
		.ok_or(ServerError::ServerNotFound)?;
	Ok(Settings {
		enabled: s.scan_enabled,
		cpus: s.scan_cpus,
		memory_mb: s.scan_memory_mb,
	})
}

/// Turns scanning on or off for a server and sets the scanner caps.
///
/// # Errors
/// `Forbidden` without `update scan`; `ServerNotFound` for another team's server;
/// `Validation` for caps outside 0.25 to 8 CPUs or 256 to 16384 MiB.
pub async fn update_settings(
	db: &PgPool,
	user: UserId,
	team: TeamId,
	server: ServerId,
	new: Settings,
) -> AppResult<()> {
	authorize(db, user, team, Action::Update, Resource::Scan).await?;
	let server = scope::server(db, team, server).await?;
	if !(0.25..=8.0).contains(&new.cpus) {
		return Err(ServerError::Validation(
			"scan CPUs must be 0.25 to 8".to_owned(),
		));
	}
	if !(256..=16_384).contains(&new.memory_mb) {
		return Err(ServerError::Validation(
			"scan memory must be 256 to 16384 MiB".to_owned(),
		));
	}
	sqlx::query(
		"UPDATE servers SET scan_enabled = $2, scan_cpus = $3, scan_memory_mb = $4 WHERE id = $1",
	)
	.bind(server.id)
	.bind(new.enabled)
	.bind(new.cpus)
	.bind(new.memory_mb)
	.execute(db)
	.await?;
	audit::log(
		db,
		user,
		team,
		"scan.settings",
		audit::Target::new("server", server.id).named(&server.name),
		Diff::new()
			.set("enabled", new.enabled)
			.set("cpus", new.cpus)
			.set("memory_mb", new.memory_mb),
	)
	.await;
	Ok(())
}
