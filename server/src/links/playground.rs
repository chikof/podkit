//! The playground: a chat with one model through its server's gateway, using
//! a key the web UI owns. The key is issued with the first message, stored
//! encrypted like an application's, opens only this model, and is never
//! handed out; its usage lands in `model_usage` like any other key's.
//!
//! The server cannot reach the gateway (rootless Podman keeps the host off the
//! `podkit` network), so the agent makes the request from a throwaway container
//! and streams what the gateway answered back (`POST /v1/gateway/chat`).

use axum::http::Method;
use futures_util::StreamExt;
use futures_util::stream::BoxStream;
use proto::{Event, GatewayChatRequest};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::auth::permission::{Action, Resource, authorize};
use crate::error::{AppResult, ServerError};
use crate::ids::{ModelId, ProjectId, TeamId, UserId};
use crate::links::{self, hash_key};
use crate::models::{Model, ModelStatus};
use crate::scope;
use crate::state::AppState;

/// Messages kept in one conversation.
pub const MAX_MESSAGES: usize = 40;
/// Longest single message, in characters.
pub const MAX_MESSAGE_CHARS: usize = 8000;
/// Tokens one answer may take.
const MAX_TOKENS: u32 = 2048;
/// Silence the gateway may keep before the chat gives up. Loading a model's
/// first answer on a CPU can take a while.
const TIMEOUT_S: u32 = 300;

/// One turn of the conversation.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, serde::Serialize)]
pub struct Message {
	/// `user` or `assistant`.
	pub role: String,
	pub content: String,
}

/// What the model said, as the page shows it. Everything here is model output
/// and must be escaped by whoever renders it.
#[derive(Debug, Clone, PartialEq)]
pub enum ChatEvent {
	/// Part of the answer.
	Token(String),
	/// Part of what a reasoning model thought before answering.
	Thinking(String),
	/// Token counts of the whole exchange.
	Usage { prompt: u64, completion: u64 },
	/// The chat failed; the text is for the user.
	Error(String),
}

fn check(messages: &[Message]) -> AppResult<()> {
	let bad = |m: &str| Err(ServerError::Validation(m.to_owned()));
	if messages.is_empty() || messages.len() > MAX_MESSAGES {
		return bad("a conversation has 1 to 40 messages");
	}
	if messages.last().is_none_or(|m| m.role != "user") {
		return bad("the last message must be yours");
	}
	for m in messages {
		if !matches!(m.role.as_str(), "user" | "assistant") {
			return bad("a message is from the user or the assistant");
		}
		if m.content.trim().is_empty() || m.content.chars().count() > MAX_MESSAGE_CHARS {
			return bad("a message has 1 to 8000 characters");
		}
	}
	Ok(())
}

/// The model's playground key, issued when it has none. The bool says whether
/// it was issued now.
async fn ensure_key(s: &AppState, model: &Model) -> AppResult<(String, bool)> {
	let key = format!("pk-{}", crypto::generate_token(32));
	let sealed = s.secrets.encrypt(&key).map_err(|e| {
		tracing::error!("failed to encrypt the playground key: {e}");
		ServerError::Internal
	})?;
	let issued = sqlx::query(
		"INSERT INTO playground_keys (model_id, key_hash, key) VALUES ($1, $2, $3)
		ON CONFLICT DO NOTHING",
	)
	.bind(model.id)
	.bind(hash_key(&key))
	.bind(sealed)
	.execute(&s.db)
	.await?
	.rows_affected()
		== 1;
	if issued {
		return Ok((key, true));
	}
	let sealed: Vec<u8> = sqlx::query_scalar("SELECT key FROM playground_keys WHERE model_id = $1")
		.bind(model.id)
		.fetch_one(&s.db)
		.await?;
	let key = s.secrets.decrypt(&sealed).map_err(|e| {
		tracing::error!(model = model.id.0, "playground key decrypt failed: {e}");
		ServerError::Internal
	})?;
	Ok((key, false))
}

/// Sends the conversation to the model and streams the answer.
///
/// # Errors
/// `Forbidden` without `update model`; `NotFound` for another team's model;
/// `Conflict` unless the model is running; `Validation` for a bad conversation.
/// Failures after the first byte arrive as [`ChatEvent::Error`].
pub async fn chat(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	model: ModelId,
	messages: &[Message],
) -> AppResult<BoxStream<'static, ChatEvent>> {
	authorize(&s.db, user, team, Action::Update, Resource::Model).await?;
	let model = scope::model(&s.db, team, project, model).await?;
	if model.status != ModelStatus::Running {
		return Err(ServerError::Conflict(
			"the model is not running; start it first".to_owned(),
		));
	}
	check(messages)?;

	let server = scope::server(&s.db, team, model.server_id).await?;
	let client = s.agents.client(s, &server).await?;
	let (key, issued) = ensure_key(s, &model).await?;
	if issued {
		links::push(s, &client, server.id).await?;
	}
	let body = json!({
		"model": model.served_name,
		"messages": messages,
		"stream": true,
		"stream_options": { "include_usage": true },
		"max_tokens": MAX_TOKENS,
	});
	let request = GatewayChatRequest {
		key,
		body: body.to_string(),
		timeout_s: TIMEOUT_S,
	};
	let events = client
		.stream(Method::POST, "/v1/gateway/chat", Some(&request))
		.await?;
	Ok(translate(events))
}

