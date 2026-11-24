//! Notification use cases shared by the JSON API and the web UI: authorize, then scope, then
//! validate, then touch the database. Secrets go in sealed and come out only to send.

use sqlx::PgPool;
use sqlx::types::Json;
use time::OffsetDateTime;

use super::alerts::{ALERT_COLUMNS, Metric, MetricAlert};
use super::channel::{
	CHANNEL_COLUMNS, Channel, ChannelFields, ChannelKind, build, open_secret, seal_secret,
};
use super::event::{EventKind, Severity};
use super::format::Message;
use super::settings::{self, MAX_DEDUP_MINUTES, Security, Settings, Smtp};
use super::{hooks, send};
use crate::audit::{self, Diff, Target};
use crate::auth::permission::{self, Action, Resource, authorize};
use crate::db::is_unique_violation;
use crate::error::{AppResult, ServerError};
use crate::ids::{
	EnvironmentId, MetricAlertId, NotificationChannelId, NotificationDeliveryId,
	NotificationEventId, NotificationRuleId, ProjectId, ServerId, TeamId, UserId,
};
use crate::scope;
use crate::state::AppState;

const MAX_NAME_LEN: usize = 60;

fn invalid(what: &str) -> ServerError {
	ServerError::Validation(format!("invalid {what}"))
}

/// One audit event for a change of this slice.
async fn note(db: &PgPool, user: UserId, team: TeamId, action: &str, target: Target, diff: Diff) {
	audit::log(db, user, team, action, target, diff).await;
}

fn name_of(raw: &str) -> AppResult<String> {
	let name = raw.trim();
	if name.is_empty() || name.chars().count() > MAX_NAME_LEN || name.chars().any(char::is_control)
	{
		return Err(invalid("name"));
	}
	Ok(name.to_owned())
}

fn not_found(what: &'static str) -> ServerError {
	ServerError::NotificationNotFound(what)
}

// --- channels -------------------------------------------------------------------------------

/// The team's channels. Each still carries its sealed secret; callers must not render it.
/// Needs `read notification`.
pub async fn channels(db: &PgPool, user: UserId, team: TeamId) -> AppResult<Vec<Channel>> {
	authorize(db, user, team, Action::Read, Resource::Notification).await?;
	Ok(sqlx::query_as::<_, Channel>(sqlx::AssertSqlSafe(format!(
		"SELECT {CHANNEL_COLUMNS} FROM notification_channels WHERE team_id = $1 ORDER BY name"
	)))
	.bind(team)
	.fetch_all(db)
	.await?)
}

async fn channel_in(db: &PgPool, team: TeamId, id: NotificationChannelId) -> AppResult<Channel> {
	sqlx::query_as::<_, Channel>(sqlx::AssertSqlSafe(format!(
		"SELECT {CHANNEL_COLUMNS} FROM notification_channels WHERE id = $1 AND team_id = $2"
	)))
	.bind(id)
	.bind(team)
	.fetch_optional(db)
	.await?
	.ok_or_else(|| not_found("channel"))
}

fn name_taken(e: sqlx::Error, constraint: &str, what: &str) -> ServerError {
	if is_unique_violation(&e, constraint) {
		ServerError::Conflict(format!("a {what} with that name already exists"))
	} else {
		e.into()
	}
}

/// Adds a channel. Needs `create notification`.
pub async fn create_channel(
	s: &AppState,
	user: UserId,
	team: TeamId,
	name: &str,
	kind: ChannelKind,
	fields: &ChannelFields,
) -> AppResult<Channel> {
	authorize(&s.db, user, team, Action::Create, Resource::Notification).await?;
	let name = name_of(name)?;
	let built = build(kind, fields)?;
	vet_built(s, &built).await?;
	let sealed = built
		.secret
		.as_ref()
		.map(|secret| seal_secret(&s.secrets, secret))
		.transpose()?;
	let channel = sqlx::query_as::<_, Channel>(sqlx::AssertSqlSafe(format!(
		"INSERT INTO notification_channels (id, team_id, name, kind, config, hint, secret)
		VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING {CHANNEL_COLUMNS}"
	)))
	.bind(NotificationChannelId::generate())
	.bind(team)
	.bind(&name)
	.bind(kind)
	.bind(Json(&built.config))
	.bind(&built.hint)
	.bind(sealed)
	.fetch_one(&s.db)
	.await
	.map_err(|e| name_taken(e, "notification_channels_team_id_name_key", "channel"))?;
	audit::log(
		&s.db,
		user,
		team,
		"notification_channel.create",
		Target::new("notification_channel", channel.id.0).named(&channel.name),
		Diff::new()
			.set("kind", kind.as_str())
			.set("secret_set", built.secret.is_some()),
	)
	.await;
	Ok(channel)
}

/// What an edit may change. `fields` replaces the settings it carries; a blank secret keeps the
/// stored one, so a page can offer "leave empty to keep".
#[derive(Debug, Default)]
pub struct ChannelEdit {
	pub name: Option<String>,
	pub enabled: Option<bool>,
	pub fields: Option<ChannelFields>,
}

