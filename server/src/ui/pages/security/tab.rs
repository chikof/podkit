//! The Security tab of one resource: the latest scan, a findings table (narrowed by links, so
//! it needs no script and a filtered view can be shared), the ignores the team has set, the
//! SBOM download and the ignore dialog.
//!
//! The page frame belongs to the resource's own area; [`content`] returns what goes inside it.
//! A stack has one scan per service, picked with `?scan=`.

use axum::response::Response;
use maud::{Markup, html};
use serde::Deserialize;

use super::{slug, when};
use crate::auth::permission::{Action, Resource};
use crate::ids::ProjectId;
use crate::scanning::service::{self, Entry, ScanView, SubjectScans};
use crate::scanning::store::{FindingFilter, StoredFinding, StoredIgnore};
use crate::scanning::{BadgeState, ScanStatus, Severity, Subject};
use crate::state::AppState;
use crate::ui::components::{
	BUTTON, BUTTON_OUTLINE, BannerKind, INPUT, LABEL, SELECT, Size, Variant, act_now_marker,
	banner, button_class, card, card_content, card_section, chip, chip_group, dialog,
	dialog_cancel, empty_state, icon, link_button, security_badge, severity_chip, table,
};
use crate::ui::error;
use crate::ui::extract::TeamCtx;
use crate::ui::sse::js_string;
use crate::ui::status::poll;

/// The most findings one page draws; the filters narrow a longer list.
const SHOWN: usize = 200;

/// What the tab's links and filter chips carry in the query string.
#[derive(Debug, Default, Clone, Deserialize)]
#[serde(default)]
pub struct TabQuery {
	scan: Option<i64>,
	severity: Option<String>,
	act_now: bool,
	fixable: bool,
	ignored: bool,
}

impl TabQuery {
	fn severity(&self) -> Option<Severity> {
		self.severity.as_deref().and_then(Severity::parse)
	}

	/// Whether any filter is chosen, so a list that came up empty says why.
	fn narrowed(&self) -> bool {
		self.severity().is_some() || self.act_now || self.fixable || self.ignored
	}

	fn filter(&self) -> FindingFilter {
		FindingFilter {
			severity: self.severity(),
			act_now: self.act_now,
			fixable: self.fixable,
			show_ignored: self.ignored,
		}
	}

	/// The tab's address with this query, for a chip.
	fn href(&self, page: &str) -> String {
		let mut parts = Vec::new();
		if let Some(scan) = self.scan {
			parts.push(format!("scan={scan}"));
		}
		if let Some(severity) = self.severity() {
			parts.push(format!("severity={}", severity.as_str()));
		}
		for (on, key) in [
			(self.act_now, "act_now"),
			(self.fixable, "fixable"),
			(self.ignored, "ignored"),
		] {
			if on {
				parts.push(format!("{key}=true"));
			}
		}
		join_query(page, &parts)
	}

	fn with(&self, change: impl FnOnce(&mut Self)) -> Self {
		let mut next = self.clone();
		change(&mut next);
		next
	}
}

/// `page` with `parts` as its query; a page that has one already (`?tab=security`) gets `&`.
fn join_query(page: &str, parts: &[String]) -> String {
	if parts.is_empty() {
		return page.to_owned();
	}
	let sep = if page.contains('?') { '&' } else { '?' };
	format!("{page}{sep}{}", parts.join("&"))
}

fn entry_label(entry: &Entry, index: usize) -> String {
	entry
		.label
		.clone()
		.unwrap_or_else(|| format!("Image {}", index + 1))
}

