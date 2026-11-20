//! From an [`Event`] to queued deliveries: flap detection, rule matching, dedup and quiet
//! hours. Nothing here talks to the network; [`super::delivery`] sends what this queues.

use sqlx::PgPool;
use time::{Duration, OffsetDateTime};

use super::channel::{CHANNEL_COLUMNS, Channel};
use super::event::{Event, EventKind, Severity};
use super::format::Message;
use super::settings::{self, Settings};
use crate::ids::{EnvironmentId, NotificationDeliveryId, NotificationEventId, ProjectId, TeamId};

/// A state change this often inside [`FLAP_WINDOW`] makes an up/down pair "flapping".
pub const FLAP_CHANGES: i32 = 4;
/// The window state changes are counted in, and the quiet time that ends flapping.
pub const FLAP_WINDOW: Duration = Duration::minutes(30);

/// What happened to an event on its way in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
	/// No rule wants it and it is too minor for the bell.
	Ignored,
	/// An up/down pair that is changing state too often; muted until it settles.
	Muted,
	/// The same event key was seen inside the dedup window; only its count went up.
	Collapsed(NotificationEventId),
	/// Stored, with this many deliveries queued (or deferred to the digest).
	Recorded {
		event: NotificationEventId,
		deliveries: usize,
	},
}

#[derive(Debug, PartialEq, Eq)]
enum Flap {
	Normal,
	Started,
	Muted,
}

/// Counts a state change of an up/down pair and says whether it is flapping.
async fn observe_flap(
	db: &PgPool,
	team: TeamId,
	key: &str,
	now: OffsetDateTime,
) -> Result<Flap, sqlx::Error> {
	let row = sqlx::query_as::<_, (OffsetDateTime, OffsetDateTime, i32, bool)>(
		"SELECT window_start, last_change, changes, flapping FROM notification_flaps
		WHERE team_id = $1 AND flap_key = $2",
	)
	.bind(team)
	.bind(key)
	.fetch_optional(db)
	.await?;
	let (window_start, changes, flapping, verdict) = match row {
		None => (now, 1, false, Flap::Normal),
		Some((_, last, changes, true)) => {
			if now - last > FLAP_WINDOW {
				(now, 1, false, Flap::Normal)
			} else {
				(now, changes + 1, true, Flap::Muted)
			}
		}
		Some((start, _, changes, false)) => {
			if now - start > FLAP_WINDOW {
				(now, 1, false, Flap::Normal)
			} else if changes + 1 >= FLAP_CHANGES {
				(start, changes + 1, true, Flap::Started)
			} else {
				(start, changes + 1, false, Flap::Normal)
			}
		}
	};
	sqlx::query(
		"INSERT INTO notification_flaps (team_id, flap_key, window_start, last_change, changes, flapping)
		VALUES ($1, $2, $3, $4, $5, $6)
		ON CONFLICT (team_id, flap_key) DO UPDATE
			SET window_start = $3, last_change = $4, changes = $5, flapping = $6",
	)
	.bind(team)
	.bind(key)
	.bind(window_start)
	.bind(now)
	.bind(changes)
	.bind(flapping)
	.execute(db)
	.await?;
	Ok(verdict)
}

/// The enabled channels a rule of the team sends this kind of event to: kind listed,
/// severity at or above the rule's minimum, project and environment matching its scope.
pub async fn matching_channels(
	db: &PgPool,
	team: TeamId,
	kind: EventKind,
	severity: Severity,
	project: Option<ProjectId>,
	environment: Option<EnvironmentId>,
) -> Result<Vec<Channel>, sqlx::Error> {
	let columns = CHANNEL_COLUMNS
		.split(", ")
		.map(|c| format!("c.{}", c.trim()))
		.collect::<Vec<_>>()
		.join(", ");
	sqlx::query_as::<_, Channel>(sqlx::AssertSqlSafe(format!(
		"SELECT DISTINCT {columns} FROM notification_rules r
		JOIN notification_rule_channels rc ON rc.rule_id = r.id
		JOIN notification_channels c ON c.id = rc.channel_id
		WHERE r.team_id = $1 AND r.enabled AND c.enabled AND c.team_id = $1
			AND $2 = ANY (r.kinds) AND r.min_severity <= $3
			AND (r.project_id IS NULL OR r.project_id = $4)
			AND (r.environment_id IS NULL OR r.environment_id = $5)
		ORDER BY c.id"
	)))
	.bind(team)
	.bind(kind.as_str())
	.bind(severity.rank())
	.bind(project)
	.bind(environment)
	.fetch_all(db)
	.await
}

/// Queues `msg` for each of `channels`; during quiet hours a message below critical waits for
/// the digest instead. Returns how many rows were queued.
pub async fn enqueue(
	db: &PgPool,
	team: TeamId,
	channels: &[Channel],
	event: Option<NotificationEventId>,
	msg: &Message,
	settings: &Settings,
	now: OffsetDateTime,
) -> Result<usize, sqlx::Error> {
	let defer = !msg.test && msg.severity < Severity::Critical && settings.is_quiet(now);
	let status = if defer { "deferred" } else { "queued" };
	let json = serde_json::to_value(msg).unwrap_or_default();
	for channel in channels {
		sqlx::query(
			"INSERT INTO notification_deliveries
				(id, team_id, channel_id, event_id, message, status, next_attempt_at, created_at)
			VALUES ($1, $2, $3, $4, $5, $6, $7, $7)",
		)
		.bind(NotificationDeliveryId::generate())
		.bind(team)
		.bind(channel.id)
		.bind(event)
		.bind(&json)
		.bind(status)
		.bind(now)
		.execute(db)
		.await?;
	}
	Ok(channels.len())
}