/// Renames, switches on or off, or re-points a channel. Needs `update notification`.
pub async fn update_channel(
	s: &AppState,
	user: UserId,
	team: TeamId,
	id: NotificationChannelId,
	edit: ChannelEdit,
) -> AppResult<Channel> {
	authorize(&s.db, user, team, Action::Update, Resource::Notification).await?;
	let current = channel_in(&s.db, team, id).await?;
	let name = match &edit.name {
		Some(n) => name_of(n)?,
		None => current.name.clone(),
	};
	let enabled = edit.enabled.unwrap_or(current.enabled);
	let (config, hint, sealed) = if let Some(fields) = edit.fields {
		// Start from what is stored so a blank field keeps its value.
		let old_secret = open_secret(&s.secrets, &current)?;
		let old = &current.config.0;
		let merged = ChannelFields {
			url: or_keep(&fields.url, old_secret.url.as_deref()),
			token: or_keep(&fields.token, old_secret.token.as_deref()),
			signing_secret: or_keep(&fields.signing_secret, old_secret.signing.as_deref()),
			chat_id: or_keep(&fields.chat_id, old.chat_id.as_deref()),
			api_base: or_keep(&fields.api_base, old.api_base.as_deref()),
			server: or_keep(&fields.server, old.server.as_deref()),
			topic: or_keep(&fields.topic, old.topic.as_deref()),
			to: or_keep(&fields.to, old.to.as_deref()),
		};
		let built = build(current.kind, &merged)?;
		vet_built(s, &built).await?;
		let sealed = built
			.secret
			.as_ref()
			.map(|secret| seal_secret(&s.secrets, secret))
			.transpose()?;
		(built.config, built.hint, sealed)
	} else {
		(
			current.config.0.clone(),
			current.hint.clone(),
			current.secret.clone(),
		)
	};
	let updated = sqlx::query_as::<_, Channel>(sqlx::AssertSqlSafe(format!(
		"UPDATE notification_channels SET name = $3, enabled = $4, config = $5, hint = $6, secret = $7
		WHERE id = $1 AND team_id = $2 RETURNING {CHANNEL_COLUMNS}"
	)))
	.bind(id)
	.bind(team)
	.bind(&name)
	.bind(enabled)
	.bind(Json(&config))
	.bind(&hint)
	.bind(sealed)
	.fetch_one(&s.db)
	.await
	.map_err(|e| name_taken(e, "notification_channels_team_id_name_key", "channel"))?;
	audit::log(
		&s.db,
		user,
		team,
		"notification_channel.update",
		Target::new("notification_channel", id.0).named(&updated.name),
		Diff::new()
			.changed("name", current.name.as_str(), updated.name.as_str())
			.changed("enabled", current.enabled, updated.enabled)
			.changed("hint", current.hint.as_str(), updated.hint.as_str()),
	)
	.await;
	Ok(updated)
}

/// Refuses a channel whose address podkit would not send to: see `net::outbound`. It is
/// checked again at every send, because a name can start to point somewhere else.
async fn vet_built(s: &AppState, built: &super::channel::Built) -> AppResult<()> {
	let urls = [
		built
			.secret
			.as_ref()
			.and_then(|secret| secret.url.as_deref()),
		built.config.api_base.as_deref(),
		built.config.server.as_deref(),
	];
	for url in urls.into_iter().flatten() {
		s.outbound
			.vet_url(url)
			.await
			.map_err(|_| crate::net::outbound::refused())?;
	}
	Ok(())
}

fn or_keep(given: &str, stored: Option<&str>) -> String {
	if given.trim().is_empty() {
		stored.unwrap_or_default().to_owned()
	} else {
		given.to_owned()
	}
}

/// Deletes a channel with its queued deliveries; rules lose it. Needs `delete notification`.
pub async fn remove_channel(
	db: &PgPool,
	user: UserId,
	team: TeamId,
	id: NotificationChannelId,
) -> AppResult<()> {
	authorize(db, user, team, Action::Delete, Resource::Notification).await?;
	let done = sqlx::query("DELETE FROM notification_channels WHERE id = $1 AND team_id = $2")
		.bind(id)
		.bind(team)
		.execute(db)
		.await?;
	if done.rows_affected() == 0 {
		return Err(not_found("channel"));
	}
	audit::log(
		db,
		user,
		team,
		"notification_channel.delete",
		Target::new("notification_channel", id.0),
		Diff::new(),
	)
	.await;
	Ok(())
}

/// How a "Send test" went: what the destination said, never the destination itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestOutcome {
	pub ok: bool,
	pub message: String,
}