/// The tab's content for `subject`. `page` is the tab's own address, which the filter chips
/// extend.
///
/// # Errors
/// The error page of whatever the service refused: no `read scan` is a 403, a resource of
/// another team a 404.
pub async fn content(
	s: &AppState,
	ctx: &TeamCtx,
	project: ProjectId,
	subject: Subject,
	page: &str,
	query: &TabQuery,
) -> Result<Markup, Response> {
	let fail = |e: crate::error::ServerError| error::page(ctx, &e);
	let found = service::subject_scans(&s.db, ctx.user.id, ctx.team.id, project, subject)
		.await
		.map_err(fail)?;
	let ignores = service::ignores(&s.db, ctx.user.id, ctx.team.id)
		.await
		.map_err(fail)?;

	// The scan being read: the one asked for, else the first that exists.
	let picked = found
		.entries
		.iter()
		.position(|e| {
			query
				.scan
				.is_some_and(|id| e.view.as_ref().is_some_and(|v| v.scan.id.0 == id))
		})
		.or_else(|| found.entries.iter().position(|e| e.view.is_some()));
	let Some(picked) = picked else {
		return Ok(html! { div class="ui-stack" { (not_scanned(ctx, &found)) } });
	};
	let view = found.entries[picked]
		.view
		.clone()
		.unwrap_or_else(|| unreachable!());
	let (view, list) = service::findings(
		&s.db,
		ctx.user.id,
		ctx.team.id,
		view.scan.id,
		query.filter(),
	)
	.await
	.map_err(fail)?;

	Ok(html! {
		div class="ui-stack" data-signals="{ig_vuln: '', ig_pkg: '', ig_scope: 'all', ig_reason: '', ig_days: '30', _ig_error: '', _busy: false}" {
			(entries_row(&found, picked, page, query))
			@if !found.scanning_on {
				(banner(
					BannerKind::Info,
					Some("Scanning is off for this server."),
					"Earlier results stay, and nothing new is scanned.",
					None,
				))
			}
			(scan_card(ctx, project, subject, &found, &view))
			@if view.scan.status == ScanStatus::Succeeded {
				(findings_section(ctx, &view, &list, page, query))
			}
			(ignores_section(ctx, &ignores))
			@if ctx.perms.can(Action::Update, Resource::Scan) { (ignore_dialog(ctx)) }
		}
	})
}

/// What a resource says before its first scan, or with scanning off.
fn not_scanned(ctx: &TeamCtx, found: &SubjectScans) -> Markup {
	if found.scanning_on {
		empty_state(
			"shield",
			"Not scanned yet",
			Some(
				"Each deploy scans the image it runs, and the result shows up here. Nothing waits for it.",
			),
			None,
		)
	} else {
		let action = ctx.perms.can(Action::Read, Resource::Server).then(|| {
			link_button(
				Variant::Outline,
				Size::Md,
				&format!("/teams/{}/servers", ctx.team.id),
				&html! { "Open servers" },
			)
		});
		empty_state(
			"shield",
			"Scanning is off for this server",
			Some("Turn image scanning on in the server's settings to see what its images contain."),
			action.as_ref(),
		)
	}
}

/// A stack's services, one chip each. Nothing for a resource with one image.
fn entries_row(found: &SubjectScans, picked: usize, page: &str, query: &TabQuery) -> Markup {
	let with_scan: Vec<(usize, &Entry)> = found
		.entries
		.iter()
		.enumerate()
		.filter(|(_, e)| e.view.is_some())
		.collect();
	if with_scan.len() < 2 {
		return html! {};
	}
	let chips = html! {
		@for (i, entry) in with_scan {
			@if let Some(view) = &entry.view {
				(chip(
					&entry_label(entry, i),
					&query.with(|q| { q.scan = Some(view.scan.id.0); }).href(page),
					i == picked,
					Some(&view.scan.image_ref),
				))
			}
		}
	};
	chip_group("Service", &chips)
}

fn digest(d: &str) -> &str {
	let end = d.char_indices().nth(19).map_or(d.len(), |(i, _)| i);
	&d[..end]
}