fn translate(
	events: BoxStream<'static, Result<Event, ServerError>>,
) -> BoxStream<'static, ChatEvent> {
	events
		.flat_map(|item| {
			let out = match item {
				Ok(Event::Log { line, .. }) => parse_line(&line),
				Ok(Event::Failed { message }) => vec![ChatEvent::Error(message)],
				Ok(_) => Vec::new(),
				Err(e) => {
					tracing::warn!(error = %e, "playground stream failed");
					vec![ChatEvent::Error(
						"the chat stream was interrupted".to_owned(),
					)]
				}
			};
			futures_util::stream::iter(out)
		})
		.boxed()
}

/// One line the chat container printed: `{"data": "<SSE payload>"}`,
/// `{"end": true}` or `{"error": "..."}`.
fn parse_line(line: &str) -> Vec<ChatEvent> {
	let Ok(value) = serde_json::from_str::<Value>(line) else {
		return Vec::new();
	};
	if let Some(error) = value.get("error").and_then(Value::as_str) {
		return vec![ChatEvent::Error(friendly(error))];
	}
	let Some(data) = value.get("data").and_then(Value::as_str) else {
		return Vec::new();
	};
	let Ok(chunk) = serde_json::from_str::<Value>(data) else {
		return Vec::new();
	};
	let mut out = Vec::new();
	let delta = chunk.pointer("/choices/0/delta");
	if let Some(text) = delta
		.and_then(|d| d.get("reasoning_content").or_else(|| d.get("reasoning")))
		.and_then(Value::as_str)
		.filter(|t| !t.is_empty())
	{
		out.push(ChatEvent::Thinking(text.to_owned()));
	}
	if let Some(text) = delta
		.and_then(|d| d.get("content"))
		.and_then(Value::as_str)
		.filter(|t| !t.is_empty())
	{
		out.push(ChatEvent::Token(text.to_owned()));
	}
	if let Some(usage) = chunk.get("usage").filter(|u| u.is_object()) {
		let count = |k: &str| usage.get(k).and_then(Value::as_u64).unwrap_or(0);
		out.push(ChatEvent::Usage {
			prompt: count("prompt_tokens"),
			completion: count("completion_tokens"),
		});
	}
	out
}

/// The gateway's JSON errors carry the message; anything else is shown as is,
/// shortened.
fn friendly(error: &str) -> String {
	let message = error
		.split_once(": ")
		.and_then(|(_, body)| serde_json::from_str::<Value>(body).ok())
		.and_then(|v| {
			v.pointer("/error/message")
				.and_then(Value::as_str)
				.map(str::to_owned)
		})
		.unwrap_or_else(|| error.to_owned());
	message.chars().take(300).collect()
}

#[cfg(test)]
mod tests {
	use super::*;

	fn data(payload: &Value) -> String {
		json!({ "data": payload.to_string() }).to_string()
	}

	#[test]
	fn lines_become_tokens_thinking_and_usage() {
		let tok = data(&json!({"choices": [{"delta": {"content": "Hel"}}]}));
		assert_eq!(parse_line(&tok), [ChatEvent::Token("Hel".into())]);
		let think = data(&json!({"choices": [{"delta": {"reasoning_content": "hm"}}]}));
		assert_eq!(parse_line(&think), [ChatEvent::Thinking("hm".into())]);
		let usage =
			data(&json!({"choices": [], "usage": {"prompt_tokens": 7, "completion_tokens": 3}}));
		assert_eq!(
			parse_line(&usage),
			[ChatEvent::Usage {
				prompt: 7,
				completion: 3
			}]
		);
		let empty = data(&json!({"choices": [{"delta": {"role": "assistant", "content": ""}}]}));
		assert!(parse_line(&empty).is_empty());
		assert!(parse_line("{\"data\":\"[DONE]\"}").is_empty());
		assert!(parse_line("{\"end\":true}").is_empty());
		assert!(parse_line("garbage").is_empty());
	}

	#[test]
	fn gateway_errors_show_their_message() {
		let line = json!({
			"error": "HTTP 404: {\"type\":\"error\",\"error\":{\"type\":\"not_found\",\"message\":\"unknown model\"}}"
		})
		.to_string();
		assert_eq!(
			parse_line(&line),
			[ChatEvent::Error("unknown model".into())]
		);
		let line = json!({ "error": "timed out" }).to_string();
		assert_eq!(parse_line(&line), [ChatEvent::Error("timed out".into())]);
	}

	#[test]
	fn conversations_are_checked() {
		let m = |role: &str, content: &str| Message {
			role: role.into(),
			content: content.into(),
		};
		assert!(check(&[m("user", "hi")]).is_ok());
		assert!(check(&[m("user", "hi"), m("assistant", "yo"), m("user", "more")]).is_ok());
		assert!(check(&[]).is_err());
		assert!(check(&[m("assistant", "hi")]).is_err());
		assert!(check(&[m("system", "x"), m("user", "hi")]).is_err());
		assert!(check(&[m("user", "  ")]).is_err());
		assert!(check(&[m("user", &"x".repeat(MAX_MESSAGE_CHARS + 1))]).is_err());
	}
}
