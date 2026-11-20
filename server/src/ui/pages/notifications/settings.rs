//! Settings → Notifications: channels, the team's SMTP server, rules, metric alerts, noise
//! control and the delivery log.

use std::collections::HashMap;

use axum::Router;
use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use datastar::axum::ReadSignals;
use maud::{Markup, html};
use serde::Deserialize;
use serde_json::{Value, json};
use time::OffsetDateTime;
use tower_cookies::Cookies;

use crate::auth::permission::{Action, Resource};
use crate::error::ServerError;
use crate::ids::{
	MetricAlertId, NotificationChannelId, NotificationDeliveryId, NotificationRuleId, TeamId,
};
use crate::notifications::alerts::{Metric, MetricAlert};
use crate::notifications::channel::{Channel, ChannelFields, ChannelKind};
use crate::notifications::service::{
	self, AlertInput, AlertTargets, ChannelEdit, DeliveryRow, Rule, RuleInput, SmtpInput,
};
use crate::notifications::settings::{Security, Settings, Smtp};
use crate::notifications::{EventKind, Severity};
use crate::state::AppState;
use crate::ui::components::{
	BUTTON, BUTTON_OUTLINE, CHECKBOX, INPUT, LABEL, SELECT, SWITCH, Size, Tone, Variant, badge,
	button_class, card, card_content, card_footer, card_section, dialog, empty_state, icon, table,
};
use crate::ui::extract::TeamCtx;
use crate::ui::layout::Crumb;
use crate::ui::pages::projects::cards::relative_time;
use crate::ui::pages::settings::{settings_body, settings_crumbs};
use crate::ui::sse::js_string;
use crate::ui::{error, shell, sse};

const BASE: &str = "/teams/{team_id}/settings/notifications";

pub fn router() -> Router<AppState> {
	let p = |suffix: &str| format!("{BASE}{suffix}");
	Router::new()
		.route(&p(""), get(page))
		.route(&p("/channels"), post(create_channel))
		.route(&p("/channels/{id}/test"), post(test_channel))
		.route(&p("/channels/{id}/enable"), post(enable_channel))
		.route(&p("/channels/{id}/disable"), post(disable_channel))
		.route(&p("/channels/{id}/delete"), post(delete_channel))
		.route(&p("/smtp"), post(save_smtp))
		.route(&p("/smtp/delete"), post(delete_smtp))
		.route(&p("/rules"), post(create_rule))
		.route(&p("/rules/{id}/enable"), post(enable_rule))
		.route(&p("/rules/{id}/disable"), post(disable_rule))
		.route(&p("/rules/{id}/delete"), post(delete_rule))
		.route(&p("/alerts"), post(create_alert))
		.route(&p("/alerts/{id}/enable"), post(enable_alert))
		.route(&p("/alerts/{id}/disable"), post(disable_alert))
		.route(&p("/alerts/{id}/delete"), post(delete_alert))
		.route(&p("/noise"), post(save_noise))
		.route(&p("/deliveries/{id}/retry"), post(retry_delivery))
}

fn page_url(team: TeamId) -> String {
	format!("/teams/{team}/settings/notifications")
}

fn post_to(team: TeamId, suffix: &str) -> String {
	format!(
		"@post({})",
		js_string(&format!("{}{suffix}", page_url(team)))
	)
}

fn err_line(signal: &str) -> Markup {
	html! {
		p class="text-sm text-destructive" role="alert" "data-show"={ "$" (signal) } "data-text"={ "$" (signal) } style="display: none" {}
	}
}

fn tone(severity: Severity) -> Tone {
	match severity {
		Severity::Info => Tone::Neutral,
		Severity::Warning => Tone::Warning,
		Severity::Critical => Tone::Danger,
	}
}

fn minutes_text(minute: i32) -> String {
	format!("{:02}:{:02}", minute / 60, minute % 60)
}

// --- the page ---------------------------------------------------------------------------------

struct Data {
	channels: Vec<Channel>,
	smtp: Option<Smtp>,
	settings: Settings,
	rules: Vec<Rule>,
	alerts: Vec<MetricAlert>,
	targets: AlertTargets,
	scopes: Vec<service::ProjectScope>,
	recent: Vec<DeliveryRow>,
	dead: Vec<DeliveryRow>,
}

async fn load(s: &AppState, ctx: &TeamCtx) -> Result<Data, ServerError> {
	let (user, team) = (ctx.user.id, ctx.team.id);
	let (recent, dead) = service::delivery_log(&s.db, user, team).await?;
	Ok(Data {
		channels: service::channels(&s.db, user, team).await?,
		smtp: service::smtp(&s.db, user, team).await?,
		settings: service::settings(&s.db, user, team).await?,
		rules: service::rules(&s.db, user, team).await?,
		alerts: service::alerts(&s.db, user, team).await?,
		targets: service::alert_targets(&s.db, user, team).await?,
		scopes: service::scopes(&s.db, user, team).await?,
		recent,
		dead,
	})
}

/// The signals the page starts with. Nothing secret is in here: the password and token fields
/// start empty whatever is saved.
fn signals(data: &Data) -> String {
	let mut out = json!({
		"ch_name": "", "ch_kind": "discord", "ch_url": "", "ch_token": "", "ch_signing": "",
		"ch_chat": "", "ch_server": "", "ch_topic": "", "ch_to": "",
		"smtp_host": data.smtp.as_ref().map_or("", |m| m.host.as_str()),
		"smtp_port": data.smtp.as_ref().map_or_else(|| "587".to_owned(), |m| m.port.to_string()),
		"smtp_security": data.smtp.as_ref().map_or("starttls", |m| m.security.as_str()),
		"smtp_user": data.smtp.as_ref().and_then(|m| m.username.as_deref()).unwrap_or(""),
		"smtp_password": "",
		"smtp_from": data.smtp.as_ref().map_or("", |m| m.from_addr.as_str()),
		"rule_name": "", "rule_min": "info", "rule_scope": "",
		"al_name": "", "al_target": "", "al_metric": "cpu", "al_threshold": "",
		"al_minutes": "5", "al_severity": "warning",
		"nz_dedup": data.settings.dedup_minutes.to_string(),
		"nz_quiet": data.settings.quiet_enabled,
		"nz_start": minutes_text(data.settings.quiet_start_min),
		"nz_end": minutes_text(data.settings.quiet_end_min),
		"nz_offset": offset_hours(data.settings.utc_offset_min),
		"_ch_error": "", "_smtp_error": "", "_rule_error": "", "_al_error": "", "_nz_error": "",
		"_del_url": "", "_del_name": "", "_del_error": "",
	});
	let map = out.as_object_mut().expect("an object");
	for kind in EventKind::ALL {
		map.insert(format!("ev_{}", kind.as_str()), json!(false));
	}
	for channel in &data.channels {
		map.insert(format!("rch_{}", channel.id), json!(false));
	}
	out.to_string()
}

