//! Database pages: an Operate surface, the resource page of one database.
//!
//! A header (engine logo, name, status, server and the Start / Stop / Restart actions with a
//! menu) over a row of tabs, each a page of its own: General (the latest job's log and the
//! port and limits), Connection (addresses, and the password only through **Reveal**),
//! Backups (schedule, destination, retention, history, restore; see `pages::backups`), Logs
//! and Advanced (identifiers and the danger zone). The create flow is one page, `create`.
//!
//! The password is write-only in spirit: no page and no signal carries it. It exists in the
//! response to the **Reveal** action and nowhere else, and that response may not be stored.

use axum::Router;
use maud::{Markup, html};
use serde::{Deserialize, Deserializer};
use serde_json::{Value, json};
use time::OffsetDateTime;

use crate::auth::permission::{Action, Resource};
use crate::databases::service::DatabaseDetail;
use crate::databases::{Database, EngineKind};
use crate::error::ServerError;
use crate::ids::{DatabaseId, ProjectId, TeamId};
use crate::jobs::JobStatus;
use crate::projects;
use crate::projects::Project;
use crate::state::AppState;
use crate::ui::components::{TabItem, copy_field, tabs};
use crate::ui::extract::TeamCtx;
use crate::ui::layout::{Crumb, app_page};
use crate::ui::{error, sse};

mod advanced;
mod connection;
mod create;
mod general;
mod header;
mod logs;
mod monitoring;
mod networks_tab;

pub(in crate::ui) use general::{SettingsSignals, schedule_hint};
pub(in crate::ui) use header::reloaded;

#[cfg(test)]
mod tests;

pub fn router() -> Router<AppState> {
	Router::new()
		.merge(create::router())
		.merge(header::router())
		.merge(general::router())
		.merge(connection::router())
		.merge(logs::router())
		.merge(monitoring::router())
		.merge(networks_tab::router())
		.merge(advanced::router())
}

// --- urls and small pieces ----------------------------------------------------

pub fn project_url(team: TeamId, project: ProjectId) -> String {
	format!("/teams/{team}/projects/{project}")
}

fn databases_url(team: TeamId, project: ProjectId) -> String {
	format!("{}/databases", project_url(team, project))
}

pub fn database_url(team: TeamId, project: ProjectId, database: DatabaseId) -> String {
	format!("{}/{database}", databases_url(team, project))
}

fn crumbs<'a>(ctx: &TeamCtx, project: &'a Project, last: &'a str) -> Vec<Crumb<'a>> {
	vec![
		("Projects", Some(format!("/teams/{}/projects", ctx.team.id))),
		(
			project.name.as_str(),
			Some(project_url(ctx.team.id, project.id)),
		),
		(last, None),
	]
}

pub fn engine_label(engine: EngineKind) -> &'static str {
	match engine {
		EngineKind::Postgres => "PostgreSQL",
		EngineKind::Mysql => "MySQL",
		EngineKind::Mongodb => "MongoDB",
		EngineKind::Redis => "Redis",
	}
}

pub fn engine_value(engine: EngineKind) -> &'static str {
	match engine {
		EngineKind::Postgres => "postgres",
		EngineKind::Mysql => "mysql",
		EngineKind::Mongodb => "mongodb",
		EngineKind::Redis => "redis",
	}
}

const ENGINES: [EngineKind; 4] = [
	EngineKind::Postgres,
	EngineKind::Mysql,
	EngineKind::Mongodb,
	EngineKind::Redis,
];

fn parse_engine(raw: &str) -> Result<EngineKind, ServerError> {
	ENGINES
		.into_iter()
		.find(|e| engine_value(*e) == raw)
		.ok_or_else(|| ServerError::Validation("Pick an engine.".to_owned()))
}

/// `2026-11-18 03:00 UTC`.
pub(in crate::ui) fn when(t: OffsetDateTime) -> String {
	format!(
		"{:04}-{:02}-{:02} {:02}:{:02} UTC",
		t.year(),
		u8::from(t.month()),
		t.day(),
		t.hour(),
		t.minute()
	)
}

/// A signal value as text, whether Datastar sent it as a string, a number or nothing.
fn lenient<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
	Ok(match Value::deserialize(d)? {
		Value::String(s) => s,
		Value::Number(n) => n.to_string(),
		_ => String::new(),
	})
}

fn number<T: std::str::FromStr>(raw: &str, what: &str) -> Result<Option<T>, ServerError> {
	let raw = raw.trim();
	if raw.is_empty() {
		return Ok(None);
	}
	raw.parse()
		.map(Some)
		.map_err(|_| ServerError::Validation(format!("{what} must be a number.")))
}

fn id<T>(raw: &str, what: &str, make: fn(i64) -> T) -> Result<Option<T>, ServerError> {
	let raw = raw.trim();
	if raw.is_empty() {
		return Ok(None);
	}
	raw.parse::<i64>()
		.map(|n| Some(make(n)))
		.map_err(|_| ServerError::Validation(format!("Pick a valid {what}.")))
}

/// A read-only field with a button that copies its value (`field::copy_field`: the
/// Clipboard API where the page is a secure context, a selected input over plain HTTP).
pub(in crate::ui) fn copy_row(label: &str, value: &str) -> Markup {
	let id: String = label
		.chars()
		.map(|c| {
			if c.is_ascii_alphanumeric() {
				c.to_ascii_lowercase()
			} else {
				'-'
			}
		})
		.collect();
	copy_field(&format!("copy-{id}"), label, value)
}

/// What a refusal of an action reads as in the header's error line.
pub(in crate::ui) fn action_error(msg: &str) -> sse::Events {
	sse::signals(&json!({ "_action_error": msg }))
}