/// Sends a test message through the channel right away (not through the queue) and remembers
/// the outcome on the channel. Needs `update notification`.
pub async fn test_channel(
	s: &AppState,
	user: UserId,
	team: TeamId,
	id: NotificationChannelId,
) -> AppResult<TestOutcome> {
	authorize(&s.db, user, team, Action::Update, Resource::Notification).await?;
	let channel = channel_in(&s.db, team, id).await?;
	let msg = Message {
		kind: "test".to_owned(),
		severity: Severity::Info,
		title: "Test notification from podkit".to_owned(),
		body: format!("If you can read this, “{}” works.", channel.name),
		link: Some(format!("/teams/{team}/settings/notifications")),
		count: 1,
		at: OffsetDateTime::now_utc()
			.format(&time::format_description::well_known::Rfc3339)
			.unwrap_or_default(),
		team_id: team.0,
		project_id: None,
		environment_id: None,
		event_id: None,
		digest: Vec::new(),
		test: true,
	};
	let result = send::send(s, &channel, &msg).await;
	let (ok, error) = match &result {
		Ok(()) => (true, None),
		Err(e) => (false, Some(e.as_str())),
	};
	sqlx::query(
		"UPDATE notification_channels SET last_test_at = NOW(), last_test_ok = $2, last_test_error = $3
		WHERE id = $1",
	)
	.bind(id)
	.bind(ok)
	.bind(error)
	.execute(&s.db)
	.await?;
	Ok(TestOutcome {
		ok,
		message: match result {
			Ok(()) => "Test message sent.".to_owned(),
			Err(e) => e,
		},
	})
}

// --- SMTP -----------------------------------------------------------------------------------

/// The team's SMTP server. Needs `read notification`. The password stays sealed in the result.
pub async fn smtp(db: &PgPool, user: UserId, team: TeamId) -> AppResult<Option<Smtp>> {
	authorize(db, user, team, Action::Read, Resource::Notification).await?;
	Ok(settings::load_smtp(db, team).await?)
}

#[derive(Debug, Clone, Default)]
pub struct SmtpInput {
	pub host: String,
	pub port: i32,
	pub security: String,
	pub username: String,
	/// Blank keeps the stored password.
	pub password: String,
	pub from_addr: String,
}

/// Saves the team's SMTP server. Needs `update notification`.
pub async fn save_smtp(
	s: &AppState,
	user: UserId,
	team: TeamId,
	input: SmtpInput,
) -> AppResult<()> {
	authorize(&s.db, user, team, Action::Update, Resource::Notification).await?;
	let host = input.host.trim();
	if host.is_empty()
		|| host.len() > 253
		|| !host
			.chars()
			.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':' | '[' | ']'))
	{
		return Err(invalid("SMTP host"));
	}
	let port = u16::try_from(input.port)
		.ok()
		.filter(|p| *p > 0)
		.ok_or_else(|| invalid("SMTP port"))?;
	s.outbound
		.resolve(host, port)
		.await
		.map_err(|_| crate::net::outbound::refused())?;
	let security = Security::parse(&input.security).ok_or_else(|| invalid("security"))?;
	let from = input.from_addr.trim();
	if from.parse::<lettre::message::Mailbox>().is_err() || from.chars().any(char::is_control) {
		return Err(invalid("From address"));
	}
	let username = Some(input.username.trim().to_owned()).filter(|u| !u.is_empty());
	if username
		.as_deref()
		.is_some_and(|u| u.len() > 255 || u.chars().any(char::is_control))
	{
		return Err(invalid("SMTP username"));
	}
	let existing = settings::load_smtp(&s.db, team).await?;
	let password = if input.password.is_empty() {
		// Keeping the password only makes sense while there is a username to use it with.
		username.as_ref().and(existing.and_then(|e| e.password))
	} else if username.is_some() && input.password.len() <= 4096 {
		Some(s.secrets.encrypt(&input.password).map_err(|e| {
			tracing::error!("failed to encrypt the SMTP password: {e}");
			ServerError::Internal
		})?)
	} else {
		return Err(invalid("SMTP password"));
	};
	sqlx::query(
		"INSERT INTO notification_smtp (team_id, host, port, security, username, password, from_addr)
		VALUES ($1, $2, $3, $4, $5, $6, $7)
		ON CONFLICT (team_id) DO UPDATE SET host = $2, port = $3, security = $4, username = $5,
			password = $6, from_addr = $7",
	)
	.bind(team)
	.bind(host)
	.bind(i32::from(port))
	.bind(security.as_str())
	.bind(username.clone())
	.bind(password)
	.bind(from)
	.execute(&s.db)
	.await?;
	note(
		&s.db,
		user,
		team,
		"notification_smtp.set",
		Target::none("notification_smtp"),
		Diff::new()
			.set("host", host)
			.set("port", i64::from(port))
			.set("security", security.as_str())
			.set("username_set", username.is_some())
			.secret("password"),
	)
	.await;
	Ok(())
}

/// Forgets the team's SMTP server. Needs `delete notification`.
pub async fn clear_smtp(db: &PgPool, user: UserId, team: TeamId) -> AppResult<()> {
	authorize(db, user, team, Action::Delete, Resource::Notification).await?;
	sqlx::query("DELETE FROM notification_smtp WHERE team_id = $1")
		.bind(team)
		.execute(db)
		.await?;
	note(
		db,
		user,
		team,
		"notification_smtp.clear",
		Target::none("notification_smtp"),
		Diff::new(),
	)
	.await;
	Ok(())
}