fn offset_hours(minutes: i32) -> String {
	let hours = f64::from(minutes) / 60.0;
	if (hours.fract()).abs() < f64::EPSILON {
		format!("{hours:.0}")
	} else {
		format!("{hours}")
	}
}

/// A block shown only while `$ch_kind` is one of `kinds`.
fn only_for(kinds: &[&str], content: &Markup) -> Markup {
	let list = kinds
		.iter()
		.map(|k| format!("'{k}'"))
		.collect::<Vec<_>>()
		.join(", ");
	let initial_hidden = !kinds.contains(&"discord");
	html! {
		div class="flex flex-col gap-4" "data-show"={ "[" (list) "].includes($ch_kind)" }
			style=[initial_hidden.then_some("display: none")] { (content) }
	}
}

fn kind_options() -> Markup {
	html! {
		@for kind in ChannelKind::ALL {
			option value=(kind.as_str()) { (kind.label()) }
		}
	}
}

fn add_channel_card(team: TeamId) -> Markup {
	let post = format!("evt.preventDefault(); {}", post_to(team, "/channels"));
	card(&html! {
		form id="add-channel" aria-label="Add a channel" "data-on:submit"=(post) {
			(card_section("Add a channel", Some("Secrets are stored encrypted and never shown again."), None))
			(card_content(&html! {
				div class="ui-pair" {
					label class=(LABEL) {
						"Name"
						input class=(INPUT) type="text" required maxlength="60" autocomplete="off" placeholder="Team chat" "data-bind:ch_name";
					}
					label class=(LABEL) {
						"Type"
						select class=(SELECT) "data-bind:ch_kind" { (kind_options()) }
					}
				}
				(only_for(&["discord", "slack", "webhook"], &html! {
					label class=(LABEL) {
						"Webhook URL"
						input class=(INPUT) type="url" autocomplete="off" spellcheck="false" inputmode="url" "data-bind:ch_url";
						span class="ui-hint" { "It carries a token, so it is stored encrypted like a password." }
					}
				}))
				(only_for(&["webhook"], &html! {
					label class=(LABEL) {
						"Signing secret"
						input class=(INPUT) type="password" autocomplete="new-password" spellcheck="false" "data-bind:ch_signing";
						span class="ui-hint" { "Each call carries X-Podkit-Signature: sha256=…, an HMAC-SHA256 of the body made with this secret. At least 8 characters." }
					}
				}))
				(only_for(&["telegram"], &html! {
					div class="ui-pair" {
						label class=(LABEL) {
							"Bot token"
							input class=(INPUT) type="password" autocomplete="new-password" spellcheck="false" "data-bind:ch_token";
						}
						label class=(LABEL) {
							"Chat ID"
							input class=(INPUT) type="text" autocomplete="off" spellcheck="false" placeholder="-1001234567890" "data-bind:ch_chat";
						}
					}
				}))
				(only_for(&["ntfy"], &html! {
					div class="ui-pair" {
						label class=(LABEL) {
							"Server"
							input class=(INPUT) type="url" autocomplete="off" spellcheck="false" inputmode="url" placeholder="https://ntfy.sh" "data-bind:ch_server";
						}
						label class=(LABEL) {
							"Topic"
							input class=(INPUT) type="text" autocomplete="off" spellcheck="false" "data-bind:ch_topic";
						}
					}
					label class=(LABEL) {
						"Access token (optional)"
						input class=(INPUT) type="password" autocomplete="new-password" spellcheck="false" "data-bind:ch_token";
						span class="ui-hint" { "Only for a topic that needs a login. Anyone who knows an open topic's name can read it." }
					}
				}))
				(only_for(&["email"], &html! {
					label class=(LABEL) {
						"Send to"
						input class=(INPUT) type="email" autocomplete="off" "data-bind:ch_to";
						span class="ui-hint" { "Uses the team's email server below." }
					}
				}))
				(err_line("_ch_error"))
			}))
			(card_footer(&html! {
				button type="submit" class=(BUTTON) "data-indicator:_ch_busy" "data-attr:disabled"="$_ch_busy" { "Add channel" }
			}))
		}
	})
}

fn test_line(id: NotificationChannelId, ok: Option<bool>, message: Option<&str>) -> Markup {
	let tone = ok.map(|ok| {
		if ok {
			"text-success"
		} else {
			"text-destructive"
		}
	});
	html! {
		p id={ "channel-test-" (id) } class={ "text-sm " (tone.unwrap_or_default()) } role="status" hidden[ok.is_none()] {
			@if let Some(message) = message { (message) }
		}
	}
}

fn channel_card(
	team: TeamId,
	channel: &Channel,
	can_edit: bool,
	can_delete: bool,
	now: OffsetDateTime,
) -> Markup {
	let id = channel.id;
	let open_delete = format!(
		"$_del_url = {}; $_del_name = {}; $_del_error = ''; el.commandForElement || document.getElementById('delete-note-dialog').showModal()",
		js_string(&format!("{}/channels/{id}/delete", page_url(team))),
		js_string(&channel.name)
	);
	let (last, message) = match (channel.last_test_ok, &channel.last_test_error) {
		(Some(true), _) => (Some(true), Some("Last test: sent.".to_owned())),
		(Some(false), error) => (
			Some(false),
			Some(format!(
				"Last test failed: {}",
				error.as_deref().unwrap_or("no answer")
			)),
		),
		(None, _) => (None, None),
	};
	html! {
		article class="ui-card ui-dest" {
			div class="ui-dest-head" {
				span class="ui-pick-icon" aria-hidden="true" { (icon("bell")) }
				div class="ui-dest-id" {
					h3 title=(channel.name) { (channel.name) }
					p { (channel.kind.label()) }
				}
				@if can_delete {
					button type="button" class=(button_class(Variant::Ghost, Size::Icon)) aria-label={ "Delete " (channel.name) }
						command="show-modal" commandfor="delete-note-dialog" aria-haspopup="dialog" "data-on:click"=(open_delete) {
						(icon("trash-2"))
					}
				}
			}
			dl class="ui-dest-facts" {
				dt { "Sends to" }
				dd class="ui-mono" { (channel.hint) }
				dt { "State" }
				dd {
					@if channel.enabled { (badge(Tone::Success, "On")) } @else { (badge(Tone::Neutral, "Off")) }
				}
				@if let Some(at) = channel.last_test_at {
					dt { "Last test" }
					dd { (relative_time(at, now)) }
				}
			}
			(test_line(id, last, message.as_deref()))
			@if can_edit {
				div class="flex flex-wrap gap-2" {
					button type="button" class=(BUTTON_OUTLINE) "data-on:click"=(post_to(team, &format!("/channels/{id}/test")))
						"data-indicator:_testing" "data-attr:disabled"="$_testing" { (icon("zap")) "Send test" }
					@if channel.enabled {
						button type="button" class=(BUTTON_OUTLINE) "data-on:click"=(post_to(team, &format!("/channels/{id}/disable"))) { "Turn off" }
					} @else {
						button type="button" class=(BUTTON_OUTLINE) "data-on:click"=(post_to(team, &format!("/channels/{id}/enable"))) { "Turn on" }
					}
				}
			}
		}
	}
}

