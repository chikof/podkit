//! The terminal's two entry points: the WebSocket `/ws/terminal/{resource}` the page opens,
//! and a small JSON API (`/api/v1/teams/{team}/terminal`) for the transcript option and for
//! reading recorded sessions back.
//!
//! The WebSocket is a plain `GET`, so the UI's origin guard (which only looks at unsafe
//! methods) does not see it, and a browser sends cookies to it from any site. Hence the
//! explicit same-origin check here, before anything else.

use axum::Json;
use axum::Router;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio_tungstenite::tungstenite::Message as Agent;

use super::pump::{self, Frame};
use super::service::{self, Caller, Subject};
use crate::auth::{AuthUser, SessionToken, session, tokens};
use crate::error::{AppResult, ServerError};
use crate::ids::{ProjectId, TeamId, TerminalSessionId};
use crate::state::AppState;

/// `/ws/terminal/{resource}`, mounted at the root next to the UI.
pub fn ws_router() -> Router<AppState> {
	Router::new().route("/ws/terminal/{resource}", get(open))
}

/// `/api/v1/teams/{team_id}/terminal`.
pub fn router() -> Router<AppState> {
	Router::new()
		.route("/recording", get(get_recording).put(put_recording))
		.route("/sessions", get(list_sessions))
		.route("/sessions/{id}/transcript", get(get_transcript))
}

#[derive(Debug, Deserialize)]
struct OpenQuery {
	team: Option<TeamId>,
	project: Option<ProjectId>,
	cols: Option<u16>,
	rows: Option<u16>,
}

/// Who is on the other end: a session cookie, or an API token (which needs `root`, checked
/// by [`service::prepare`]).
async fn caller(s: &AppState, token: AppResult<SessionToken>) -> AppResult<Caller> {
	let SessionToken(token) = token.map_err(|_| ServerError::InvalidToken)?;
	if token.starts_with(tokens::PREFIX) {
		let grant = tokens::service::resolve(&s.db, &token).await?;
		return Ok(Caller {
			user: grant.user,
			grant: Some(grant),
		});
	}
	let user = session::resolve(&s.db, &token)
		.await?
		.ok_or(ServerError::InvalidToken)?;
	Ok(Caller { user, grant: None })
}

async fn open(
	State(s): State<AppState>,
	headers: HeaderMap,
	Path(resource): Path<String>,
	Query(q): Query<OpenQuery>,
	token: AppResult<SessionToken>,
	ws: Result<WebSocketUpgrade, axum::extract::ws::rejection::WebSocketUpgradeRejection>,
) -> Response {
	if !crate::auth::origin::same_origin(&headers) {
		return (StatusCode::FORBIDDEN, "cross-origin request refused").into_response();
	}
	let result = async {
		let caller = caller(&s, token).await?;
		// A path nobody could have a terminal at is the same 404 as one that is not theirs.
		let subject = Subject::parse(&resource).ok_or(ServerError::NotFound)?;
		let (Some(team), Some(project)) = (q.team, q.project) else {
			return Err(ServerError::NotFound);
		};
		let prepared = service::prepare(&s, &caller, team, project, &subject).await?;
		Ok(prepared)
	}
	.await;
	let prepared = match result {
		Ok(p) => p,
		Err(e) => return e.into_response(),
	};
	let ws = match ws {
		Ok(ws) => ws,
		Err(rejection) => return rejection.into_response(),
	};
	let (cols, rows) = (q.cols.unwrap_or(80), q.rows.unwrap_or(24));
	let (session, agent) = match service::open(&s, prepared, cols, rows).await {
		Ok(opened) => opened,
		Err(e) => return e.into_response(),
	};
	ws.on_upgrade(move |socket| async move {
		let options = session.options();
		let outcome = run(socket, agent.socket, options).await;
		service::finish(&s, &session, &outcome).await;
	})
}