// --- the tabs ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::ui) enum Tab {
	General,
	Connection,
	Backups,
	Monitoring,
	Networks,
	Logs,
	Advanced,
}

impl Tab {
	const ALL: [Self; 7] = [
		Self::General,
		Self::Connection,
		Self::Backups,
		Self::Monitoring,
		Self::Networks,
		Self::Logs,
		Self::Advanced,
	];

	const fn label(self) -> &'static str {
		match self {
			Self::General => "General",
			Self::Connection => "Connection",
			Self::Backups => "Backups",
			Self::Monitoring => "Monitoring",
			Self::Networks => "Networks",
			Self::Logs => "Logs",
			Self::Advanced => "Advanced",
		}
	}

	/// The path under the database's page.
	const fn path(self) -> &'static str {
		match self {
			Self::General => "",
			Self::Connection => "/connection",
			Self::Backups => "/backups",
			Self::Monitoring => "/monitoring",
			Self::Networks => "/networks",
			Self::Logs => "/logs",
			Self::Advanced => "/advanced",
		}
	}

	/// What a person must be allowed for the tab to be shown (and its page to open).
	pub(in crate::ui) fn allowed(self, ctx: &TeamCtx) -> bool {
		let can = |action, resource| ctx.perms.can(action, resource);
		match self {
			Self::General | Self::Logs => true,
			Self::Connection => can(Action::Update, Resource::Database),
			Self::Backups => can(Action::Read, Resource::Backup),
			Self::Monitoring | Self::Networks => can(Action::Read, Resource::Database),
			Self::Advanced => {
				can(Action::Update, Resource::Database) || can(Action::Delete, Resource::Database)
			}
		}
	}
}

fn tab_row(ctx: &TeamCtx, database: &Database, active: Tab) -> Markup {
	let base = database_url(ctx.team.id, database.project_id, database.id);
	let links: Vec<(Tab, String)> = Tab::ALL
		.into_iter()
		.filter(|t| t.allowed(ctx))
		.map(|t| (t, format!("{base}{}", t.path())))
		.collect();
	let items: Vec<TabItem<'_>> = links
		.iter()
		.map(|(tab, href)| TabItem {
			label: tab.label(),
			href,
			current: *tab == active,
		})
		.collect();
	tabs("Database sections", &items)
}

/// A tab's page that needs `tab.allowed` before anything is read.
pub(in crate::ui) fn forbid_unless_allowed(
	ctx: &TeamCtx,
	tab: Tab,
) -> Result<(), axum::response::Response> {
	if tab.allowed(ctx) {
		Ok(())
	} else {
		Err(error::page(ctx, &ServerError::Forbidden))
	}
}

// --- loading and the frame -----------------------------------------------------------------

pub(in crate::ui) async fn load(
	s: &AppState,
	ctx: &TeamCtx,
	project: ProjectId,
	database: DatabaseId,
) -> Result<(Project, DatabaseDetail), axum::response::Response> {
	let fail = |e: ServerError| error::page(ctx, &e);
	let project = projects::service::find(&s.db, ctx.user.id, ctx.team.id, project)
		.await
		.map_err(fail)?;
	let detail =
		crate::databases::service::detail(&s.db, ctx.user.id, ctx.team.id, project.id, database)
			.await
			.map_err(fail)?;
	Ok((project, detail))
}

/// True while the status, or a queued or running job, can still change it.
pub(in crate::ui) fn busy(d: &DatabaseDetail) -> bool {
	use crate::ui::status::Status;
	d.database.status.in_flight()
		|| matches!(d.job, Some((_, JobStatus::Queued | JobStatus::Running)))
}

/// What every tab's signals start as: the saved settings (the General and Backups forms
/// post them all together), and the slots the header, the dialogs and the actions use.
fn frame_signals(database: &Database) -> String {
	let text = |n: Option<String>| n.unwrap_or_default();
	json!({
		"public_port": text(database.public_port.map(|n| n.to_string())),
		"memory_limit_mb": text(database.memory_limit_mb.map(|n| n.to_string())),
		"cpu_limit": text(database.cpu_limit.map(|n| n.to_string())),
		"backup_target_id": text(database.backup_target_id.map(|n| n.0.to_string())),
		"backup_schedule": database.backup_schedule.clone().unwrap_or_default(),
		"backup_keep": database.backup_keep.to_string(),
		"confirm": "", "purge": false, "restore_id": "", "backup_id": "",
		"_restore_label": "", "_error": "", "_action_error": "", "_acting": false,
		"_delete_error": "", "_reveal_error": "", "_dialog_error": "", "_close": "",
	})
	.to_string()
}

/// The page frame of every tab: header, tab row, `content`, and the dialogs the header's
/// menu opens. The dialogs sit outside the header, which polling re-renders.
pub(in crate::ui) async fn page(
	s: &AppState,
	ctx: &TeamCtx,
	project: &Project,
	d: &DatabaseDetail,
	active: Tab,
	content: &Markup,
) -> Markup {
	let database = &d.database;
	// The environment crumb names the database's own environment, not the first one.
	let mut ctx = ctx.clone();
	if let Some(scope) = &mut ctx.scope
		&& let Some(env) = scope
			.environments
			.iter()
			.find(|e| e.id == database.environment_id)
	{
		scope.environment = env.clone();
	}
	let body = html! {
		div class="flex flex-col gap-6" data-signals=(frame_signals(database)) {
			(header::header(s, &ctx, d).await)
			(tab_row(&ctx, database, active))
			(content)
			(header::dialogs(&ctx, database))
		}
	};
	let title = match active {
		Tab::General => database.name.clone(),
		tab => format!("{} · {}", tab.label(), database.name),
	};
	app_page(&ctx, &title, &crumbs(&ctx, project, &database.name), &body)
}