fn channels_section(ctx: &TeamCtx, data: &Data, now: OffsetDateTime) -> Markup {
	let can_create = ctx.perms.can(Action::Create, Resource::Notification);
	let can_edit = ctx.perms.can(Action::Update, Resource::Notification);
	let can_delete = ctx.perms.can(Action::Delete, Resource::Notification);
	html! {
		section class="ui-stack" aria-labelledby="channels-title" id="channels" {
			h2 id="channels-title" class="ui-sec-h" { "Channels" }
			p class="ui-lede" { "Where a message can go: Discord, Slack, Telegram, ntfy, email or a webhook of your own." }
			@if data.channels.is_empty() {
				(empty_state("bell", "No channels yet.",
					Some(if can_create { "Add one below, press Send test, then point a rule at it." } else { "Ask someone who can manage notifications to add one." }),
					None))
			} @else {
				div class="ui-dest-grid" {
					@for channel in &data.channels { (channel_card(ctx.team.id, channel, can_edit, can_delete, now)) }
				}
			}
			@if can_create { (add_channel_card(ctx.team.id)) }
		}
	}
}

fn smtp_section(ctx: &TeamCtx, data: &Data) -> Markup {
	let can_update = ctx.perms.can(Action::Update, Resource::Notification);
	let post = format!("evt.preventDefault(); {}", post_to(ctx.team.id, "/smtp"));
	let has_password = data.smtp.as_ref().is_some_and(|m| m.password.is_some());
	html! {
		section class="ui-stack" aria-labelledby="smtp-title" id="smtp" {
			h2 id="smtp-title" class="ui-sec-h" { "Email server" }
			p class="ui-lede" { "Email channels send through this SMTP server. Without one they cannot send." }
			(card(&html! {
				form id="smtp-form" aria-label="Email server" "data-on:submit"=(post) {
					(card_content(&html! {
						fieldset class="m-0 flex min-w-0 flex-col gap-4 border-0 p-0" disabled[!can_update] {
							legend class="ui-visually-hidden" { "Email server" }
							div class="ui-pair" {
								label class=(LABEL) {
									"Host"
									input class=(INPUT) type="text" required autocomplete="off" spellcheck="false" placeholder="smtp.example.com" "data-bind:smtp_host";
								}
								div class="ui-pair" {
									label class=(LABEL) {
										"Port"
										input class=(INPUT) type="number" min="1" max="65535" required inputmode="numeric" "data-bind:smtp_port";
									}
									label class=(LABEL) {
										"Security"
										select class=(SELECT) "data-bind:smtp_security" {
											@for mode in [Security::StartTls, Security::Tls, Security::None] {
												option value=(mode.as_str()) {
													@match mode {
														Security::StartTls => { "STARTTLS (587)" }
														Security::Tls => { "TLS (465)" }
														Security::None => { "None (trusted network only)" }
													}
												}
											}
										}
									}
								}
							}
							div class="ui-pair" {
								label class=(LABEL) {
									"Username"
									input class=(INPUT) type="text" autocomplete="off" spellcheck="false" "data-bind:smtp_user";
								}
								label class=(LABEL) {
									"Password"
									input class=(INPUT) type="password" autocomplete="new-password" spellcheck="false"
										placeholder=(if has_password { "Leave empty to keep the stored one" } else { "" }) "data-bind:smtp_password";
									span class="ui-hint" { "Stored encrypted and never shown again." }
								}
							}
							label class=(LABEL) {
								"From"
								input class=(INPUT) type="text" required autocomplete="off" spellcheck="false" placeholder="podkit <podkit@example.com>" "data-bind:smtp_from";
							}
						}
						(err_line("_smtp_error"))
					}))
					@if can_update {
						(card_footer(&html! {
							button type="submit" class=(BUTTON) "data-indicator:_smtp_busy" "data-attr:disabled"="$_smtp_busy" { "Save email server" }
							@if data.smtp.is_some() {
								button type="button" class=(BUTTON_OUTLINE) "data-on:click"=(post_to(ctx.team.id, "/smtp/delete")) { "Remove" }
							}
						}))
					}
				}
			}))
		}
	}
}

fn scope_options(data: &Data) -> Markup {
	html! {
		option value="" { "Everything in the team" }
		@for (project, name, envs) in &data.scopes {
			option value={ "p:" (project) } { (name) " (every environment)" }
			@for (env, env_name) in envs {
				option value={ "e:" (project) ":" (env) } { (name) " · " (env_name) }
			}
		}
	}
}

fn severity_options() -> Markup {
	html! {
		@for sev in Severity::ALL { option value=(sev.as_str()) { (sev.label()) } }
	}
}

fn scope_text(data: &Data, rule: &Rule) -> String {
	let Some(project) = rule.project_id else {
		return "Whole team".to_owned();
	};
	let Some((_, name, envs)) = data.scopes.iter().find(|(id, _, _)| *id == project) else {
		return "A project".to_owned();
	};
	match rule
		.environment_id
		.and_then(|e| envs.iter().find(|(id, _)| *id == e))
	{
		Some((_, env)) => format!("{name} · {env}"),
		None => format!("{name} (every environment)"),
	}
}