/// The scan itself: where it stands, what it looked at, and the actions on it.
fn scan_card(
	ctx: &TeamCtx,
	project: ProjectId,
	subject: Subject,
	found: &SubjectScans,
	view: &ScanView,
) -> Markup {
	let scan = &view.scan;
	let can_scan = ctx.perms.can(Action::Create, Resource::Scan);
	let (kind, id) = slug(subject);
	let again = format!(
		"@post({})",
		js_string(&format!(
			"/teams/{}/projects/{project}/security/{kind}/{id}/scan",
			ctx.team.id
		))
	);
	let sbom = format!("/teams/{}/security/scans/{}/sbom", ctx.team.id, scan.id);
	let action = html! {
		div class="flex flex-wrap items-center gap-2" {
			@if view.has_sbom {
				a class=(BUTTON_OUTLINE) href=(sbom) download { (icon("download")) "SBOM" }
			}
			@if can_scan && found.scanning_on && !scan.status.in_progress() {
				button type="button" class=(BUTTON_OUTLINE) "data-on:click"=(again) "data-indicator:_busy" "data-attr:disabled"="$_busy" {
					(icon("refresh-cw")) "Scan again"
				}
			}
		}
	};
	html! {
		(card(&html! {
			(card_section("Latest scan", Some("Informational only. A finding never stops a deploy."), Some(&action)))
			(card_content(&html! {
				div id=(format!("scan-{}", scan.id)) class="ui-stack" {
					@if scan.status.in_progress() {
						(poll(&format!("/teams/{}/security/scans/{}/status", ctx.team.id, scan.id), true))
						p class="ui-meta" role="status" { (icon("loader-circle")) "Scanning image…" }
					} @else if scan.status == ScanStatus::Failed {
						(banner(
							BannerKind::Error,
							Some("The scan failed."),
							scan.error.as_deref().unwrap_or("No reason was recorded."),
							None,
						))
					} @else {
						div class="ui-meta" {
							(security_badge(&view.badge, None))
							span class="ui-lede" { (view.counts.summary()) }
						}
					}
					dl class="ui-facts ui-facts-row ui-scan-facts" aria-label="Scan details" {
						(fact("Image", &html! { code class="break-all" { (scan.image_ref) } }))
						@if !scan.image_digest.is_empty() {
							(fact("Digest", &html! { code title=(scan.image_digest) { (digest(&scan.image_digest)) } }))
						}
						(fact("Scanned", &html! {
							@if let Some(t) = scan.rescanned_at.or(scan.finished_at) { (when(t)) } @else { "\u{2014}" }
						}))
						@if let Some(built) = &scan.db_built {
							(fact("Vulnerability data", &html! { (built) }))
						}
					}
				}
			}))
		}))
	}
}

fn fact(label: &str, value: &Markup) -> Markup {
	html! {
		div {
			dt { (label) }
			dd { (value) }
		}
	}
}

fn filters(page: &str, query: &TabQuery, view: &ScanView) -> Markup {
	let level = |label: &str, severity: Option<Severity>| {
		let count = severity.map_or_else(|| view.counts.total(), |s| view.counts.of_severity(s));
		let text = format!("{label} {count}");
		chip(
			&text,
			&query
				.with(|q| q.severity = severity.map(|s| s.as_str().to_owned()))
				.href(page),
			query.severity() == severity,
			None,
		)
	};
	let toggle = |label: &str, on: bool, set: fn(&mut TabQuery, bool)| {
		chip(label, &query.with(|q| set(q, !on)).href(page), on, None)
	};
	html! {
		div class="flex flex-col gap-2" {
			(chip_group("Severity", &html! {
				(level("All", None))
				(level("Critical", Some(Severity::Critical)))
				(level("High", Some(Severity::High)))
				(level("Medium", Some(Severity::Medium)))
				(level("Low", Some(Severity::Low)))
			}))
			(chip_group("Show", &html! {
				(toggle("Act now", query.act_now, |q, v| q.act_now = v))
				(toggle("Fix available", query.fixable, |q, v| q.fixable = v))
				(toggle("Ignored", query.ignored, |q, v| q.ignored = v))
			}))
		}
	}
}

/// Only an http(s) link leaves the page: the scanner's data is not trusted to pick a scheme.
fn safe_url(url: &str) -> Option<&str> {
	(url.starts_with("https://") || url.starts_with("http://")).then_some(url)
}