// --- noise control --------------------------------------------------------------------------

/// The team's dedup window and quiet hours. Needs `read notification`.
pub async fn settings(db: &PgPool, user: UserId, team: TeamId) -> AppResult<Settings> {
	authorize(db, user, team, Action::Read, Resource::Notification).await?;
	Ok(settings::load(db, team).await?)
}

/// Saves them. Needs `update notification`.
pub async fn save_settings(
	db: &PgPool,
	user: UserId,
	team: TeamId,
	new: &Settings,
) -> AppResult<()> {
	authorize(db, user, team, Action::Update, Resource::Notification).await?;
	let minute = |m: i32| (0..24 * 60).contains(&m);
	if !(1..=MAX_DEDUP_MINUTES).contains(&new.dedup_minutes)
		|| !minute(new.quiet_start_min)
		|| !minute(new.quiet_end_min)
		|| !(-14 * 60..=14 * 60).contains(&new.utc_offset_min)
	{
		return Err(invalid("noise control settings"));
	}
	settings::save(db, team, new).await?;
	note(
		db,
		user,
		team,
		"notification_settings.update",
		Target::none("notification_settings"),
		Diff::new()
			.set("dedup_minutes", i64::from(new.dedup_minutes))
			.set("quiet_start_min", i64::from(new.quiet_start_min))
			.set("quiet_end_min", i64::from(new.quiet_end_min)),
	)
	.await;
	Ok(())
}

// --- rules ----------------------------------------------------------------------------------

/// A row of `notification_rules` with the channels it sends to.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Rule {
	pub id: NotificationRuleId,
	pub name: String,
	pub kinds: Vec<String>,
	pub project_id: Option<ProjectId>,
	pub environment_id: Option<EnvironmentId>,
	pub min_severity: i16,
	pub enabled: bool,
	pub channel_ids: Vec<i64>,
}

const RULE_SELECT: &str = "SELECT r.id, r.name, r.kinds, r.project_id, r.environment_id,
	r.min_severity, r.enabled,
	COALESCE(ARRAY(SELECT channel_id FROM notification_rule_channels
		WHERE rule_id = r.id ORDER BY channel_id), '{}') AS channel_ids
	FROM notification_rules r";

/// The team's rules. Needs `read notification`.
pub async fn rules(db: &PgPool, user: UserId, team: TeamId) -> AppResult<Vec<Rule>> {
	authorize(db, user, team, Action::Read, Resource::Notification).await?;
	Ok(sqlx::query_as::<_, Rule>(sqlx::AssertSqlSafe(format!(
		"{RULE_SELECT} WHERE r.team_id = $1 ORDER BY r.name"
	)))
	.bind(team)
	.fetch_all(db)
	.await?)
}

#[derive(Debug, Clone)]
pub struct RuleInput {
	pub name: String,
	pub kinds: Vec<String>,
	pub channel_ids: Vec<i64>,
	pub project_id: Option<i64>,
	pub environment_id: Option<i64>,
	pub min_severity: Severity,
	pub enabled: bool,
}

async fn check_rule(
	db: &PgPool,
	team: TeamId,
	input: &RuleInput,
) -> AppResult<(String, Vec<String>)> {
	let name = name_of(&input.name)?;
	let mut kinds: Vec<String> = Vec::new();
	for raw in &input.kinds {
		let kind = EventKind::parse(raw).ok_or_else(|| invalid("event kind"))?;
		if !kinds.iter().any(|k| k == kind.as_str()) {
			kinds.push(kind.as_str().to_owned());
		}
	}
	if kinds.is_empty() {
		return Err(ServerError::Validation(
			"pick at least one event for the rule".to_owned(),
		));
	}
	if input.channel_ids.is_empty() {
		return Err(ServerError::Validation(
			"pick at least one channel for the rule".to_owned(),
		));
	}
	// A channel of another team does not exist.
	let known = sqlx::query_scalar::<_, i64>(
		"SELECT COUNT(*) FROM notification_channels WHERE team_id = $1 AND id = ANY($2)",
	)
	.bind(team)
	.bind(&input.channel_ids)
	.fetch_one(db)
	.await?;
	let mut distinct = input.channel_ids.clone();
	distinct.sort_unstable();
	distinct.dedup();
	if usize::try_from(known).ok() != Some(distinct.len()) {
		return Err(not_found("channel"));
	}
	if let Some(project) = input.project_id {
		scope::project(db, team, ProjectId(project)).await?;
	}
	if let Some(environment) = input.environment_id {
		let project = input.project_id.ok_or_else(|| {
			ServerError::Validation("an environment needs its project".to_owned())
		})?;
		let found = sqlx::query_scalar::<_, bool>(
			"SELECT EXISTS (SELECT 1 FROM environments WHERE id = $1 AND project_id = $2)",
		)
		.bind(environment)
		.bind(project)
		.fetch_one(db)
		.await?;
		if !found {
			return Err(ServerError::EnvironmentNotFound);
		}
	}
	Ok((name, kinds))
}