fn rule_row(ctx: &TeamCtx, data: &Data, rule: &Rule, can_edit: bool, can_delete: bool) -> Markup {
	let kinds: Vec<&str> = rule
		.kinds
		.iter()
		.filter_map(|k| EventKind::parse(k))
		.map(EventKind::label)
		.collect();
	let channels: Vec<&str> = data
		.channels
		.iter()
		.filter(|c| rule.channel_ids.contains(&c.id.0))
		.map(|c| c.name.as_str())
		.collect();
	let team = ctx.team.id;
	let open_delete = format!(
		"$_del_url = {}; $_del_name = {}; $_del_error = ''; el.commandForElement || document.getElementById('delete-note-dialog').showModal()",
		js_string(&format!("{}/rules/{}/delete", page_url(team), rule.id)),
		js_string(&rule.name)
	);
	let severity = Severity::from_rank(rule.min_severity);
	html! {
		li class="ui-list-row" {
			div class="ui-list-main" {
				strong { (rule.name) }
				p { (kinds.join(", ")) }
				p { "→ " (channels.join(", ")) " · " (scope_text(data, rule)) }
			}
			(badge(tone(severity), &format!("{} and up", severity.label())))
			@if rule.enabled { (badge(Tone::Success, "On")) } @else { (badge(Tone::Neutral, "Off")) }
			@if can_edit {
				@if rule.enabled {
					button type="button" class=(BUTTON_OUTLINE) "data-on:click"=(post_to(team, &format!("/rules/{}/disable", rule.id))) { "Turn off" }
				} @else {
					button type="button" class=(BUTTON_OUTLINE) "data-on:click"=(post_to(team, &format!("/rules/{}/enable", rule.id))) { "Turn on" }
				}
			}
			@if can_delete {
				button type="button" class=(button_class(Variant::Ghost, Size::Icon)) aria-label={ "Delete rule " (rule.name) }
					command="show-modal" commandfor="delete-note-dialog" aria-haspopup="dialog" "data-on:click"=(open_delete) {
					(icon("trash-2"))
				}
			}
		}
	}
}

fn add_rule_card(ctx: &TeamCtx, data: &Data) -> Markup {
	let post = format!("evt.preventDefault(); {}", post_to(ctx.team.id, "/rules"));
	let mut groups: Vec<(&str, Vec<EventKind>)> = Vec::new();
	for kind in EventKind::ALL {
		match groups.iter_mut().find(|(g, _)| *g == kind.group()) {
			Some((_, list)) => list.push(kind),
			None => groups.push((kind.group(), vec![kind])),
		}
	}
	card(&html! {
		form id="add-rule" aria-label="Add a rule" "data-on:submit"=(post) {
			(card_section("Add a rule", Some("When one of the events below happens, send it to the channels you tick."), None))
			(card_content(&html! {
				label class=(LABEL) {
					"Name"
					input class=(INPUT) type="text" required maxlength="60" autocomplete="off" placeholder="Failures to chat" "data-bind:rule_name";
				}
				@for (group, kinds) in &groups {
					fieldset class="ui-checks" {
						legend { (group) }
						@for kind in kinds {
							div class="ui-check" {
								input id={ "ev-" (kind.as_str()) } type="checkbox" class=(CHECKBOX) "data-bind"={ "ev_" (kind.as_str()) };
								label for={ "ev-" (kind.as_str()) } class="cursor-pointer" { (kind.label()) }
							}
						}
					}
				}
				@if data.channels.is_empty() {
					p class="text-sm text-muted-foreground" { "Add a channel first: a rule needs somewhere to send to." }
				} @else {
					fieldset class="ui-checks" {
						legend { "Send to" }
						@for channel in &data.channels {
							div class="ui-check" {
								input id={ "rch-" (channel.id) } type="checkbox" class=(CHECKBOX) "data-bind"={ "rch_" (channel.id) };
								label for={ "rch-" (channel.id) } class="cursor-pointer" { (channel.name) " (" (channel.kind.label()) ")" }
							}
						}
					}
				}
				div class="ui-pair" {
					label class=(LABEL) {
						"Only for"
						select class=(SELECT) "data-bind:rule_scope" { (scope_options(data)) }
					}
					label class=(LABEL) {
						"Minimum severity"
						select class=(SELECT) "data-bind:rule_min" { (severity_options()) }
					}
				}
				(err_line("_rule_error"))
			}))
			(card_footer(&html! {
				button type="submit" class=(BUTTON) "data-indicator:_rule_busy" "data-attr:disabled"="$_rule_busy" { "Add rule" }
			}))
		}
	})
}

fn rules_section(ctx: &TeamCtx, data: &Data) -> Markup {
	let can_create = ctx.perms.can(Action::Create, Resource::Notification);
	let can_edit = ctx.perms.can(Action::Update, Resource::Notification);
	let can_delete = ctx.perms.can(Action::Delete, Resource::Notification);
	html! {
		section class="ui-stack" aria-labelledby="rules-title" id="rules" {
			h2 id="rules-title" class="ui-sec-h" { "Rules" }
			p class="ui-lede" { "Which events go to which channels, optionally for one project or environment and above a severity." }
			@if data.rules.is_empty() {
				(empty_state("inbox", "No rules yet.", Some("Without a rule an event only shows up in the bell."), None))
			} @else {
				(card(&html! {
					ul class="m-0 list-none p-4" {
						@for rule in &data.rules { (rule_row(ctx, data, rule, can_edit, can_delete)) }
					}
				}))
			}
			@if can_create { (add_rule_card(ctx, data)) }
		}
	}
}

fn alert_target_text(data: &Data, alert: &MetricAlert) -> String {
	if alert.scope == "server" {
		let name = data
			.targets
			.servers
			.iter()
			.find(|(id, _)| Some(*id) == alert.server_id.map(|s| s.0))
			.map_or("a server", |(_, n)| n.as_str());
		return format!("Server {name}");
	}
	let name = data
		.targets
		.resources
		.iter()
		.find(|(kind, id, _)| {
			Some(kind.as_str()) == alert.resource_kind.as_deref() && Some(*id) == alert.resource_id
		})
		.map_or("a resource", |(_, _, n)| n.as_str());
	format!(
		"{} {name}",
		alert.resource_kind.as_deref().unwrap_or("resource")
	)
}

