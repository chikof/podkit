//! The playground: a chat box on a model's page. The answer streams from the gateway
//! through the agent and is appended token by token. Every token is model output, so it only
//! ever goes through maud's escaping.
//!
//! Screen readers: the thread is a plain container, not a live region, so no token is
//! announced. A visually hidden `role="status"` element says "The model is responding" when
//! a turn starts and "Answer complete, N tokens" when it ends: two announcements per turn.
//! The speaker labels ("You", "Model") are text in the DOM, not CSS generated content.

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
use crate::ui::components::{
	Size, TEXTAREA, Variant, button_class, card, card_content, card_section, empty_state,
};
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

/// Enter sends, Shift+Enter adds a line. While an input method is composing (Japanese,
/// Chinese, Korean), Enter confirms the candidate and must not send: `isComposing`, plus
/// the legacy `keyCode` 229 that Safari reports for the confirming keydown.
const ENTER_SENDS: &str = "if (evt.key === 'Enter' && !evt.shiftKey) { evt.preventDefault(); \
	if (!evt.isComposing && evt.keyCode !== 229 && !$_busy) el.form.requestSubmit() }";

/// The announcement element. Patched twice per turn, never per token.
fn live(text: &str) -> Markup {
	html! { p id="pg-live" class="ui-visually-hidden" role="status" { (text) } }
}

/// The chat box, shown to someone who may use the model once it runs.
pub fn chat_panel(ctx: &TeamCtx, model: &Model) -> Markup {
	if !ctx.perms.can(Action::Update, Resource::Model) {
		return html! {};
	}
	if model.status != ModelStatus::Running {
		return empty_state(
			"bot",
			"The model is not running",
			Some("Start the model to chat with it."),
			None,
		);
	}
	let post = format!(
		"evt.preventDefault(); $prompt.trim() && @post({})",
		js_string(&format!(
			"{}/chat",
			model_url(ctx.team.id, model.project_id, model.id)
		))
	);
	card(&html! {
		(card_section("Playground", Some("Chats through the gateway with a key of its own, so it shows up in the usage table as Playground. Nothing is saved."), None))
		(card_content(&html! {
			div class="flex flex-col gap-4" data-signals="{prompt: '', chat: []}" {
				div id="pg-thread" class="ui-chat-thread flex flex-col gap-4" {}
				(live(""))
				form class="flex flex-col gap-3" "data-on:submit"=(post) {
					label for="pg-input" class="text-sm leading-none font-medium" { "Message" }
					textarea id="pg-input" name="message" class=(TEXTAREA) rows="3" required maxlength="8000"
						placeholder="Say something" aria-describedby="pg-hint"
						"data-bind:prompt" "data-on:keydown"=(ENTER_SENDS) {}
					p id="pg-hint" class="text-xs text-muted-foreground" { "Enter sends. Shift+Enter adds a line." }
					(form_error())
					div class="flex flex-wrap items-center gap-3" {
						button type="submit" class=(button_class(Variant::Default, Size::Md)) "data-indicator:_busy" "data-attr:disabled"="$_busy" { "Send" }
						button type="button" class=(button_class(Variant::Ghost, Size::Md))
							"data-on:click"="$chat = []; document.getElementById('pg-thread').replaceChildren(); document.getElementById('pg-stats').textContent = ''; document.getElementById('pg-live').textContent = ''" { "Clear" }
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

fn patch(markup: &Markup) -> Event {
	PatchElements::new(markup.0.clone()).write_as_axum_sse_event()
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
			article class="flex flex-col gap-2" aria-label={ "Exchange " (n + 1) } {
				div class="ui-chat-user flex flex-col gap-1 p-3 text-sm" {
					span class="text-xs font-medium text-muted-foreground" { "You" }
					p class="break-words whitespace-pre-wrap" { (self.prompt) }
				}
				div class="ui-chat-model flex flex-col gap-1 p-3 text-sm" {
					span class="text-xs font-medium text-muted-foreground" { "Model" }
					p id={ "pg-think-" (n) } class="ui-chat-thinking break-words whitespace-pre-wrap text-muted-foreground italic" {}
					p id={ "pg-ans-" (n) } class="break-words whitespace-pre-wrap" {}
				}
			}
		};
		vec![
			append("#pg-thread", &markup),
			patch(&live("The model is responding.")),
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
		let stats = self.stats();
		out.push(patch(&html! {
			span id="pg-stats" class="text-xs text-muted-foreground" { (stats) }
		}));
		out.push(patch(&live(&if self.failed {
			"The answer failed. The message is in the thread.".to_owned()
		} else {
			format!("Answer complete. {stats}")
		})));
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
