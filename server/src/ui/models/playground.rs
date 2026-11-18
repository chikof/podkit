//! The playground: a chat box on a model's page. The answer streams from the
//! gateway through the agent and is appended token by token. Every token is
//! model output, so it only ever goes through maud's escaping.

use std::convert::Infallible;
use std::fmt::Write as _;
use std::time::{Duration, Instant};

use axum::Router;
use axum::extract::{Path, State};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use datastar::axum::ReadSignals;
use datastar::consts::ElementPatchMode;
use datastar::prelude::*;
use futures_util::StreamExt;
use futures_util::stream::{self, BoxStream};
use maud::{Markup, html};
use serde::Deserialize;
use serde_json::json;
use tokio_util::sync::CancellationToken;

use super::model_url;
use crate::auth::permission::{Action, Resource};
use crate::ids::{ModelId, ProjectId, TeamId};
use crate::links::playground::{self as chat_service, ChatEvent, Message};
use crate::models::{Model, ModelStatus};
use crate::state::AppState;
use crate::ui::components::{BUTTON, INPUT, card, card_content, card_header};
use crate::ui::error;
use crate::ui::extract::TeamCtx;
use crate::ui::layout::form_error;
use crate::ui::sse::js_string;

pub fn router() -> Router<AppState> {
	Router::new().route(
		"/teams/{team_id}/projects/{project_id}/models/{model_id}/chat",
		post(chat),
	)
}

/// The chat box, shown to someone who may use the model once it runs.
pub fn chat_card(ctx: &TeamCtx, model: &Model) -> Markup {
	if !ctx.perms.can(Action::Update, Resource::Model) {
		return html! {};
	}
	if model.status != ModelStatus::Running {
		return html! {
			(card(&html! {
				(card_header("Playground", None))
				(card_content(&html! {
					p class="text-sm text-muted-foreground" { "Start the model to chat with it." }
				}))
			}))
		};
	}
	let post = format!(
		"evt.preventDefault(); $prompt.trim() && @post({})",
		js_string(&format!(
			"{}/chat",
			model_url(ctx.team.id, model.project_id, model.id)
		))
	);
	card(&html! {
		(card_header("Playground", Some("Chats through the gateway with a key of its own, so it shows up in the usage table as Playground. Nothing is saved.")))
		(card_content(&html! {
			div class="flex flex-col gap-4" data-signals="{prompt: '', chat: []}" {
				div id="pg-thread" class="flex flex-col gap-3" {}
				form class="flex flex-col gap-3" "data-on:submit"=(post) {
					textarea class=(INPUT) rows="3" required maxlength="8000" placeholder="Say something" aria-label="Message"
						"data-bind:prompt" "data-on:keydown"="(evt.metaKey || evt.ctrlKey) && evt.key === 'Enter' && el.form.requestSubmit()" {}
					(form_error())
					div class="flex flex-wrap items-center gap-3" {
						button type="submit" class=(BUTTON) "data-indicator:_busy" "data-attr:disabled"="$_busy" { "Send" }
						button type="button" class="text-sm text-muted-foreground hover:underline" "data-on:click"="$chat = []; document.getElementById('pg-thread').replaceChildren(); document.getElementById('pg-stats').textContent = ''" { "Clear" }
						span id="pg-stats" class="text-xs text-muted-foreground" {}
					}
				}
			}
		}))
	})
}

#[derive(Deserialize)]
struct ChatSignals {
	prompt: String,
	#[serde(default)]
	chat: Vec<Message>,
}

/// What the stream has gathered so far.
struct Run {
	turn: usize,
	prompt: String,
	history: Vec<Message>,
	events: BoxStream<'static, ChatEvent>,
	answer: String,
	started: Instant,
	first_token: Option<Instant>,
	chunks: u64,
	usage: Option<(u64, u64)>,
	failed: bool,
	stage: Stage,
}

enum Stage {
	Opening,
	Streaming,
	Closed,
}

fn append(selector: &str, markup: &Markup) -> Event {
	PatchElements::new(markup.0.clone())
		.selector(selector)
		.mode(ElementPatchMode::Append)
		.write_as_axum_sse_event()
}

fn signals(value: &serde_json::Value) -> Event {
	// `<` and `>` as \u escapes: the same JSON, and nothing that looks like markup.
	let json = value
		.to_string()
		.replace('<', "\\u003c")
		.replace('>', "\\u003e");
	PatchSignals::new(json).write_as_axum_sse_event()
}