fn alert_row(
	ctx: &TeamCtx,
	data: &Data,
	alert: &MetricAlert,
	can_edit: bool,
	can_delete: bool,
) -> Markup {
	let team = ctx.team.id;
	let metric = Metric::parse(&alert.metric);
	let open_delete = format!(
		"$_del_url = {}; $_del_name = {}; $_del_error = ''; el.commandForElement || document.getElementById('delete-note-dialog').showModal()",
		js_string(&format!("{}/alerts/{}/delete", page_url(team), alert.id)),
		js_string(&alert.name)
	);
	html! {
		li class="ui-list-row" {
			div class="ui-list-main" {
				strong { (alert.name) }
				p {
					(alert_target_text(data, alert)) ": "
					(metric.map_or("metric", Metric::label)) " over " (alert.threshold) (metric.map_or("", Metric::unit))
					" for " (alert.duration_minutes) " min"
				}
			}
			(badge(tone(Severity::from_rank(alert.severity)), Severity::from_rank(alert.severity).label()))
			@if alert.state == "firing" { (badge(Tone::Danger, "Firing")) }
			@else if alert.enabled { (badge(Tone::Success, "Watching")) }
			@else { (badge(Tone::Neutral, "Off")) }
			@if can_edit {
				@if alert.enabled {
					button type="button" class=(BUTTON_OUTLINE) "data-on:click"=(post_to(team, &format!("/alerts/{}/disable", alert.id))) { "Turn off" }
				} @else {
					button type="button" class=(BUTTON_OUTLINE) "data-on:click"=(post_to(team, &format!("/alerts/{}/enable", alert.id))) { "Turn on" }
				}
			}
			@if can_delete {
				button type="button" class=(button_class(Variant::Ghost, Size::Icon)) aria-label={ "Delete alert " (alert.name) }
					command="show-modal" commandfor="delete-note-dialog" aria-haspopup="dialog" "data-on:click"=(open_delete) {
					(icon("trash-2"))
				}
			}
		}
	}
}

fn add_alert_card(ctx: &TeamCtx, data: &Data) -> Markup {
	let post = format!("evt.preventDefault(); {}", post_to(ctx.team.id, "/alerts"));
	card(&html! {
		form id="add-alert" aria-label="Add a metric alert" "data-on:submit"=(post) {
			(card_section("Add a metric alert", Some("Fires when a value stays over the threshold for the whole time, and says so when it is back under."), None))
			(card_content(&html! {
				div class="ui-pair" {
					label class=(LABEL) {
						"Name"
						input class=(INPUT) type="text" required maxlength="60" autocomplete="off" placeholder="Disk almost full" "data-bind:al_name";
					}
					label class=(LABEL) {
						"Watch"
						select class=(SELECT) required "data-bind:al_target" {
							option value="" { "Choose…" }
							@for (id, name) in &data.targets.servers { option value={ "server:" (id) } { "Server · " (name) } }
							@for (kind, id, name) in &data.targets.resources { option value={ (kind) ":" (id) } { (kind) " · " (name) } }
						}
					}
				}
				div class="ui-pair" {
					label class=(LABEL) {
						"Metric"
						select class=(SELECT) "data-bind:al_metric" {
							option value="cpu" { "CPU (%)" }
							option value="mem" { "Memory (% of the server)" }
							option value="disk" { "Disk (% of the server)" }
							option value="gpu" { "GPU (% of the server)" }
							option value="mem_mb" { "Memory (MiB, for an app, database, model or stack)" }
						}
					}
					div class="ui-pair" {
						label class=(LABEL) {
							"Over"
							input class=(INPUT) type="number" min="1" step="any" required inputmode="decimal" "data-bind:al_threshold";
						}
						label class=(LABEL) {
							"For (minutes)"
							input class=(INPUT) type="number" min="1" max="1440" required inputmode="numeric" "data-bind:al_minutes";
						}
					}
				}
				label class=(LABEL) {
					"Severity"
					select class=(SELECT) "data-bind:al_severity" { (severity_options()) }
				}
				(err_line("_al_error"))
			}))
			(card_footer(&html! {
				button type="submit" class=(BUTTON) "data-indicator:_al_busy" "data-attr:disabled"="$_al_busy" { "Add alert" }
			}))
		}
	})
}

fn alerts_section(ctx: &TeamCtx, data: &Data) -> Markup {
	let can_create = ctx.perms.can(Action::Create, Resource::Notification);
	let can_edit = ctx.perms.can(Action::Update, Resource::Notification);
	let can_delete = ctx.perms.can(Action::Delete, Resource::Notification);
	html! {
		section class="ui-stack" aria-labelledby="alerts-title" id="alerts" {
			h2 id="alerts-title" class="ui-sec-h" { "Metric alerts" }
			p class="ui-lede" { "Checked every time podkit samples its servers. An alert sends a “" (EventKind::MetricAlert.label()) "” event; rules route it like any other." }
			@if data.alerts.is_empty() {
				(empty_state("activity", "No metric alerts.", Some("Try “Disk over 90% for 10 minutes” on your main server."), None))
			} @else {
				(card(&html! {
					ul class="m-0 list-none p-4" {
						@for alert in &data.alerts { (alert_row(ctx, data, alert, can_edit, can_delete)) }
					}
				}))
			}
			@if can_create { (add_alert_card(ctx, data)) }
		}
	}
}

fn noise_section(ctx: &TeamCtx) -> Markup {
	let can_update = ctx.perms.can(Action::Update, Resource::Notification);
	let post = format!("evt.preventDefault(); {}", post_to(ctx.team.id, "/noise"));
	html! {
		section class="ui-stack" aria-labelledby="noise-title" id="noise" {
			h2 id="noise-title" class="ui-sec-h" { "Noise control" }
			(card(&html! {
				form id="noise-form" aria-label="Noise control" "data-on:submit"=(post) {
					(card_content(&html! {
						fieldset class="m-0 flex min-w-0 flex-col gap-4 border-0 p-0" disabled[!can_update] {
							legend class="ui-visually-hidden" { "Noise control" }
							label class=(LABEL) {
								"Collapse repeats within (minutes)"
								input class=(INPUT) type="number" min="1" max="1440" required inputmode="numeric" "data-bind:nz_dedup";
								span class="ui-hint" { "The same event inside this time is sent once; a follow-up says how many times it happened." }
							}
							div class="ui-check-row" {
								input id="nz-quiet" type="checkbox" switch role="switch" class=(SWITCH) "data-bind:nz_quiet" aria-describedby="nz-quiet-help";
								div {
									label for="nz-quiet" class="ui-check-title" { "Quiet hours" }
									small id="nz-quiet-help" { "Between these times only critical events are sent at once; the rest arrive as one digest when they end." }
								}
							}
							div class="ui-pair" {
								label class=(LABEL) { "From" input class=(INPUT) type="time" required "data-bind:nz_start"; }
								label class=(LABEL) { "Until" input class=(INPUT) type="time" required "data-bind:nz_end"; }
							}
							label class=(LABEL) {
								"Your time zone, in hours from UTC"
								input class=(INPUT) type="number" min="-14" max="14" step="0.25" required inputmode="decimal" "data-bind:nz_offset";
								span class="ui-hint" { "Quiet hours follow this offset all year; change it when the clocks change." }
							}
						}
						(err_line("_nz_error"))
					}))
					@if can_update {
						(card_footer(&html! {
							button type="submit" class=(BUTTON) "data-indicator:_nz_busy" "data-attr:disabled"="$_nz_busy" { "Save" }
						}))
					}
				}
			}))
		}
	}
}

