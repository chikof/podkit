//! Autodeploy use cases for the web UI: authorize, then scope, then validate, then touch the
//! database. Rules and freezes need `update application`; forcing a held push needs what a
//! manual deploy needs, `create deployment`.

use time::macros::format_description;
use time::{OffsetDateTime, PrimitiveDateTime, UtcOffset};

use super::rules::{self, Window};
use super::{Freeze, NewFreeze, PushRow, Settings, State, engine};
use crate::audit::{self, Diff, Target};
use crate::auth::permission::{Action, Resource, authorize};
use crate::error::{AppResult, ServerError};
use crate::ids::{ApplicationId, ProjectId, TeamId, UserId};
use crate::scope;
use crate::state::AppState;

/// How many recent pushes the page lists.
const RECENT: i64 = 15;
/// How many freezes an application may have.
const MAX_FREEZES: usize = 20;

/// What the settings form sends, as text.
#[derive(Debug, Clone, Default)]
#[allow(
	clippy::struct_excessive_bools,
	reason = "one flag per checkbox of the form"
)]
pub struct SettingsInput {
	pub enabled: bool,
	/// One glob per line.
	pub branches: String,
	pub deploy_tags: bool,
	/// One glob per line, `!` to exclude.
	pub paths: String,
	pub wait_for_ci: bool,
	/// Minutes, as typed.
	pub ci_timeout_min: String,
	pub deploy_on_timeout: bool,
	pub post_status: bool,
}

fn lines(raw: &str) -> Vec<String> {
	raw.lines()
		.map(str::trim)
		.filter(|l| !l.is_empty())
		.map(str::to_owned)
		.collect()
}

impl SettingsInput {
	/// The settings the form describes.
	///
	/// # Errors
	/// [`ServerError::Validation`] for a timeout that is not a number of minutes or a pattern
	/// that cannot be kept.
	pub fn parse(&self) -> Result<Settings, ServerError> {
		let minutes = self.ci_timeout_min.trim();
		let ci_timeout_secs = if minutes.is_empty() {
			Settings::default().ci_timeout_secs
		} else {
			minutes
				.parse::<i32>()
				.ok()
				.and_then(|m| m.checked_mul(60))
				.ok_or_else(|| {
					ServerError::Validation(
						"Wait for checks must be a number of minutes.".to_owned(),
					)
				})?
		};
		let settings = Settings {
			enabled: self.enabled,
			branch_patterns: lines(&self.branches),
			deploy_tags: self.deploy_tags,
			path_filters: lines(&self.paths),
			wait_for_ci: self.wait_for_ci,
			ci_timeout_secs,
			deploy_on_ci_timeout: self.deploy_on_timeout,
			post_status: self.post_status,
		};
		settings.validate()?;
		Ok(settings)
	}
}

/// What the freeze form sends, as text.
#[derive(Debug, Clone, Default)]
pub struct FreezeInput {
	/// `recurring` or `once`.
	pub kind: String,
	/// 0 (Monday) to 6.
	pub start_day: String,
	/// `HH:MM`.
	pub start_time: String,
	pub end_day: String,
	pub end_time: String,
	/// Minutes ahead of UTC of the clock the times are in.
	pub offset: String,
	/// `YYYY-MM-DDTHH:MM`, from a `datetime-local` input.
	pub from: String,
	pub to: String,
	pub note: String,
}

fn time_of_day(raw: &str, what: &str) -> Result<i64, ServerError> {
	let bad = || ServerError::Validation(format!("{what} must be a time like 16:00."));
	let (h, m) = raw.trim().split_once(':').ok_or_else(bad)?;
	let (h, m) = (
		h.parse::<i64>().map_err(|_| bad())?,
		m.get(..2).unwrap_or(m).parse::<i64>().map_err(|_| bad())?,
	);
	if (0..24).contains(&h) && (0..60).contains(&m) {
		Ok(h * 60 + m)
	} else {
		Err(bad())
	}
}

fn day(raw: &str, what: &str) -> Result<i64, ServerError> {
	raw.trim()
		.parse::<i64>()
		.ok()
		.filter(|d| (0..7).contains(d))
		.ok_or_else(|| ServerError::Validation(format!("{what} must be a day of the week.")))
}