impl Run {
	fn opening(&self) -> Vec<Event> {
		let n = self.turn;
		let markup = html! {
			div class="flex flex-col gap-1 rounded-lg border border-border p-3 text-sm" {
				span class="text-xs font-medium text-muted-foreground" { "You" }
				p class="break-words whitespace-pre-wrap" { (self.prompt) }
			}
			div class="flex flex-col gap-1 rounded-lg bg-foreground/5 p-3 text-sm" {
				span class="text-xs font-medium text-muted-foreground" { "Model" }
				p id={ "pg-think-" (n) } class="break-words whitespace-pre-wrap text-muted-foreground italic empty:hidden" {}
				p id={ "pg-ans-" (n) } class="break-words whitespace-pre-wrap" {}
			}
		};
		vec![
			append("#pg-thread", &markup),
			signals(&json!({ "prompt": "", "_error": "" })),
		]
	}

	fn absorb(&mut self, event: ChatEvent) -> Vec<Event> {
		let n = self.turn;
		match event {
			ChatEvent::Token(text) => {
				self.first_token.get_or_insert_with(Instant::now);
				self.chunks += 1;
				self.answer.push_str(&text);
				vec![append(&format!("#pg-ans-{n}"), &html! { span { (text) } })]
			}
			ChatEvent::Thinking(text) => {
				self.first_token.get_or_insert_with(Instant::now);
				vec![append(
					&format!("#pg-think-{n}"),
					&html! { span { (text) } },
				)]
			}
			ChatEvent::Usage { prompt, completion } => {
				self.usage = Some((prompt, completion));
				Vec::new()
			}
			ChatEvent::Error(message) => {
				self.failed = true;
				vec![append(
					&format!("#pg-ans-{n}"),
					&html! { span class="text-destructive" { (message) } },
				)]
			}
		}
	}

	fn stats(&self) -> String {
		let completion = self
			.usage
			.map(|(_, c)| c)
			.filter(|c| *c > 0)
			.unwrap_or(self.chunks);
		let elapsed = self.first_token.unwrap_or(self.started).elapsed();
		let mut out = format!("{completion} tokens");
		if self.first_token.is_some() && elapsed >= Duration::from_millis(100) && completion > 1 {
			let _ = write!(
				out,
				" · {:.1} tokens/s",
				completion as f64 / elapsed.as_secs_f64()
			);
		}
		if let Some((prompt, completion)) = self.usage {
			let _ = write!(out, " · {prompt} in, {completion} out");
		}
		out
	}

	fn finish(&mut self) -> Vec<Event> {
		let mut out = Vec::new();
		if !self.failed && !self.answer.is_empty() {
			let mut chat = std::mem::take(&mut self.history);
			chat.push(Message {
				role: "user".to_owned(),
				content: self.prompt.clone(),
			});
			chat.push(Message {
				role: "assistant".to_owned(),
				content: std::mem::take(&mut self.answer),
			});
			out.push(signals(&json!({ "chat": chat })));
		}
		let stats = html! {
			span id="pg-stats" class="text-xs text-muted-foreground" { (self.stats()) }
		};
		out.push(PatchElements::new(stats.0).write_as_axum_sse_event());
		out
	}
}

/// Streams one exchange as Datastar patches. Everything the model said is
/// escaped by maud; `events` is whatever the gateway produced.
pub(super) fn respond(
	shutdown: CancellationToken,
	prompt: String,
	history: Vec<Message>,
	events: BoxStream<'static, ChatEvent>,
) -> Response {
	let run = Run {
		turn: history.len() / 2,
		prompt,
		history,
		events,
		answer: String::new(),
		started: Instant::now(),
		first_token: None,
		chunks: 0,
		usage: None,
		failed: false,
		stage: Stage::Opening,
	};
	let out = stream::unfold(run, |mut run| async move {
		match run.stage {
			Stage::Opening => {
				run.stage = Stage::Streaming;
				let events = run.opening();
				Some((events, run))
			}
			Stage::Streaming => {
				let events = if let Some(event) = run.events.next().await {
					run.absorb(event)
				} else {
					run.stage = Stage::Closed;
					run.finish()
				};
				Some((events, run))
			}
			Stage::Closed => None,
		}
	})
	.flat_map(|events| stream::iter(events.into_iter().map(Ok::<_, Infallible>)))
	.take_until(shutdown.cancelled_owned());
	Sse::new(out)
		.keep_alive(KeepAlive::default())
		.into_response()
}

async fn chat(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, model)): Path<(TeamId, ProjectId, ModelId)>,
	ReadSignals(sig): ReadSignals<ChatSignals>,
) -> Response {
	let prompt = sig.prompt.trim().to_owned();
	let mut messages = sig.chat;
	messages.push(Message {
		role: "user".to_owned(),
		content: prompt.clone(),
	});
	match chat_service::chat(&s, ctx.user.id, ctx.team.id, project, model, &messages).await {
		Ok(events) => {
			messages.pop();
			respond(s.shutdown.clone(), prompt, messages, events)
		}
		Err(e) => error::action(&e),
	}
}