fn status_tone(status: &str) -> Tone {
	match status {
		"sent" => Tone::Success,
		"dead" => Tone::Danger,
		"sending" => Tone::Progress,
		_ => Tone::Neutral,
	}
}

fn status_text(status: &str) -> &'static str {
	match status {
		"sent" => "Sent",
		"dead" => "Failed",
		"sending" => "Sending",
		"deferred" => "Held for digest",
		"queued" => "Waiting",
		_ => "Other",
	}
}

fn delivery_cells(
	team: TeamId,
	row: &DeliveryRow,
	now: OffsetDateTime,
	retry: bool,
) -> Vec<Markup> {
	vec![
		html! { (row.title.as_deref().unwrap_or("Notification")) },
		html! { (row.channel_name) },
		html! { (badge(status_tone(&row.status), status_text(&row.status))) },
		html! { (row.attempts) },
		html! { @if let Some(error) = &row.last_error { span class="text-sm" { (error) } } @else { "—" } },
		html! { (relative_time(row.created_at, now)) },
		html! {
			@if retry {
				button type="button" class=(BUTTON_OUTLINE) "data-on:click"=(post_to(team, &format!("/deliveries/{}/retry", row.id))) { "Retry" }
			}
		},
	]
}

fn log_section(ctx: &TeamCtx, data: &Data, now: OffsetDateTime) -> Markup {
	let can_edit = ctx.perms.can(Action::Update, Resource::Notification);
	let head = [
		"Message",
		"Channel",
		"Status",
		"Tries",
		"Last error",
		"When",
		"",
	];
	let dead: Vec<Vec<Markup>> = data
		.dead
		.iter()
		.map(|r| delivery_cells(ctx.team.id, r, now, can_edit))
		.collect();
	let recent: Vec<Vec<Markup>> = data
		.recent
		.iter()
		.map(|r| delivery_cells(ctx.team.id, r, now, false))
		.collect();
	html! {
		section class="ui-stack" aria-labelledby="log-title" id="deliveries" {
			h2 id="log-title" class="ui-sec-h" { "Delivery log" }
			p class="ui-lede" { "A failed send is retried after 30 s, 1, 2, 4 and 8 minutes. After that it waits here until you retry it." }
			@if !dead.is_empty() {
				(card(&html! {
					(card_section("Failed for good", Some("These ran out of retries."), None))
					(table("dead-letters", "Deliveries that failed for good", &head, &dead))
				}))
			}
			@if recent.is_empty() {
				(empty_state("history", "Nothing sent yet.", Some("Messages appear here as rules send them."), None))
			} @else {
				(card(&html! {
					(card_section("Latest", Some("The newest 30 messages."), None))
					(table("deliveries-table", "The newest deliveries", &head, &recent))
				}))
			}
		}
	}
}

fn delete_dialog() -> Markup {
	dialog(
		"delete-note-dialog",
		"Delete this?",
		Some(
			"A channel's queued messages go with it; rules that used only that channel stop sending.",
		),
		&html! {
			div class="flex flex-col gap-3" {
				p { "Delete " strong class="break-all" "data-text"="$_del_name" {} "?" }
				p class="text-sm text-destructive" role="alert" "data-show"="$_del_error" "data-text"="$_del_error" style="display: none" {}
			}
		},
		&html! {
			form method="dialog" {
				button class=(button_class(Variant::Outline, Size::Md)) autofocus { "Keep it" }
			}
			button type="button" class=(button_class(Variant::Destructive, Size::Md)) "data-on:click"="@post($_del_url)"
				"data-indicator:_deleting" "data-attr:disabled"="$_deleting" { "Delete" }
		},
	)
}

async fn page(State(s): State<AppState>, ctx: TeamCtx) -> Result<Markup, Response> {
	let data = load(&s, &ctx).await.map_err(|e| error::page(&ctx, &e))?;
	let now = OffsetDateTime::now_utc();
	let content = html! {
		div class="ui-stack" "data-signals"=(signals(&data)) {
			(channels_section(&ctx, &data, now))
			(smtp_section(&ctx, &data))
			(rules_section(&ctx, &data))
			(alerts_section(&ctx, &data))
			(noise_section(&ctx))
			(log_section(&ctx, &data, now))
			@if ctx.perms.can(Action::Delete, Resource::Notification) { (delete_dialog()) }
		}
	};
	let body = settings_body(&ctx, &content);
	let crumbs: Vec<Crumb<'_>> = settings_crumbs(&ctx, "Notifications");
	Ok(crate::ui::layout::app_page(
		&ctx,
		"Notifications",
		&crumbs,
		&body,
	))
}

// --- actions ----------------------------------------------------------------------------------

fn done(cookies: &Cookies, team: TeamId, message: &str) -> Response {
	shell::flash(cookies, Tone::Success, message);
	sse::redirect(&page_url(team)).into_response()
}

fn refused(signal: &str, e: &ServerError) -> Response {
	sse::signals(&json!({ signal: error::message(e) })).into_response()
}

#[derive(Deserialize, Default)]
#[serde(default)]
#[allow(
	clippy::struct_field_names,
	reason = "the field names are the page's signal names"
)]
struct ChannelSignals {
	ch_name: String,
	ch_kind: String,
	ch_url: String,
	ch_token: String,
	ch_signing: String,
	ch_chat: String,
	ch_server: String,
	ch_topic: String,
	ch_to: String,
}

async fn create_channel(
	State(s): State<AppState>,
	ctx: TeamCtx,
	cookies: Cookies,
	ReadSignals(sig): ReadSignals<ChannelSignals>,
) -> Response {
	let Some(kind) = ChannelKind::parse(&sig.ch_kind) else {
		return refused(
			"_ch_error",
			&ServerError::Validation("pick a type".to_owned()),
		);
	};
	let fields = ChannelFields {
		url: sig.ch_url,
		token: sig.ch_token,
		signing_secret: sig.ch_signing,
		chat_id: sig.ch_chat,
		api_base: String::new(),
		server: sig.ch_server,
		topic: sig.ch_topic,
		to: sig.ch_to,
	};
	match service::create_channel(&s, ctx.user.id, ctx.team.id, &sig.ch_name, kind, &fields).await {
		Ok(channel) => done(
			&cookies,
			ctx.team.id,
			&format!("Channel {} added.", channel.name),
		),
		Err(e) => refused("_ch_error", &e),
	}
}