fn local(raw: &str, offset: UtcOffset, what: &str) -> Result<OffsetDateTime, ServerError> {
	let parsed = PrimitiveDateTime::parse(
		raw.trim(),
		format_description!("[year]-[month]-[day]T[hour]:[minute]"),
	)
	.or_else(|_| {
		PrimitiveDateTime::parse(
			raw.trim(),
			format_description!("[year]-[month]-[day]T[hour]:[minute]:[second]"),
		)
	})
	.map_err(|_| ServerError::Validation(format!("{what} must be a date and a time.")))?;
	Ok(parsed.assume_offset(offset))
}

impl FreezeInput {
	/// The freeze the form describes.
	///
	/// # Errors
	/// [`ServerError::Validation`] naming the field.
	pub fn parse(&self) -> Result<NewFreeze, ServerError> {
		let offset_minutes = match self.offset.trim() {
			"" => 0,
			raw => raw
				.parse::<i32>()
				.ok()
				.filter(|m| (-840..=840).contains(m))
				.ok_or_else(|| {
					ServerError::Validation("The UTC offset is not valid.".to_owned())
				})?,
		};
		let note: String = self.note.trim().chars().take(120).collect();
		match self.kind.as_str() {
			"recurring" => {
				let start = rules::minute_of_week(
					day(&self.start_day, "The first day")?,
					0,
					time_of_day(&self.start_time, "The start time")?,
				);
				let end = rules::minute_of_week(
					day(&self.end_day, "The last day")?,
					0,
					time_of_day(&self.end_time, "The end time")?,
				);
				if start == end {
					return Err(ServerError::Validation(
						"A freeze must end at a different time than it starts.".to_owned(),
					));
				}
				Ok(NewFreeze {
					kind: "recurring",
					start_minute: i32::try_from(start).ok(),
					end_minute: i32::try_from(end).ok(),
					utc_offset_minutes: offset_minutes,
					starts_at: None,
					ends_at: None,
					note,
				})
			}
			"once" => {
				let offset = UtcOffset::from_whole_seconds(offset_minutes * 60).map_err(|_| {
					ServerError::Validation("The UTC offset is not valid.".to_owned())
				})?;
				let from = local(&self.from, offset, "The start")?;
				let to = local(&self.to, offset, "The end")?;
				if to <= from {
					return Err(ServerError::Validation(
						"The freeze must end after it starts.".to_owned(),
					));
				}
				Ok(NewFreeze {
					kind: "once",
					start_minute: None,
					end_minute: None,
					utc_offset_minutes: offset_minutes,
					starts_at: Some(from),
					ends_at: Some(to),
					note,
				})
			}
			_ => Err(ServerError::Validation(
				"A freeze repeats every week or happens once.".to_owned(),
			)),
		}
	}
}

/// Everything the Git tab shows.
pub struct View {
	/// `None`: nothing was ever saved, and the plain behaviour applies.
	pub settings: Option<Settings>,
	pub freezes: Vec<Freeze>,
	pub pushes: Vec<PushRow>,
	/// Whether a freeze window is open right now.
	pub frozen_now: bool,
}

/// The tab's data; needs `read application`.
pub async fn get(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	app: ApplicationId,
) -> AppResult<View> {
	authorize(&s.db, user, team, Action::Read, Resource::Application).await?;
	let app = scope::application(&s.db, team, project, app).await?;
	let freezes = super::freezes(&s.db, app.id).await?;
	let windows: Vec<Window> = freezes.iter().filter_map(Freeze::window).collect();
	Ok(View {
		settings: super::settings(&s.db, app.id).await?,
		frozen_now: rules::frozen_at(&windows, OffsetDateTime::now_utc()),
		freezes,
		pushes: super::recent(&s.db, app.id, RECENT).await?,
	})
}

