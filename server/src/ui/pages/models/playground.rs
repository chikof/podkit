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
use crate::ui::components::{Size, Variant, button_class, empty_state, icon};
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

/// The label of a turn's thinking block. Patched when the thinking ends, not per token.
fn think_state(turn: usize, text: &str) -> Markup {
	html! { span id={ "pg-think-state-" (turn) } class="ui-think-state" { (text) } }
}

/// The usage figures of the last answer; `—` until there is one. Patched once per turn.
fn usage_stats(figures: Option<&Figures>) -> Markup {
	let cell = |label: &str, value: Option<String>, unit: &str| {
		html! {
			div class="ui-ustat" {
				dt { (label) }
				dd {
					@if let Some(v) = value { (v) @if !unit.is_empty() { span class="ui-ustat-u" { (unit) } } } @else { "—" }
				}
			}
		}
	};
	html! {
		dl id="pg-stats" class="ui-ustats" {
			(cell("Prompt", figures.and_then(|f| f.prompt).map(|v| v.to_string()), "tokens"))
			(cell("Completion", figures.map(|f| f.completion.to_string()), "tokens"))
			(cell("Speed", figures.and_then(|f| f.per_second).map(|v| format!("{v:.1}")), "tok/s"))
			(cell("First token", figures.and_then(|f| f.first_token_ms).map(|v| v.to_string()), "ms"))
		}
	}
}

/// What one answer cost, for the usage panel.
struct Figures {
	prompt: Option<u64>,
	completion: u64,
	per_second: Option<f64>,
	first_token_ms: Option<u128>,
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
	// The textarea is not `required`: an empty box is refused in the handler above, so
	// Clear and a sent message leave nothing for `:user-invalid` to paint red.
	html! {
		div class="ui-pg" data-signals="{prompt: '', chat: []}" {
			section class="ui-chat" aria-label="Conversation" {
				div id="pg-thread" class="ui-msgs" {}
				(live(""))
				form class="ui-composer" "data-on:submit"=(post) {
					label for="pg-input" class="ui-visually-hidden" { "Message" }
					textarea id="pg-input" name="message" class="ui-composer-input" rows="2" maxlength="8000"
						placeholder="Message the model…  (Shift+Enter for a new line)" aria-describedby="pg-hint"
						"data-bind:prompt" "data-on:keydown"=(ENTER_SENDS) {}
					div class="ui-composer-actions" {
						button type="button" class=(button_class(Variant::Ghost, Size::Md))
							"data-on:click"="$chat = []; $prompt = ''; document.getElementById('pg-thread').replaceChildren(); document.getElementById('pg-live').textContent = ''; document.getElementById('pg-stats').querySelectorAll('dd').forEach(d => d.textContent = '—')" { "Clear" }
						button type="submit" class=(button_class(Variant::Default, Size::Md)) "data-indicator:_busy" "data-attr:disabled"="$_busy" { "Send" (icon("arrow-right")) }
					}
				}
				p id="pg-hint" class="ui-hint" { "Enter sends. Shift+Enter adds a line." }
				(form_error())
			}
			aside class="ui-side" aria-label="Session" {
				section class="ui-side-sec" {
					h2 class="ui-sec-h" { "Usage" }
					(usage_stats(None))
					p class="ui-hint" { "Chats through the gateway with a key of its own, so it shows up in the usage table as Playground. Nothing is saved." }
				}
			}
		}
	}
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
	/// When the first thinking token came, and when the thinking gave way to the answer.
	think_start: Option<Instant>,
	think_end: Option<Instant>,
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
		// The thinking block is a native `<details>`: closed until opened, and Enter or Space
		// on its summary toggles it. It stays hidden (`:has(:empty)`) for a model that does
		// not think. Its body is not a live region, so no thinking token is announced.
		let markup = html! {
			article class="ui-xchg" aria-label={ "Exchange " (n + 1) } {
				div class="ui-msg ui-msg-user" {
					span class="ui-msg-who" { "You" }
					p class="ui-msg-body" { (self.prompt) }
				}
				div class="ui-msg ui-msg-bot" {
					span class="ui-msg-who" { "Model" }
					details class="ui-think" {
						summary class="ui-think-sum" {
							(icon("brain"))
							(think_state(n, "Thinking…"))
							(icon("chevron-down"))
						}
						p id={ "pg-think-" (n) } class="ui-think-body" {}
					}
					p id={ "pg-ans-" (n) } class="ui-msg-body ui-answer" {}
				}
			}
		};
		vec![
			append("#pg-thread", &markup),
			patch(&live("The model is responding.")),
			signals(&json!({ "prompt": "", "_error": "" })),
		]
	}

	/// The summary of the thinking block once the thinking is over: said once, not per token.
	fn thought(&mut self) -> Vec<Event> {
		let (Some(start), None) = (self.think_start, self.think_end) else {
			return Vec::new();
		};
		let end = Instant::now();
		self.think_end = Some(end);
		let text = format!("Thought for {:.1} s", (end - start).as_secs_f64());
		vec![patch(&think_state(self.turn, &text))]
	}

	fn absorb(&mut self, event: ChatEvent) -> Vec<Event> {
		let n = self.turn;
		match event {
			ChatEvent::Token(text) => {
				let mut out = self.thought();
				self.first_token.get_or_insert_with(Instant::now);
				self.chunks += 1;
				self.answer.push_str(&text);
				out.push(append(&format!("#pg-ans-{n}"), &html! { span { (text) } }));
				out
			}
			ChatEvent::Thinking(text) => {
				self.first_token.get_or_insert_with(Instant::now);
				self.think_start.get_or_insert_with(Instant::now);
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
				let mut out = self.thought();
				out.push(append(
					&format!("#pg-ans-{n}"),
					&html! { span class="ui-err" { (message) } },
				));
				out
			}
		}
	}

	fn completion(&self) -> u64 {
		self.usage
			.map(|(_, c)| c)
			.filter(|c| *c > 0)
			.unwrap_or(self.chunks)
	}

	fn figures(&self) -> Figures {
		let completion = self.completion();
		let elapsed = self.first_token.unwrap_or(self.started).elapsed();
		Figures {
			prompt: self.usage.map(|(p, _)| p),
			completion,
			per_second: (self.first_token.is_some()
				&& elapsed >= Duration::from_millis(100)
				&& completion > 1)
				.then(|| completion as f64 / elapsed.as_secs_f64()),
			first_token_ms: self
				.first_token
				.map(|t| t.duration_since(self.started).as_millis()),
		}
	}

	fn stats(&self) -> String {
		let completion = self.completion();
		let figures = self.figures();
		let mut out = format!("{completion} tokens");
		if let Some(rate) = figures.per_second {
			let _ = write!(out, " · {rate:.1} tokens/s");
		}
		if let Some((prompt, completion)) = self.usage {
			let _ = write!(out, " · {prompt} in, {completion} out");
		}
		out
	}

	fn finish(&mut self) -> Vec<Event> {
		let mut out = self.thought();
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
		out.push(patch(&usage_stats(Some(&self.figures()))));
		let thought = match (self.think_start, self.think_end) {
			(Some(a), Some(b)) => format!(" Thought for {:.1} s.", (b - a).as_secs_f64()),
			_ => String::new(),
		};
		out.push(patch(&live(&if self.failed {
			"The answer failed. The message is in the thread.".to_owned()
		} else {
			format!("Answer complete.{thought} {stats}")
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
		think_start: None,
		think_end: None,
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