async fn test_channel(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, id)): Path<(TeamId, NotificationChannelId)>,
) -> Response {
	match service::test_channel(&s, ctx.user.id, ctx.team.id, id).await {
		Ok(outcome) => {
			let message = if outcome.ok {
				"Last test: sent.".to_owned()
			} else {
				format!("Last test failed: {}", outcome.message)
			};
			sse::patch(&test_line(id, Some(outcome.ok), Some(&message))).into_response()
		}
		Err(e) => {
			sse::patch(&test_line(id, Some(false), Some(&error::message(&e)))).into_response()
		}
	}
}

async fn set_channel(
	s: &AppState,
	ctx: &TeamCtx,
	cookies: &Cookies,
	id: NotificationChannelId,
	on: bool,
) -> Response {
	let edit = ChannelEdit {
		enabled: Some(on),
		..ChannelEdit::default()
	};
	match service::update_channel(s, ctx.user.id, ctx.team.id, id, edit).await {
		Ok(channel) => done(
			cookies,
			ctx.team.id,
			&format!(
				"Channel {} turned {}.",
				channel.name,
				if on { "on" } else { "off" }
			),
		),
		Err(e) => error::action(&e),
	}
}

async fn enable_channel(
	State(s): State<AppState>,
	ctx: TeamCtx,
	cookies: Cookies,
	Path((_, id)): Path<(TeamId, NotificationChannelId)>,
) -> Response {
	set_channel(&s, &ctx, &cookies, id, true).await
}

async fn disable_channel(
	State(s): State<AppState>,
	ctx: TeamCtx,
	cookies: Cookies,
	Path((_, id)): Path<(TeamId, NotificationChannelId)>,
) -> Response {
	set_channel(&s, &ctx, &cookies, id, false).await
}

async fn delete_channel(
	State(s): State<AppState>,
	ctx: TeamCtx,
	cookies: Cookies,
	Path((_, id)): Path<(TeamId, NotificationChannelId)>,
) -> Response {
	match service::remove_channel(&s.db, ctx.user.id, ctx.team.id, id).await {
		Ok(()) => done(&cookies, ctx.team.id, "Channel deleted."),
		Err(e) => refused("_del_error", &e),
	}
}

#[derive(Deserialize, Default)]
#[serde(default)]
#[allow(
	clippy::struct_field_names,
	reason = "the field names are the page's signal names"
)]
struct SmtpSignals {
	smtp_host: String,
	smtp_port: Value,
	smtp_security: String,
	smtp_user: String,
	smtp_password: String,
	smtp_from: String,
}

/// A number the browser may send as text or as a number.
fn number(value: &Value) -> Option<f64> {
	match value {
		Value::Number(n) => n.as_f64(),
		Value::String(s) => s.trim().parse().ok(),
		_ => None,
	}
}

async fn save_smtp(
	State(s): State<AppState>,
	ctx: TeamCtx,
	cookies: Cookies,
	ReadSignals(sig): ReadSignals<SmtpSignals>,
) -> Response {
	let Some(port) =
		number(&sig.smtp_port).filter(|p| p.fract() == 0.0 && (1.0..=65535.0).contains(p))
	else {
		return refused(
			"_smtp_error",
			&ServerError::Validation("invalid SMTP port".to_owned()),
		);
	};
	let input = SmtpInput {
		host: sig.smtp_host,
		#[allow(
			clippy::cast_possible_truncation,
			reason = "checked to be a whole number up to 65535"
		)]
		port: port as i32,
		security: sig.smtp_security,
		username: sig.smtp_user,
		password: sig.smtp_password,
		from_addr: sig.smtp_from,
	};
	match service::save_smtp(&s, ctx.user.id, ctx.team.id, input).await {
		Ok(()) => done(&cookies, ctx.team.id, "Email server saved."),
		Err(e) => refused("_smtp_error", &e),
	}
}

async fn delete_smtp(State(s): State<AppState>, ctx: TeamCtx, cookies: Cookies) -> Response {
	match service::clear_smtp(&s.db, ctx.user.id, ctx.team.id).await {
		Ok(()) => done(&cookies, ctx.team.id, "Email server removed."),
		Err(e) => error::action(&e),
	}
}

fn flag(map: &HashMap<String, Value>, key: &str) -> bool {
	map.get(key).and_then(Value::as_bool).unwrap_or(false)
}

fn text(map: &HashMap<String, Value>, key: &str) -> String {
	map.get(key)
		.and_then(Value::as_str)
		.unwrap_or_default()
		.to_owned()
}

/// `p:<project>` or `e:<project>:<environment>` from the scope select; empty for the whole team.
fn parse_scope(raw: &str) -> Result<(Option<i64>, Option<i64>), ServerError> {
	let bad = || ServerError::Validation("invalid scope".to_owned());
	if raw.is_empty() {
		return Ok((None, None));
	}
	let mut parts = raw.split(':');
	match (parts.next(), parts.next(), parts.next(), parts.next()) {
		(Some("p"), Some(p), None, None) => Ok((Some(p.parse().map_err(|_| bad())?), None)),
		(Some("e"), Some(p), Some(e), None) => Ok((
			Some(p.parse().map_err(|_| bad())?),
			Some(e.parse().map_err(|_| bad())?),
		)),
		_ => Err(bad()),
	}
}

async fn create_rule(
	State(s): State<AppState>,
	ctx: TeamCtx,
	cookies: Cookies,
	ReadSignals(sig): ReadSignals<HashMap<String, Value>>,
) -> Response {
	let kinds = EventKind::ALL
		.into_iter()
		.filter(|k| flag(&sig, &format!("ev_{}", k.as_str())))
		.map(|k| k.as_str().to_owned())
		.collect();
	let channel_ids = sig
		.iter()
		.filter(|(_, v)| v.as_bool() == Some(true))
		.filter_map(|(k, _)| k.strip_prefix("rch_")?.parse::<i64>().ok())
		.collect();
	let (project_id, environment_id) = match parse_scope(&text(&sig, "rule_scope")) {
		Ok(scope) => scope,
		Err(e) => return refused("_rule_error", &e),
	};
	let input = RuleInput {
		name: text(&sig, "rule_name"),
		kinds,
		channel_ids,
		project_id,
		environment_id,
		min_severity: Severity::parse(&text(&sig, "rule_min")).unwrap_or(Severity::Info),
		enabled: true,
	};
	match service::create_rule(&s.db, ctx.user.id, ctx.team.id, input).await {
		Ok(_) => done(&cookies, ctx.team.id, "Rule added."),
		Err(e) => refused("_rule_error", &e),
	}
}