/// Adds a rule. Needs `create notification`.
pub async fn create_rule(
	db: &PgPool,
	user: UserId,
	team: TeamId,
	input: RuleInput,
) -> AppResult<NotificationRuleId> {
	authorize(db, user, team, Action::Create, Resource::Notification).await?;
	let (name, kinds) = check_rule(db, team, &input).await?;
	let id = NotificationRuleId::generate();
	let mut tx = db.begin().await?;
	sqlx::query(
		"INSERT INTO notification_rules
			(id, team_id, name, kinds, project_id, environment_id, min_severity, enabled)
		VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
	)
	.bind(id)
	.bind(team)
	.bind(&name)
	.bind(&kinds)
	.bind(input.project_id)
	.bind(input.environment_id)
	.bind(input.min_severity.rank())
	.bind(input.enabled)
	.execute(&mut *tx)
	.await
	.map_err(|e| name_taken(e, "notification_rules_team_id_name_key", "rule"))?;
	set_rule_channels(&mut tx, id, &input.channel_ids).await?;
	tx.commit().await?;
	note(
		db,
		user,
		team,
		"notification_rule.create",
		Target::new("notification_rule", id.0).named(&name),
		Diff::new()
			.set("kinds", kinds.clone())
			.set("channels", input.channel_ids.len()),
	)
	.await;
	Ok(id)
}

async fn set_rule_channels(
	tx: &mut sqlx::PgConnection,
	rule: NotificationRuleId,
	channels: &[i64],
) -> Result<(), sqlx::Error> {
	sqlx::query("DELETE FROM notification_rule_channels WHERE rule_id = $1")
		.bind(rule)
		.execute(&mut *tx)
		.await?;
	sqlx::query(
		"INSERT INTO notification_rule_channels (rule_id, channel_id)
		SELECT $1, c FROM UNNEST($2::bigint[]) AS c ON CONFLICT DO NOTHING",
	)
	.bind(rule)
	.bind(channels)
	.execute(&mut *tx)
	.await?;
	Ok(())
}

/// Replaces a rule. Needs `update notification`.
pub async fn update_rule(
	db: &PgPool,
	user: UserId,
	team: TeamId,
	id: NotificationRuleId,
	input: RuleInput,
) -> AppResult<()> {
	authorize(db, user, team, Action::Update, Resource::Notification).await?;
	rule_in(db, team, id).await?;
	let (name, kinds) = check_rule(db, team, &input).await?;
	let mut tx = db.begin().await?;
	sqlx::query(
		"UPDATE notification_rules SET name = $3, kinds = $4, project_id = $5, environment_id = $6,
			min_severity = $7, enabled = $8 WHERE id = $1 AND team_id = $2",
	)
	.bind(id)
	.bind(team)
	.bind(&name)
	.bind(&kinds)
	.bind(input.project_id)
	.bind(input.environment_id)
	.bind(input.min_severity.rank())
	.bind(input.enabled)
	.execute(&mut *tx)
	.await
	.map_err(|e| name_taken(e, "notification_rules_team_id_name_key", "rule"))?;
	set_rule_channels(&mut tx, id, &input.channel_ids).await?;
	tx.commit().await?;
	note(
		db,
		user,
		team,
		"notification_rule.update",
		Target::new("notification_rule", id.0).named(&name),
		Diff::new()
			.set("kinds", kinds.clone())
			.set("channels", input.channel_ids.len()),
	)
	.await;
	Ok(())
}

async fn rule_in(db: &PgPool, team: TeamId, id: NotificationRuleId) -> AppResult<Rule> {
	sqlx::query_as::<_, Rule>(sqlx::AssertSqlSafe(format!(
		"{RULE_SELECT} WHERE r.id = $1 AND r.team_id = $2"
	)))
	.bind(id)
	.bind(team)
	.fetch_optional(db)
	.await?
	.ok_or_else(|| not_found("rule"))
}

/// Switches a rule on or off. Needs `update notification`.
pub async fn set_rule_enabled(
	db: &PgPool,
	user: UserId,
	team: TeamId,
	id: NotificationRuleId,
	enabled: bool,
) -> AppResult<()> {
	authorize(db, user, team, Action::Update, Resource::Notification).await?;
	let done =
		sqlx::query("UPDATE notification_rules SET enabled = $3 WHERE id = $1 AND team_id = $2")
			.bind(id)
			.bind(team)
			.bind(enabled)
			.execute(db)
			.await?;
	if done.rows_affected() == 0 {
		return Err(not_found("rule"));
	}
	note(
		db,
		user,
		team,
		"notification_rule.update",
		Target::new("notification_rule", id.0),
		Diff::new().set("enabled", enabled),
	)
	.await;
	Ok(())
}

/// Deletes a rule. Needs `delete notification`.
pub async fn remove_rule(
	db: &PgPool,
	user: UserId,
	team: TeamId,
	id: NotificationRuleId,
) -> AppResult<()> {
	authorize(db, user, team, Action::Delete, Resource::Notification).await?;
	let done = sqlx::query("DELETE FROM notification_rules WHERE id = $1 AND team_id = $2")
		.bind(id)
		.bind(team)
		.execute(db)
		.await?;
	if done.rows_affected() == 0 {
		return Err(not_found("rule"));
	}
	note(
		db,
		user,
		team,
		"notification_rule.delete",
		Target::new("notification_rule", id.0),
		Diff::new(),
	)
	.await;
	Ok(())
}