fn message_of(event: &Event, id: Option<NotificationEventId>, now: OffsetDateTime) -> Message {
	Message {
		kind: event.kind.as_str().to_owned(),
		severity: event.severity,
		title: event.title.clone(),
		body: event.body.clone(),
		link: event.link.clone(),
		count: 1,
		at: now
			.format(&time::format_description::well_known::Rfc3339)
			.unwrap_or_default(),
		team_id: event.team.0,
		project_id: event.project.map(|p| p.0),
		environment_id: event.environment.map(|e| e.0),
		event_id: id.map(|i| i.0),
		digest: Vec::new(),
		test: false,
	}
}

/// Routes one event. See [`Outcome`].
pub async fn route(
	db: &PgPool,
	mut event: Event,
	now: OffsetDateTime,
) -> Result<Outcome, sqlx::Error> {
	let settings = settings::load(db, event.team).await?;
	let mut key = event.key();

	if let Some(flap_key) = event.flap_key() {
		match observe_flap(db, event.team, &flap_key, now).await? {
			Flap::Normal => {}
			Flap::Muted => return Ok(Outcome::Muted),
			Flap::Started => {
				event.title = format!("Flapping: {}", event.title);
				let note = format!(
					"{} Changed state {FLAP_CHANGES} times in {} minutes; further changes stay quiet until it settles.",
					event.body,
					FLAP_WINDOW.whole_minutes()
				);
				note.trim().clone_into(&mut event.body);
				key.push_str(":flapping");
			}
		}
	}

	let channels = matching_channels(
		db,
		event.team,
		event.kind,
		event.severity,
		event.project,
		event.environment,
	)
	.await?;
	if channels.is_empty() && event.severity < Severity::Warning {
		return Ok(Outcome::Ignored);
	}

	// The same key inside the window only counts; the summary goes out when it ends.
	let window = now - Duration::minutes(i64::from(settings.dedup_minutes));
	let collapsed = sqlx::query_scalar::<_, NotificationEventId>(
		"UPDATE notification_events SET count = count + 1, last_at = $3
		WHERE id = (SELECT id FROM notification_events
			WHERE team_id = $1 AND event_key = $2 AND last_at > $4
			ORDER BY id DESC LIMIT 1)
		RETURNING id",
	)
	.bind(event.team)
	.bind(&key)
	.bind(now)
	.bind(window)
	.fetch_optional(db)
	.await?;
	if let Some(id) = collapsed {
		return Ok(Outcome::Collapsed(id));
	}

	let id = NotificationEventId::generate();
	sqlx::query(
		"INSERT INTO notification_events
			(id, team_id, kind, severity, event_key, title, body, link, project_id,
				environment_id, created_at, last_at)
		VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $11)",
	)
	.bind(id)
	.bind(event.team)
	.bind(event.kind.as_str())
	.bind(event.severity.rank())
	.bind(&key)
	.bind(super::event::clip(&event.title, 255))
	.bind(&event.body)
	.bind(&event.link)
	.bind(event.project)
	.bind(event.environment)
	.bind(now)
	.execute(db)
	.await?;

	let msg = message_of(&event, Some(id), now);
	let deliveries = enqueue(db, event.team, &channels, Some(id), &msg, &settings, now).await?;
	Ok(Outcome::Recorded {
		event: id,
		deliveries,
	})
}

/// Sends a "happened N times" follow-up for every event whose dedup window has ended with
/// repeats the channels never heard about. Returns how many follow-ups were queued.
pub async fn summarize_repeats(db: &PgPool, now: OffsetDateTime) -> Result<usize, sqlx::Error> {
	let rows = sqlx::query_as::<
		_,
		(
			NotificationEventId,
			TeamId,
			String,
			i16,
			String,
			String,
			Option<String>,
			Option<ProjectId>,
			Option<EnvironmentId>,
			i32,
			i32,
		),
	>(
		"SELECT e.id, e.team_id, e.kind, e.severity, e.title, e.body, e.link, e.project_id,
			e.environment_id, e.count, COALESCE(s.dedup_minutes, $2)
		FROM notification_events e
		LEFT JOIN notification_settings s ON s.team_id = e.team_id
		WHERE e.count > e.notified_count
			AND e.last_at < $1 - make_interval(mins => COALESCE(s.dedup_minutes, $2))
		LIMIT 100",
	)
	.bind(now)
	.bind(settings::DEFAULT_DEDUP_MINUTES)
	.fetch_all(db)
	.await?;
	let mut queued = 0;
	for (id, team, kind, severity, title, body, link, project, environment, count, _) in rows {
		sqlx::query("UPDATE notification_events SET notified_count = count WHERE id = $1")
			.bind(id)
			.execute(db)
			.await?;
		let Some(kind) = EventKind::parse(&kind) else {
			continue;
		};
		let severity = Severity::from_rank(severity);
		let channels = matching_channels(db, team, kind, severity, project, environment).await?;
		let settings = settings::load(db, team).await?;
		let mut msg = message_of(
			&Event::new(team, kind, "", title)
				.body(body)
				.severity(severity),
			Some(id),
			now,
		);
		msg.link = link;
		msg.project_id = project.map(|p| p.0);
		msg.environment_id = environment.map(|e| e.0);
		msg.count = count;
		let note = format!(
			"Happened {count} times; only the first was sent right away.\n{}",
			msg.body
		);
		note.trim().clone_into(&mut msg.body);
		queued += enqueue(db, team, &channels, Some(id), &msg, &settings, now).await?;
	}
	Ok(queued)
}