async fn set_rule(
	s: &AppState,
	ctx: &TeamCtx,
	cookies: &Cookies,
	id: NotificationRuleId,
	on: bool,
) -> Response {
	match service::set_rule_enabled(&s.db, ctx.user.id, ctx.team.id, id, on).await {
		Ok(()) => done(
			cookies,
			ctx.team.id,
			if on {
				"Rule turned on."
			} else {
				"Rule turned off."
			},
		),
		Err(e) => error::action(&e),
	}
}

async fn enable_rule(
	State(s): State<AppState>,
	ctx: TeamCtx,
	cookies: Cookies,
	Path((_, id)): Path<(TeamId, NotificationRuleId)>,
) -> Response {
	set_rule(&s, &ctx, &cookies, id, true).await
}

async fn disable_rule(
	State(s): State<AppState>,
	ctx: TeamCtx,
	cookies: Cookies,
	Path((_, id)): Path<(TeamId, NotificationRuleId)>,
) -> Response {
	set_rule(&s, &ctx, &cookies, id, false).await
}

async fn delete_rule(
	State(s): State<AppState>,
	ctx: TeamCtx,
	cookies: Cookies,
	Path((_, id)): Path<(TeamId, NotificationRuleId)>,
) -> Response {
	match service::remove_rule(&s.db, ctx.user.id, ctx.team.id, id).await {
		Ok(()) => done(&cookies, ctx.team.id, "Rule deleted."),
		Err(e) => refused("_del_error", &e),
	}
}

async fn create_alert(
	State(s): State<AppState>,
	ctx: TeamCtx,
	cookies: Cookies,
	ReadSignals(sig): ReadSignals<HashMap<String, Value>>,
) -> Response {
	let target = text(&sig, "al_target");
	let Some((kind, id)) = target
		.split_once(':')
		.and_then(|(k, i)| Some((k, i.parse::<i64>().ok()?)))
	else {
		return refused(
			"_al_error",
			&ServerError::Validation("choose what to watch".to_owned()),
		);
	};
	let (Some(threshold), Some(minutes)) = (
		sig.get("al_threshold").and_then(number),
		sig.get("al_minutes").and_then(number),
	) else {
		return refused(
			"_al_error",
			&ServerError::Validation("invalid threshold or duration".to_owned()),
		);
	};
	let server = kind == "server";
	let input = AlertInput {
		name: text(&sig, "al_name"),
		scope: if server { "server" } else { "resource" }.to_owned(),
		server_id: server.then_some(id),
		resource_kind: (!server).then(|| kind.to_owned()),
		resource_id: (!server).then_some(id),
		metric: text(&sig, "al_metric"),
		threshold,
		#[allow(
			clippy::cast_possible_truncation,
			reason = "validated to 1..=1440 by the service; a huge value saturates"
		)]
		duration_minutes: minutes as i32,
		severity: Severity::parse(&text(&sig, "al_severity")).unwrap_or(Severity::Warning),
		enabled: true,
	};
	match service::create_alert(&s.db, ctx.user.id, ctx.team.id, input).await {
		Ok(_) => done(&cookies, ctx.team.id, "Metric alert added."),
		Err(e) => refused("_al_error", &e),
	}
}

async fn set_alert(
	s: &AppState,
	ctx: &TeamCtx,
	cookies: &Cookies,
	id: MetricAlertId,
	on: bool,
) -> Response {
	match service::set_alert_enabled(&s.db, ctx.user.id, ctx.team.id, id, on).await {
		Ok(()) => done(
			cookies,
			ctx.team.id,
			if on {
				"Alert turned on."
			} else {
				"Alert turned off."
			},
		),
		Err(e) => error::action(&e),
	}
}

async fn enable_alert(
	State(s): State<AppState>,
	ctx: TeamCtx,
	cookies: Cookies,
	Path((_, id)): Path<(TeamId, MetricAlertId)>,
) -> Response {
	set_alert(&s, &ctx, &cookies, id, true).await
}

async fn disable_alert(
	State(s): State<AppState>,
	ctx: TeamCtx,
	cookies: Cookies,
	Path((_, id)): Path<(TeamId, MetricAlertId)>,
) -> Response {
	set_alert(&s, &ctx, &cookies, id, false).await
}

async fn delete_alert(
	State(s): State<AppState>,
	ctx: TeamCtx,
	cookies: Cookies,
	Path((_, id)): Path<(TeamId, MetricAlertId)>,
) -> Response {
	match service::remove_alert(&s.db, ctx.user.id, ctx.team.id, id).await {
		Ok(()) => done(&cookies, ctx.team.id, "Metric alert deleted."),
		Err(e) => refused("_del_error", &e),
	}
}

/// `HH:MM` from a time input, as minutes after midnight.
fn parse_time(raw: &str) -> Option<i32> {
	let (h, m) = raw.trim().split_once(':')?;
	let (h, m): (i32, i32) = (h.parse().ok()?, m.get(..2)?.parse().ok()?);
	((0..24).contains(&h) && (0..60).contains(&m)).then_some(h * 60 + m)
}

async fn save_noise(
	State(s): State<AppState>,
	ctx: TeamCtx,
	cookies: Cookies,
	ReadSignals(sig): ReadSignals<HashMap<String, Value>>,
) -> Response {
	let invalid = || {
		refused(
			"_nz_error",
			&ServerError::Validation("invalid noise control settings".to_owned()),
		)
	};
	let (Some(dedup), Some(offset), Some(start), Some(end)) = (
		sig.get("nz_dedup").and_then(number),
		sig.get("nz_offset").and_then(number),
		parse_time(&text(&sig, "nz_start")),
		parse_time(&text(&sig, "nz_end")),
	) else {
		return invalid();
	};
	if !dedup.is_finite() || !offset.is_finite() {
		return invalid();
	}
	#[allow(
		clippy::cast_possible_truncation,
		reason = "the service rejects anything out of range; a huge value saturates"
	)]
	let new = Settings {
		dedup_minutes: dedup as i32,
		quiet_enabled: flag(&sig, "nz_quiet"),
		quiet_start_min: start,
		quiet_end_min: end,
		utc_offset_min: (offset * 60.0).round() as i32,
	};
	match service::save_settings(&s.db, ctx.user.id, ctx.team.id, &new).await {
		Ok(()) => done(&cookies, ctx.team.id, "Noise control saved."),
		Err(e) => refused("_nz_error", &e),
	}
}

async fn retry_delivery(
	State(s): State<AppState>,
	ctx: TeamCtx,
	cookies: Cookies,
	Path((_, id)): Path<(TeamId, NotificationDeliveryId)>,
) -> Response {
	match service::retry_delivery(&s, ctx.user.id, ctx.team.id, id).await {
		Ok(()) => done(&cookies, ctx.team.id, "Queued again."),
		Err(e) => error::action(&e),
	}
}