// --- metric alerts --------------------------------------------------------------------------

/// The team's metric alerts. Needs `read notification`.
pub async fn alerts(db: &PgPool, user: UserId, team: TeamId) -> AppResult<Vec<MetricAlert>> {
	authorize(db, user, team, Action::Read, Resource::Notification).await?;
	Ok(
		sqlx::query_as::<_, MetricAlert>(sqlx::AssertSqlSafe(format!(
			"SELECT {ALERT_COLUMNS} FROM metric_alerts WHERE team_id = $1 ORDER BY name"
		)))
		.bind(team)
		.fetch_all(db)
		.await?,
	)
}

#[derive(Debug, Clone)]
pub struct AlertInput {
	pub name: String,
	/// `server` or `resource`.
	pub scope: String,
	pub server_id: Option<i64>,
	pub resource_kind: Option<String>,
	pub resource_id: Option<i64>,
	pub metric: String,
	pub threshold: f64,
	pub duration_minutes: i32,
	pub severity: Severity,
	pub enabled: bool,
}

/// Adds a metric alert. Needs `create notification`.
pub async fn create_alert(
	db: &PgPool,
	user: UserId,
	team: TeamId,
	input: AlertInput,
) -> AppResult<MetricAlertId> {
	authorize(db, user, team, Action::Create, Resource::Notification).await?;
	let name = name_of(&input.name)?;
	let metric = Metric::parse(&input.metric).ok_or_else(|| invalid("metric"))?;
	if !input.threshold.is_finite() || input.threshold <= 0.0 {
		return Err(invalid("threshold"));
	}
	if !(1..=24 * 60).contains(&input.duration_minutes) {
		return Err(invalid("duration"));
	}
	let (server, kind, resource) = match input.scope.as_str() {
		"server" => {
			if !metric.for_server() || input.threshold > 100.0 {
				return Err(invalid("metric or threshold for a server"));
			}
			let id = input.server_id.ok_or_else(|| invalid("server"))?;
			scope::server(db, team, ServerId(id)).await?;
			(Some(id), None, None)
		}
		"resource" => {
			let kind = input
				.resource_kind
				.clone()
				.ok_or_else(|| invalid("resource"))?;
			let id = input.resource_id.ok_or_else(|| invalid("resource"))?;
			if !metric.for_resource(&kind) || (metric.is_percent() && input.threshold > 100.0) {
				return Err(invalid("metric or threshold for a resource"));
			}
			let target = if matches!(
				kind.as_str(),
				"application" | "database" | "model" | "stack"
			) {
				hooks::by_kind(db, &kind, id).await
			} else {
				None
			};
			// A resource of another team does not exist.
			if target.is_none_or(|t| t.team != team) {
				return Err(ServerError::NotFound);
			}
			(None, Some(kind), Some(id))
		}
		_ => return Err(invalid("scope")),
	};
	let id = MetricAlertId::generate();
	sqlx::query(
		"INSERT INTO metric_alerts (id, team_id, name, scope, server_id, resource_kind, resource_id,
			metric, threshold, duration_minutes, severity, enabled)
		VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
	)
	.bind(id)
	.bind(team)
	.bind(&name)
	.bind(&input.scope)
	.bind(server)
	.bind(kind)
	.bind(resource)
	.bind(metric.as_str())
	.bind(input.threshold)
	.bind(input.duration_minutes)
	.bind(input.severity.rank())
	.bind(input.enabled)
	.execute(db)
	.await
	.map_err(|e| name_taken(e, "metric_alerts_team_id_name_key", "metric alert"))?;
	note(
		db,
		user,
		team,
		"metric_alert.create",
		Target::new("metric_alert", id.0).named(&name),
		Diff::new()
			.set("metric", metric.as_str())
			.set("threshold", input.threshold)
			.set("duration_minutes", i64::from(input.duration_minutes)),
	)
	.await;
	Ok(id)
}

/// Switches an alert on or off (it forgets a firing state either way). Needs
/// `update notification`.
pub async fn set_alert_enabled(
	db: &PgPool,
	user: UserId,
	team: TeamId,
	id: MetricAlertId,
	enabled: bool,
) -> AppResult<()> {
	authorize(db, user, team, Action::Update, Resource::Notification).await?;
	let done = sqlx::query(
		"UPDATE metric_alerts SET enabled = $3, state = 'ok', breach_since = NULL
		WHERE id = $1 AND team_id = $2",
	)
	.bind(id)
	.bind(team)
	.bind(enabled)
	.execute(db)
	.await?;
	if done.rows_affected() == 0 {
		return Err(not_found("metric alert"));
	}
	note(
		db,
		user,
		team,
		"metric_alert.update",
		Target::new("metric_alert", id.0),
		Diff::new().set("enabled", enabled),
	)
	.await;
	Ok(())
}