fn score(f: &StoredFinding) -> Markup {
	html! {
		@if let Some(cvss) = f.cvss { span { "CVSS " (format!("{cvss:.1}")) } }
		@if let Some(epss) = f.epss {
			@if f.cvss.is_some() { " \u{b7} " }
			span { "EPSS " (format!("{:.1}%", epss * 100.0)) }
		}
		@if f.kev {
			@if f.cvss.is_some() || f.epss.is_some() { " \u{b7} " }
			span { "Known exploited" }
		}
		@if f.cvss.is_none() && f.epss.is_none() && !f.kev { "\u{2014}" }
	}
}

fn ignore_button(f: &StoredFinding) -> Markup {
	let open = format!(
		"$ig_vuln = {}; $ig_pkg = {}; $ig_scope = 'all'; $ig_reason = ''; $ig_days = '30'; $_ig_error = ''; el.commandForElement || document.getElementById('ignore-dialog').showModal()",
		js_string(&f.vuln_id),
		js_string(&f.package)
	);
	html! {
		button type="button" class=(button_class(Variant::Outline, Size::Sm)) command="show-modal" commandfor="ignore-dialog"
			aria-haspopup="dialog" aria-label={ "Ignore " (f.vuln_id) " in " (f.package) } "data-on:click"=(open) { "Ignore" }
	}
}

fn finding_row(f: &StoredFinding, can_ignore: bool) -> Vec<Markup> {
	let severity = Severity::parse(&f.severity).unwrap_or(Severity::Unknown);
	vec![
		html! {
			span class="inline-flex flex-wrap items-center gap-2" {
				@if let Some(url) = f.url.as_deref().and_then(safe_url) {
					a class="font-medium underline underline-offset-4" href=(url) target="_blank" rel="noopener noreferrer" { (f.vuln_id) }
				} @else {
					span class="font-medium" { (f.vuln_id) }
				}
				@if f.act_now { (act_now_marker("Listed as actively exploited, or likely to be and severe")) }
			}
		},
		severity_chip(severity),
		html! { code { (f.package) } " " span class="text-muted-foreground" { (f.version) } },
		html! { @if let Some(fixed) = &f.fixed_version { code { (fixed) } } @else { span class="text-muted-foreground" { "No fix yet" } } },
		score(f),
		html! {
			@if f.ignored {
				span class="text-sm text-muted-foreground" { "Ignored" }
			} @else if can_ignore {
				(ignore_button(f))
			}
		},
	]
}

fn findings_section(
	ctx: &TeamCtx,
	view: &ScanView,
	list: &[StoredFinding],
	page: &str,
	query: &TabQuery,
) -> Markup {
	let can_ignore = ctx.perms.can(Action::Update, Resource::Scan);
	let clean = view.badge.state == BadgeState::Clean;
	let rows: Vec<Vec<Markup>> = list
		.iter()
		.take(SHOWN)
		.map(|f| finding_row(f, can_ignore))
		.collect();
	html! {
		section class="ui-stack" aria-labelledby="findings-title" {
			h2 id="findings-title" class="ui-sec-h" { "Findings" }
			@if !(clean && list.is_empty() && !query.narrowed()) {
				(filters(page, query, view))
			}
			@if list.is_empty() {
				(empty_state(
					if clean { "circle-check" } else { "search" },
					if clean { "No known vulnerabilities" } else { "Nothing matches" },
					Some(if clean {
						"The scan found nothing in this image. It is checked again every day."
					} else {
						"No finding fits the filters, or every one that does is ignored."
					}),
					None,
				))
			} @else {
				p class="ui-lede" aria-live="polite" {
					(list.len()) @if list.len() == 1 { " finding" } @else { " findings" }
					@if list.len() > SHOWN { ", the first " (SHOWN) " shown. Narrow them with the filters." }
				}
				(table(
					"findings",
					"Findings of the latest scan",
					&["Vulnerability", "Severity", "Package", "Fixed in", "Score", ""],
					&rows,
				))
			}
		}
	}
}

