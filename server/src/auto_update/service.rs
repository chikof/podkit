//! Auto-update use cases for the JSON API and the web UI: authorize, then scope, then
//! validate, then touch the database. Reading needs `read application`, the settings need
//! `update application`, and Check now, which may deploy, needs what a manual deploy needs,
//! `create deployment`.

use time::{Duration, OffsetDateTime};

use super::engine::{self, Trigger};
use super::rules::{self, Interval, Window};
use super::{Row, Settings};
use crate::applications::{Application, SourceKind};
use crate::audit::{self, Diff, Target};
use crate::auth::permission::{Action, Resource, authorize};
use crate::autodeploy;
use crate::error::{AppResult, ServerError};
use crate::ids::{ApplicationId, ProjectId, TeamId, UserId};
use crate::scope;
use crate::state::AppState;

/// What the settings form (or the API) sends, as text.
#[derive(Debug, Clone, Default)]
pub struct SettingsInput {
	pub enabled: bool,
	/// `hourly`, `6h` or `daily`; empty is `daily`.
	pub interval: String,
	/// `HH:MM`, both empty for no window.
	pub window_start: String,
	pub window_end: String,
	/// Minutes ahead of UTC of the clock the window is in.
	pub offset: String,
}

impl SettingsInput {
	/// The settings the input describes.
	///
	/// # Errors
	/// [`ServerError::Validation`] naming the field.
	pub fn parse(&self) -> Result<Settings, ServerError> {
		let interval = match self.interval.trim() {
			"" => Interval::Daily,
			raw => Interval::parse(raw).ok_or_else(|| {
				ServerError::Validation(
					"Check every hour, every 6 hours or every day (hourly, 6h, daily).".to_owned(),
				)
			})?,
		};
		let offset = match self.offset.trim() {
			"" => 0,
			raw => raw
				.parse::<i64>()
				.ok()
				.filter(|m| (-840..=840).contains(m))
				.ok_or_else(|| {
					ServerError::Validation("The UTC offset is not valid.".to_owned())
				})?,
		};
		let window = match (self.window_start.trim(), self.window_end.trim()) {
			("", "") => None,
			(start, end) => {
				let time = |raw: &str, what: &str| {
					rules::parse_time(raw).ok_or_else(|| {
						ServerError::Validation(format!("{what} must be a time like 02:00."))
					})
				};
				let (start, end) = (
					time(start, "The window start")?,
					time(end, "The window end")?,
				);
				if start == end {
					return Err(ServerError::Validation(
						"The window must end at a different time than it starts.".to_owned(),
					));
				}
				Some(Window { start, end, offset })
			}
		};
		Ok(Settings {
			enabled: self.enabled,
			interval,
			window,
		})
	}
}

/// What the page and the API show.
#[derive(Debug, Clone)]
pub struct View {
	/// `None`: never set, auto-update is off.
	pub row: Option<Row>,
	/// The image reference, for an image application.
	pub image: Option<String>,
	/// Why the application cannot be auto-updated, if it cannot.
	pub unavailable: Option<&'static str>,
	/// Whether an autodeploy freeze window is open now.
	pub frozen_now: bool,
}

impl View {
	#[must_use]
	pub fn settings(&self) -> Settings {
		self.row.as_ref().map(Row::settings).unwrap_or_default()
	}
}

fn unavailable(app: &Application) -> Option<&'static str> {
	match (&app.source_kind, app.image.as_deref()) {
		(SourceKind::Image, Some(image)) => rules::trackable(image).err(),
		_ => Some("Only an application deployed from a registry image can be auto-updated."),
	}
}

async fn view(s: &AppState, app: &Application) -> AppResult<View> {
	let windows = autodeploy::windows(&s.db, app.id).await?;
	Ok(View {
		row: super::get(&s.db, app.id).await?,
		image: app.image.clone(),
		unavailable: unavailable(app),
		frozen_now: autodeploy::rules::frozen_at(&windows, OffsetDateTime::now_utc()),
	})
}

/// The application's auto-update; needs `read application`.
pub async fn get(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	app: ApplicationId,
) -> AppResult<View> {
	authorize(&s.db, user, team, Action::Read, Resource::Application).await?;
	let app = scope::application(&s.db, team, project, app).await?;
	view(s, &app).await
}

fn window_text(w: Option<Window>) -> String {
	w.map_or_else(String::new, |w| {
		format!(
			"{}-{} {:+}min",
			rules::time_label(w.start),
			rules::time_label(w.end),
			w.offset
		)
	})
}

/// Stores the settings; needs `update application`. Turning it on (or changing when it looks)
/// schedules the first check at once, inside the window when there is one.
pub async fn save(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	app: ApplicationId,
	input: &SettingsInput,
) -> AppResult<View> {
	authorize(&s.db, user, team, Action::Update, Resource::Application).await?;
	let app = scope::application(&s.db, team, project, app).await?;
	let settings = input.parse()?;
	if settings.enabled
		&& let Some(why) = unavailable(&app)
	{
		return Err(ServerError::Validation(why.to_owned()));
	}
	let before = super::get(&s.db, app.id).await?;
	let old = before.as_ref().map(Row::settings).unwrap_or_default();
	let next = if !settings.enabled {
		None
	} else if old == settings {
		before.as_ref().and_then(|r| r.next_check_at)
	} else {
		Some(rules::next_check(
			OffsetDateTime::now_utc(),
			Duration::ZERO,
			settings.window,
		))
	};
	super::save(&s.db, app.id, &settings, next).await?;
	audit::log(
		&s.db,
		user,
		team,
		"app.auto_update",
		Target::new("application", app.id.0)
			.named(&app.slug)
			.in_project(project),
		Diff::new()
			.changed("enabled", old.enabled, settings.enabled)
			.changed(
				"interval",
				old.interval.as_str(),
				settings.interval.as_str(),
			)
			.changed(
				"window",
				window_text(old.window),
				window_text(settings.window),
			),
	)
	.await;
	view(s, &app).await
}

/// Checks the tag now and deploys a new digest like a scheduled check would, without waiting
/// for the maintenance window (freezes still hold it); needs `create deployment`. Auto-update
/// must be on.
pub async fn check_now(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	app: ApplicationId,
) -> AppResult<View> {
	authorize(&s.db, user, team, Action::Create, Resource::Deployment).await?;
	let app = scope::application(&s.db, team, project, app).await?;
	if !super::enabled(&s.db, app.id).await? {
		return Err(ServerError::Validation(
			"Turn auto-update on first.".to_owned(),
		));
	}
	let row = engine::check(s, app.id, Trigger::Manual(user)).await?;
	audit::log(
		&s.db,
		user,
		team,
		"app.auto_update.check",
		Target::new("application", app.id.0)
			.named(&app.slug)
			.in_project(project),
		Diff::new()
			.set(
				"result",
				row.as_ref()
					.and_then(Row::last)
					.map_or("", |c| c.as_str())
					.to_owned(),
			)
			.set(
				"digest",
				row.as_ref()
					.and_then(|r| r.remote_digest.clone())
					.unwrap_or_default(),
			)
			.set("trigger", engine::TRIGGER.to_owned()),
	)
	.await;
	view(s, &app).await
}
