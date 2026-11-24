use axum::Json;
use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::header;
use axum::response::IntoResponse;
use axum::routing::get;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use super::service::{self, Filter, MAX_LIMIT, PAGE};
use super::{ActorKind, Event};
use crate::auth::AuthUser;
use crate::error::{AppResult, ServerError};
use crate::ids::{ApiTokenId, AuditEventId, ProjectId, TeamId, UserId};
use crate::state::AppState;

pub fn router() -> Router<AppState> {
	Router::new()
		.route("/", get(list))
		.route("/export", get(export))
}

/// Every filter is optional. Times are RFC 3339 (`2026-11-20T08:00:00Z`).
#[derive(Debug, Default, Deserialize)]
pub struct AuditQuery {
	pub actor: Option<UserId>,
	pub actor_kind: Option<String>,
	/// An action, or the start of one: `app.` is every application event.
	pub action: Option<String>,
	pub target_kind: Option<String>,
	pub target_id: Option<i64>,
	pub since: Option<String>,
	pub until: Option<String>,
	/// The `next_before` of the previous page.
	pub before: Option<AuditEventId>,
	pub limit: Option<i64>,
}

fn blank(v: Option<String>) -> Option<String> {
	v.map(|s| s.trim().to_owned()).filter(|s| !s.is_empty())
}

fn time(raw: Option<String>, name: &str) -> AppResult<Option<OffsetDateTime>> {
	blank(raw)
		.map(|s| {
			OffsetDateTime::parse(&s, &Rfc3339).map_err(|_| {
				ServerError::Validation(format!("{name} must be a time like 2026-11-20T08:00:00Z."))
			})
		})
		.transpose()
}

impl AuditQuery {
	pub fn filter(self) -> AppResult<(Filter, i64)> {
		let actor_kind = blank(self.actor_kind)
			.map(|k| {
				ActorKind::parse(&k).ok_or_else(|| {
					ServerError::Validation("actor_kind is user, token, system or mcp.".to_owned())
				})
			})
			.transpose()?;
		let filter = Filter {
			actor: self.actor,
			actor_kind,
			action: blank(self.action),
			target_kind: blank(self.target_kind),
			target_id: self.target_id,
			since: time(self.since, "since")?,
			until: time(self.until, "until")?,
			before: self.before,
		};
		Ok((filter, self.limit.unwrap_or(PAGE).clamp(1, MAX_LIMIT)))
	}
}

#[derive(Serialize)]
struct ActorResponse {
	kind: ActorKind,
	user_id: Option<UserId>,
	name: Option<String>,
	token_id: Option<ApiTokenId>,
}

#[derive(Serialize)]
struct EventResponse {
	id: AuditEventId,
	actor: ActorResponse,
	action: String,
	target_kind: String,
	target_id: Option<i64>,
	project_id: Option<ProjectId>,
	summary: String,
	/// What changed. Secrets are `•••`.
	diff: Value,
	ip: Option<String>,
	user_agent: Option<String>,
	/// Unix seconds.
	created_at: i64,
}

impl From<&Event> for EventResponse {
	fn from(e: &Event) -> Self {
		Self {
			id: e.id,
			actor: ActorResponse {
				kind: e.actor_kind,
				user_id: e.actor_user,
				name: e.actor_name.clone(),
				token_id: e.actor_token,
			},
			action: e.action.clone(),
			target_kind: e.target_kind.clone(),
			target_id: e.target_id,
			project_id: e.project,
			summary: e.summary.clone(),
			diff: e.diff.clone(),
			ip: e.ip.clone(),
			user_agent: e.user_agent.clone(),
			created_at: e.created_at.unix_timestamp(),
		}
	}
}

#[derive(Serialize)]
struct ListResponse {
	events: Vec<EventResponse>,
	/// Pass as `before` for the next page; absent on the last one.
	next_before: Option<AuditEventId>,
}

/// The team's audit events, newest first. Needs `read audit` (owners by default) and, for
/// a token, the `read` scope.
async fn list(
	State(s): State<AppState>,
	user: AuthUser,
	Path(team): Path<TeamId>,
	Query(q): Query<AuditQuery>,
) -> AppResult<Json<ListResponse>> {
	let (filter, limit) = q.filter()?;
	let mut events = service::list(&s.db, user.id, team, &filter, limit + 1).await?;
	let more = i64::try_from(events.len()).is_ok_and(|n| n > limit);
	if more {
		events.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
	}
	Ok(Json(ListResponse {
		next_before: more.then(|| events.last().map(|e| e.id)).flatten(),
		events: events.iter().map(EventResponse::from).collect(),
	}))
}

/// The same events as CSV (up to 10,000), with the same filters.
async fn export(
	State(s): State<AppState>,
	user: AuthUser,
	Path(team): Path<TeamId>,
	Query(q): Query<AuditQuery>,
) -> AppResult<impl IntoResponse> {
	let (filter, _) = q.filter()?;
	let csv = service::export(&s, user.id, team, &filter).await?;
	Ok((
		[
			(header::CONTENT_TYPE, "text/csv; charset=utf-8"),
			(
				header::CONTENT_DISPOSITION,
				"attachment; filename=\"audit-log.csv\"",
			),
		],
		csv,
	))
}