fn ignore_row(ctx: &TeamCtx, i: &StoredIgnore, can_remove: bool) -> Vec<Markup> {
	let remove = format!(
		"@post({})",
		js_string(&format!(
			"/teams/{}/security/ignores/{}/remove",
			ctx.team.id, i.id
		))
	);
	vec![
		html! { span class="font-medium" { (i.vuln_id) } },
		html! { @if let Some(p) = &i.package { code { (p) } } @else { "Every package" } },
		html! { (i.reason) },
		html! { (when(i.expires_at)) },
		html! { (i.created_by_name.as_deref().unwrap_or("\u{2014}")) },
		html! {
			@if can_remove {
				button type="button" class=(button_class(Variant::Ghost, Size::Sm)) "data-on:click"=(remove)
					aria-label={ "Stop ignoring " (i.vuln_id) } { "Remove" }
			}
		},
	]
}

fn ignores_section(ctx: &TeamCtx, ignores: &[StoredIgnore]) -> Markup {
	let now = time::OffsetDateTime::now_utc();
	let active: Vec<&StoredIgnore> = ignores.iter().filter(|i| i.expires_at > now).collect();
	if active.is_empty() {
		return html! {};
	}
	let can_remove = ctx.perms.can(Action::Delete, Resource::Scan);
	let rows: Vec<Vec<Markup>> = active
		.into_iter()
		.map(|i| ignore_row(ctx, i, can_remove))
		.collect();
	html! {
		section class="ui-stack" aria-labelledby="ignores-title" {
			h2 id="ignores-title" class="ui-sec-h" { "Ignored findings" }
			p class="ui-lede" { "Ignored for the whole team until they expire, then they count again." }
			(table(
				"ignores",
				"Findings the team ignores",
				&["Vulnerability", "Applies to", "Reason", "Expires", "Added by", ""],
				&rows,
			))
		}
	}
}

/// The one dialog every Ignore button opens; the button fills its signals first.
fn ignore_dialog(ctx: &TeamCtx) -> Markup {
	let post = format!(
		"@post({})",
		js_string(&format!("/teams/{}/security/ignores", ctx.team.id))
	);
	html! {
		(dialog(
			"ignore-dialog",
			"Ignore this finding?",
			Some("It stops counting in the badge and the lists until it expires. Say why, so the next person knows."),
			&html! {
				div class="flex flex-col gap-3" {
					p class="text-sm" { "Ignoring " strong class="font-mono break-all" "data-text"="$ig_vuln" {} }
					label class=(LABEL) {
						"Applies to"
						div class="ui-select-wrap" {
							select class=(SELECT) "data-bind:ig_scope" {
								button { selectedcontent {} }
								option value="all" { "Every package" }
								option value="package" { "Only this package" }
							}
							(icon("chevron-down"))
						}
						span class="text-xs font-normal text-muted-foreground" "data-show"="$ig_scope === 'package'" style="display: none" {
							"Only " code "data-text"="$ig_pkg" {}
						}
					}
					label class=(LABEL) {
						"Reason"
						input class=(INPUT) type="text" required maxlength="500" autocomplete="off" autofocus
							placeholder="Not reachable, fixed upstream next week…" "data-bind:ig_reason";
					}
					label class=(LABEL) {
						"Ignore for"
						div class="ui-select-wrap" {
							select class=(SELECT) "data-bind:ig_days" {
								button { selectedcontent {} }
								option value="7" { "7 days" }
								option value="30" { "30 days" }
								option value="90" { "90 days" }
								option value="365" { "1 year" }
							}
							(icon("chevron-down"))
						}
					}
					p class="text-sm text-destructive" role="alert" "data-show"="$_ig_error" "data-text"="$_ig_error" style="display: none" {}
				}
			},
			&html! {
				(dialog_cancel(BUTTON_OUTLINE, &html! { "Cancel" }))
				button type="button" class=(BUTTON) "data-attr:disabled"="$_busy || !$ig_reason.trim()"
					"data-indicator:_busy" "data-on:click"=(post) { "Ignore" }
			},
		))
	}
}