/// Deletes an alert. Needs `delete notification`.
pub async fn remove_alert(
	db: &PgPool,
	user: UserId,
	team: TeamId,
	id: MetricAlertId,
) -> AppResult<()> {
	authorize(db, user, team, Action::Delete, Resource::Notification).await?;
	let done = sqlx::query("DELETE FROM metric_alerts WHERE id = $1 AND team_id = $2")
		.bind(id)
		.bind(team)
		.execute(db)
		.await?;
	if done.rows_affected() == 0 {
		return Err(not_found("metric alert"));
	}
	note(
		db,
		user,
		team,
		"metric_alert.delete",
		Target::new("metric_alert", id.0),
		Diff::new(),
	)
	.await;
	Ok(())
}

/// What an alert can watch in this team, for the form: servers and resources, by name.
#[derive(Debug, Clone, Default)]
pub struct AlertTargets {
	pub servers: Vec<(i64, String)>,
	/// `(kind, id, name)` with kind one of application, database, model, stack.
	pub resources: Vec<(String, i64, String)>,
}

/// Needs `read notification`.
pub async fn alert_targets(db: &PgPool, user: UserId, team: TeamId) -> AppResult<AlertTargets> {
	authorize(db, user, team, Action::Read, Resource::Notification).await?;
	let servers = sqlx::query_as::<_, (i64, String)>(
		"SELECT id, name FROM servers WHERE team_id = $1 ORDER BY name",
	)
	.bind(team)
	.fetch_all(db)
	.await?;
	let resources = sqlx::query_as::<_, (String, i64, String)>(
		"SELECT 'application', a.id, a.name FROM applications a JOIN projects p ON p.id = a.project_id WHERE p.team_id = $1
		UNION ALL SELECT 'database', d.id, d.name FROM databases d JOIN projects p ON p.id = d.project_id WHERE p.team_id = $1
		UNION ALL SELECT 'model', m.id, m.name FROM models m JOIN projects p ON p.id = m.project_id WHERE p.team_id = $1
		UNION ALL SELECT 'stack', s.id, s.name FROM stacks s JOIN projects p ON p.id = s.project_id WHERE p.team_id = $1
		ORDER BY 3, 1",
	)
	.bind(team)
	.fetch_all(db)
	.await?;
	Ok(AlertTargets { servers, resources })
}

/// A project and its environments, for the rule form's scope picker.
pub type ProjectScope = (ProjectId, String, Vec<(EnvironmentId, String)>);

/// Every project of the team with its environments. Needs `read notification`.
pub async fn scopes(db: &PgPool, user: UserId, team: TeamId) -> AppResult<Vec<ProjectScope>> {
	authorize(db, user, team, Action::Read, Resource::Notification).await?;
	let projects = sqlx::query_as::<_, (ProjectId, String)>(
		"SELECT id, name FROM projects WHERE team_id = $1 ORDER BY name",
	)
	.bind(team)
	.fetch_all(db)
	.await?;
	let envs = sqlx::query_as::<_, (ProjectId, EnvironmentId, String)>(
		"SELECT e.project_id, e.id, e.name FROM environments e
		JOIN projects p ON p.id = e.project_id WHERE p.team_id = $1 ORDER BY e.position, e.name",
	)
	.bind(team)
	.fetch_all(db)
	.await?;
	Ok(projects
		.into_iter()
		.map(|(id, name)| {
			let own = envs
				.iter()
				.filter(|(p, _, _)| *p == id)
				.map(|(_, e, n)| (*e, n.clone()))
				.collect();
			(id, name, own)
		})
		.collect())
}

// --- delivery log ---------------------------------------------------------------------------

/// A line of the delivery log.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct DeliveryRow {
	pub id: NotificationDeliveryId,
	pub channel_name: String,
	pub title: Option<String>,
	pub status: String,
	pub attempts: i32,
	pub last_error: Option<String>,
	pub created_at: OffsetDateTime,
	pub sent_at: Option<OffsetDateTime>,
}

const DELIVERY_SELECT: &str = "SELECT d.id, c.name AS channel_name, d.message->>'title' AS title,
	d.status, d.attempts, d.last_error, d.created_at, d.sent_at
	FROM notification_deliveries d JOIN notification_channels c ON c.id = d.channel_id";

/// The newest deliveries and the dead letters (those that ran out of retries), newest first.
/// Needs `read notification`.
pub async fn delivery_log(
	db: &PgPool,
	user: UserId,
	team: TeamId,
) -> AppResult<(Vec<DeliveryRow>, Vec<DeliveryRow>)> {
	authorize(db, user, team, Action::Read, Resource::Notification).await?;
	let recent = sqlx::query_as::<_, DeliveryRow>(sqlx::AssertSqlSafe(format!(
		"{DELIVERY_SELECT} WHERE d.team_id = $1 AND d.status <> 'digested'
		ORDER BY d.id DESC LIMIT 30"
	)))
	.bind(team)
	.fetch_all(db)
	.await?;
	let dead = sqlx::query_as::<_, DeliveryRow>(sqlx::AssertSqlSafe(format!(
		"{DELIVERY_SELECT} WHERE d.team_id = $1 AND d.status = 'dead' ORDER BY d.id DESC LIMIT 50"
	)))
	.bind(team)
	.fetch_all(db)
	.await?;
	Ok((recent, dead))
}