/// Joins the browser's socket and the agent's through [`pump::pump`].
async fn run(
	browser: WebSocket,
	agent: tokio_tungstenite::WebSocketStream<tokio::net::UnixStream>,
	options: pump::Options,
) -> pump::Outcome {
	let (browser_tx, browser_rx) = browser.split();
	let (agent_tx, agent_rx) = agent.split();

	let browser_rx = browser_rx
		.filter_map(|m| async move {
			match m {
				Ok(Message::Binary(b)) => Some(Frame::Binary(b)),
				Ok(Message::Text(t)) => Some(Frame::Text(t.as_str().to_owned())),
				Ok(Message::Close(_)) | Err(_) => Some(Frame::Close),
				Ok(Message::Ping(_) | Message::Pong(_)) => None,
			}
		})
		.boxed();
	let browser_tx = Box::pin(browser_tx.sink_map_err(|_| ()).with(|f: Frame| async move {
		Ok::<_, ()>(match f {
			Frame::Binary(b) => Message::Binary(b),
			Frame::Text(t) => Message::Text(t.into()),
			Frame::Close => Message::Close(None),
		})
	}));
	let agent_rx = agent_rx
		.filter_map(|m| async move {
			match m {
				Ok(Agent::Binary(b)) => Some(Frame::Binary(b)),
				Ok(Agent::Text(t)) => Some(Frame::Text(t.as_str().to_owned())),
				Ok(Agent::Close(_)) | Err(_) => Some(Frame::Close),
				Ok(Agent::Ping(_) | Agent::Pong(_) | Agent::Frame(_)) => None,
			}
		})
		.boxed();
	let agent_tx = Box::pin(agent_tx.sink_map_err(|_| ()).with(|f: Frame| async move {
		Ok::<_, ()>(match f {
			Frame::Binary(b) => Agent::Binary(b),
			Frame::Text(t) => Agent::Text(t.into()),
			Frame::Close => Agent::Close(None),
		})
	}));
	pump::pump(browser_rx, browser_tx, agent_rx, agent_tx, options).await
}

// --- JSON API ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize)]
struct Recording {
	enabled: bool,
}

async fn get_recording(
	State(s): State<AppState>,
	user: AuthUser,
	Path(team): Path<TeamId>,
) -> AppResult<Json<Recording>> {
	let enabled = service::recording(&s.db, user.id, team).await?;
	Ok(Json(Recording { enabled }))
}

/// Turns the team's transcript option on or off. Needs `update team`.
async fn put_recording(
	State(s): State<AppState>,
	user: AuthUser,
	Path(team): Path<TeamId>,
	Json(body): Json<Recording>,
) -> AppResult<Json<Recording>> {
	service::set_recording(&s, user.id, team, body.enabled).await?;
	Ok(Json(body))
}

#[derive(Serialize)]
struct SessionResponse {
	id: TerminalSessionId,
	user_id: crate::ids::UserId,
	target_kind: String,
	target_id: i64,
	service: Option<String>,
	/// Unix seconds.
	started_at: i64,
	ended_at: Option<i64>,
	/// `closed`, `exited`, `idle` or `error`; empty while the session is open.
	end_reason: Option<String>,
	has_transcript: bool,
}

/// The team's latest terminal sessions. Needs `read audit`.
async fn list_sessions(
	State(s): State<AppState>,
	user: AuthUser,
	Path(team): Path<TeamId>,
) -> AppResult<Json<Vec<SessionResponse>>> {
	let rows = service::sessions(&s.db, user.id, team, 100).await?;
	Ok(Json(
		rows.into_iter()
			.map(|r| SessionResponse {
				id: r.id,
				user_id: r.user_id,
				target_kind: r.target_kind,
				target_id: r.target_id,
				service: r.service,
				started_at: r.started_at.unix_timestamp(),
				ended_at: r.ended_at.map(time::OffsetDateTime::unix_timestamp),
				end_reason: r.end_reason,
				has_transcript: r.has_transcript,
			})
			.collect(),
	))
}

/// What the terminal printed in one session. Needs `read audit`; a token needs
/// `read:sensitive`.
async fn get_transcript(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, id)): Path<(TeamId, TerminalSessionId)>,
) -> AppResult<Response> {
	let caller = Caller {
		user: user.id,
		grant: user.grant,
	};
	let bytes = service::transcript(&s, &caller, team, id).await?;
	Ok((
		[
			(header::CONTENT_TYPE, "text/plain; charset=utf-8"),
			(header::CACHE_CONTROL, "no-store"),
			(header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
		],
		bytes,
	)
		.into_response())
}