/// Stores the rules; needs `update application`.
pub async fn save(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	app: ApplicationId,
	input: &SettingsInput,
) -> AppResult<Settings> {
	authorize(&s.db, user, team, Action::Update, Resource::Application).await?;
	let app = scope::application(&s.db, team, project, app).await?;
	let settings = input.parse()?;
	let before = super::settings(&s.db, app.id)
		.await?
		.unwrap_or_else(Settings::form_default);
	super::save_settings(&s.db, app.id, &settings).await?;
	audit::log(
		&s.db,
		user,
		team,
		"app.autodeploy",
		Target::new("application", app.id.0)
			.named(&app.slug)
			.in_project(project),
		Diff::new()
			.changed("enabled", before.enabled, settings.enabled)
			.changed("deploy_tags", before.deploy_tags, settings.deploy_tags)
			.changed("wait_for_ci", before.wait_for_ci, settings.wait_for_ci)
			.changed(
				"ci_timeout_secs",
				i64::from(before.ci_timeout_secs),
				i64::from(settings.ci_timeout_secs),
			)
			.changed(
				"deploy_on_ci_timeout",
				before.deploy_on_ci_timeout,
				settings.deploy_on_ci_timeout,
			)
			.changed("post_status", before.post_status, settings.post_status)
			.changed(
				"branch_patterns",
				before.branch_patterns.join(", "),
				settings.branch_patterns.join(", "),
			)
			.changed(
				"path_filters",
				before.path_filters.join(", "),
				settings.path_filters.join(", "),
			),
	)
	.await;
	Ok(settings)
}

/// Adds a freeze window; needs `update application`. A push that is waiting stays waiting, and
/// the background pass holds it from the next look.
pub async fn add_freeze(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	app: ApplicationId,
	input: &FreezeInput,
) -> AppResult<i64> {
	authorize(&s.db, user, team, Action::Update, Resource::Application).await?;
	let app = scope::application(&s.db, team, project, app).await?;
	let freeze = input.parse()?;
	if super::freezes(&s.db, app.id).await?.len() >= MAX_FREEZES {
		return Err(ServerError::Validation(format!(
			"An application can have {MAX_FREEZES} freezes. Remove one first."
		)));
	}
	let id = super::add_freeze(&s.db, app.id, &freeze, user).await?;
	audit::log(
		&s.db,
		user,
		team,
		"app.autodeploy.freeze_add",
		Target::new("application", app.id.0)
			.named(&app.slug)
			.in_project(project),
		Diff::new()
			.set("kind", freeze.kind)
			.set("note", freeze.note.clone()),
	)
	.await;
	Ok(id)
}

/// Removes a freeze window; needs `update application`. A freeze of another application is
/// not found.
pub async fn remove_freeze(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	app: ApplicationId,
	freeze: i64,
) -> AppResult<()> {
	authorize(&s.db, user, team, Action::Update, Resource::Application).await?;
	let app = scope::application(&s.db, team, project, app).await?;
	if !super::remove_freeze(&s.db, app.id, freeze).await? {
		return Err(ServerError::Validation(
			"That freeze is already gone.".to_owned(),
		));
	}
	audit::log(
		&s.db,
		user,
		team,
		"app.autodeploy.freeze_remove",
		Target::new("application", app.id.0)
			.named(&app.slug)
			.in_project(project),
		Diff::new().set("freeze_id", freeze),
	)
	.await;
	Ok(())
}

/// Deploys a push that is waiting for its checks or for a freeze to end, without waiting;
/// needs `create deployment`. A push that is not waiting any more is a validation error: it
/// was already deployed, replaced or dropped.
pub async fn force(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	app: ApplicationId,
	push: i64,
) -> AppResult<engine::Advance> {
	authorize(&s.db, user, team, Action::Create, Resource::Deployment).await?;
	let app = scope::application(&s.db, team, project, app).await?;
	let row = super::find_push(&s.db, app.id, push)
		.await?
		.ok_or(ServerError::ApplicationNotFound)?;
	if !row.state().is_some_and(State::pending) {
		return Err(ServerError::Validation(
			"That push is no longer waiting.".to_owned(),
		));
	}
	let sha = row.commit_sha.clone();
	let result = engine::advance(s, row, OffsetDateTime::now_utc(), Some(user)).await?;
	if matches!(result, engine::Advance::Gone) {
		return Err(ServerError::Validation(
			"That push is no longer waiting.".to_owned(),
		));
	}
	audit::log(
		&s.db,
		user,
		team,
		"app.autodeploy.force",
		Target::new("application", app.id.0)
			.named(&app.slug)
			.in_project(project),
		Diff::new().set("commit", sha),
	)
	.await;
	Ok(result)
}