/// Puts a dead letter back in the queue with a fresh set of retries. Needs
/// `update notification`.
pub async fn retry_delivery(
	s: &AppState,
	user: UserId,
	team: TeamId,
	id: NotificationDeliveryId,
) -> AppResult<()> {
	authorize(&s.db, user, team, Action::Update, Resource::Notification).await?;
	let done = sqlx::query(
		"UPDATE notification_deliveries
		SET status = 'queued', attempts = 0, last_error = NULL, next_attempt_at = NOW()
		WHERE id = $1 AND team_id = $2 AND status = 'dead'",
	)
	.bind(id)
	.bind(team)
	.execute(&s.db)
	.await?;
	if done.rows_affected() == 0 {
		return Err(not_found("delivery"));
	}
	s.jobs.notify.notify_one();
	Ok(())
}

// --- the bell -------------------------------------------------------------------------------

/// One line of a person's notification history.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct FeedItem {
	pub id: NotificationEventId,
	pub kind: String,
	pub severity: i16,
	pub title: String,
	pub body: String,
	pub link: Option<String>,
	pub count: i32,
	pub last_at: OffsetDateTime,
	pub unread: bool,
}

async fn member(db: &PgPool, user: UserId, team: TeamId) -> AppResult<()> {
	if permission::granted(db, user, team).await?.is_member() {
		Ok(())
	} else {
		Err(ServerError::NotFound)
	}
}

/// The team's newest events with what `user` has not read yet marked. Any member.
pub async fn feed(db: &PgPool, user: UserId, team: TeamId, limit: i64) -> AppResult<Vec<FeedItem>> {
	member(db, user, team).await?;
	Ok(sqlx::query_as::<_, FeedItem>(
		"SELECT e.id, e.kind, e.severity, e.title, e.body, e.link, e.count, e.last_at,
			(e.id > COALESCE(us.read_through, 0) AND r.event_id IS NULL) AS unread
		FROM notification_events e
		LEFT JOIN notification_user_state us ON us.user_id = $2 AND us.team_id = e.team_id
		LEFT JOIN notification_reads r ON r.user_id = $2 AND r.event_id = e.id
		WHERE e.team_id = $1 ORDER BY e.id DESC LIMIT $3",
	)
	.bind(team)
	.bind(user)
	.bind(limit.clamp(1, 200))
	.fetch_all(db)
	.await?)
}

/// How many events `user` has not read in `team`; zero for someone outside it.
pub async fn unread_count(db: &PgPool, user: UserId, team: TeamId) -> Result<i64, sqlx::Error> {
	sqlx::query_scalar(
		"SELECT COUNT(*) FROM notification_events e
		LEFT JOIN notification_user_state us ON us.user_id = $2 AND us.team_id = e.team_id
		LEFT JOIN notification_reads r ON r.user_id = $2 AND r.event_id = e.id
		WHERE e.team_id = $1 AND e.id > COALESCE(us.read_through, 0) AND r.event_id IS NULL
			AND EXISTS (SELECT 1 FROM team_members m WHERE m.team_id = $1 AND m.user_id = $2)",
	)
	.bind(team)
	.bind(user)
	.fetch_one(db)
	.await
}

/// Marks one event read for `user`. An event of another team does not exist.
pub async fn mark_read(
	db: &PgPool,
	user: UserId,
	team: TeamId,
	event: NotificationEventId,
) -> AppResult<()> {
	member(db, user, team).await?;
	let inserted = sqlx::query(
		"INSERT INTO notification_reads (user_id, event_id)
		SELECT $1, id FROM notification_events WHERE id = $2 AND team_id = $3
		ON CONFLICT DO NOTHING",
	)
	.bind(user)
	.bind(event)
	.bind(team)
	.execute(db)
	.await?;
	if inserted.rows_affected() == 0 {
		let exists = sqlx::query_scalar::<_, bool>(
			"SELECT EXISTS (SELECT 1 FROM notification_events WHERE id = $1 AND team_id = $2)",
		)
		.bind(event)
		.bind(team)
		.fetch_one(db)
		.await?;
		if !exists {
			return Err(not_found("notification"));
		}
	}
	Ok(())
}

/// Marks everything up to now read for `user`.
pub async fn mark_all_read(db: &PgPool, user: UserId, team: TeamId) -> AppResult<()> {
	member(db, user, team).await?;
	sqlx::query(
		"INSERT INTO notification_user_state (user_id, team_id, read_through)
		VALUES ($1, $2, COALESCE((SELECT MAX(id) FROM notification_events WHERE team_id = $2), 0))
		ON CONFLICT (user_id, team_id) DO UPDATE SET read_through = EXCLUDED.read_through",
	)
	.bind(user)
	.bind(team)
	.execute(db)
	.await?;
	Ok(())
}
