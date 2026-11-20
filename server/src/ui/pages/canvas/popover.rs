//! The confirm popover of a wire (and of unlinking one): what exactly will change, in the order
//! it will run. The server builds it from the same [`Plan`] the POST then runs, so what it
//! promises is what happens. A secret's value is never here, only that one is written.

use maud::{Markup, html};

use super::view::{Cx, cv_icon};
use crate::projects::graph::rules::{Intent, Plan, Step};
use crate::projects::graph::{Graph, NodeKey};
use crate::ui::components::{BUTTON, BUTTON_DANGER, BUTTON_OUTLINE, SELECT};
use crate::ui::layout::form_error;
use crate::ui::sse::js_string;

/// What the popover is about.
pub enum Mode {
	Wire,
	Unlink,
}

fn name(graph: &Graph, key: NodeKey) -> &str {
	graph.node(key).map_or("this resource", |n| n.name.as_str())
}

fn redeploys(plan: &Plan) -> Vec<NodeKey> {
	plan.steps
		.iter()
		.filter_map(|s| match s {
			Step::Redeploy { node } => Some(*node),
			_ => None,
		})
		.collect()
}

/// The primary button's label.
fn primary(plan: &Plan, mode: &Mode) -> &'static str {
	let redeploy = !redeploys(plan).is_empty();
	match (mode, &plan.intent, redeploy) {
		(Mode::Wire, Intent::Variable { .. }, true) => "Connect and redeploy",
		(Mode::Wire, Intent::Variable { .. }, false) => "Connect",
		(Mode::Wire, Intent::Gateway { .. }, true) => "Link and redeploy",
		(Mode::Wire, Intent::Gateway { .. }, false) => "Link",
		(Mode::Wire, Intent::Network { .. }, true) => "Join network and redeploy",
		(Mode::Wire, Intent::Network { .. }, false) => "Join network",
		(Mode::Unlink, _, true) => "Unlink and redeploy",
		(Mode::Unlink, _, false) => "Unlink",
	}
}

/// The title, the one-line lead and the exact list of changes.
fn text(graph: &Graph, plan: &Plan, mode: &Mode) -> (String, Markup, Vec<(&'static str, Markup)>) {
	let (from, to) = plan.ends();
	let (a, b) = (name(graph, from), name(graph, to));
	let redeploy = redeploys(plan);
	let mut rows: Vec<(&'static str, Markup)> = Vec::new();
	let unlink = matches!(mode, Mode::Unlink);
	let title = match (&plan.intent, unlink) {
		(Intent::Network { .. }, false) => format!("Put {a} and {b} on one network"),
		(Intent::Gateway { .. }, false) => format!("Link {a} to {b}"),
		(_, false) => format!("Connect {a} to {b}"),
		(_, true) => format!("Unlink {a} from {b}"),
	};
	for step in &plan.steps {
		match step {
			Step::JoinNetwork { node, network } => rows.push((
				"Network",
				html! { "Add " code { (name(graph, *node)) } " to " code { (network) } },
			)),
			Step::LeaveNetwork { node, network } => rows.push((
				"Network",
				html! { "Take " code { (name(graph, *node)) } " off " code { (network) } },
			)),
			Step::SetVariable { node, key } => rows.push((
				"Env var",
				html! {
					"Inject " code { (key) } " into " code { (name(graph, *node)) }
					small { "Secret, write-only. The value is never shown." }
				},
			)),
			Step::RemoveVariable { node, key } => rows.push((
				"Env var",
				html! { "Remove " code { (key) } " from " code { (name(graph, *node)) } },
			)),
			Step::LinkModel { app, model } => {
				rows.push((
					"Gateway",
					html! { code { (name(graph, *app)) } " calls " code { (name(graph, *model)) } " through the podkit gateway" },
				));
				rows.push((
					"Env vars",
					html! {
						"At its next deploy " code { (name(graph, *app)) } " gets the gateway variables, such as " code { "OPENAI_BASE_URL" }
						small { "A new key scoped to " (name(graph, *model)) ". Write-only." }
					},
				));
			}
			Step::UnlinkModel { app, model } => rows.push((
				"Gateway",
				html! { "Stop routing " code { (name(graph, *app)) } " to " code { (name(graph, *model)) } },
			)),
			Step::Redeploy { node } => rows.push((
				"Redeploy",
				html! { "Rolling redeploy of " code { (name(graph, *node)) } },
			)),
		}
	}
	if redeploy.is_empty() && !unlink && !matches!(plan.intent, Intent::Network { .. }) {
		rows.push((
			"Redeploy",
			html! { code { (a) } " has not been deployed yet. The change applies on its first deploy." },
		));
	}
	let lead = match (&plan.intent, unlink) {
		(Intent::Variable { .. }, false) => html! {
			"Inject the connection variable into " code { (a) }
			@if redeploy.is_empty() { "?" } @else { " and redeploy it?" }
		},
		(Intent::Gateway { .. }, false) => html! {
			"Route " code { (a) } " through the model gateway to " code { (b) }
			@if redeploy.is_empty() { "?" } @else { " and redeploy it?" }
		},
		(Intent::Network { .. }, false) => html! { "Put both on one network?" },
		(_, true) => html! {
			"Remove the link between " code { (a) } " and " code { (b) } "."
			@if !redeploy.is_empty() { " The running container keeps its variables until it is redeployed." }
		},
	};
	(title, lead, rows)
}

/// The popover element, ready to patch into `#cv-pop`.
///
/// With `pick`, it also carries a select of everything `from` may be wired to (the list
/// view's way in, for a screen reader, a phone or anyone who cannot drag).
pub fn popover(
	c: &Cx<'_>,
	mode: &Mode,
	from: NodeKey,
	to: Option<NodeKey>,
	plan: Option<&Plan>,
	pick: bool,
) -> Markup {
	let unlink = matches!(mode, Mode::Unlink);
	let (title, lead, rows) = plan.map_or_else(
		|| {
			(
				format!("Connect {}", name(c.graph, from)),
				html! { "Pick what to connect it to." },
				Vec::new(),
			)
		},
		|p| text(c.graph, p, mode),
	);
	let post = format!(
		"evt.preventDefault(); @post({})",
		js_string(&c.url(if unlink { "/unlink" } else { "/wire" }))
	);
	let change = format!(
		"$cvTo = el.value; @get({})",
		js_string(&c.url("/wire/preview"))
	);
	let targets = crate::projects::graph::rules::targets(
		c.graph,
		c.graph.node(from).expect("the source is in the graph"),
	);
	html! {
		div id="cv-pop" class="ui-cv-pop" popover role="dialog" aria-labelledby="cv-pop-t" aria-describedby="cv-pop-d" {
			form "data-on:submit"=(post) {
				div class="ui-dialog-head" {
					span class={ "ui-dialog-ic" @if unlink { " ui-dialog-ic-err" } } { (if unlink { cv_icon("unlink") } else { cv_icon("plug") }) }
					div {
						h2 class="ui-dialog-title" id="cv-pop-t" { (title) }
						p class="ui-dialog-desc" id="cv-pop-d" { (lead) }
					}
				}
				div class="ui-dialog-body" {
					@if pick {
						label class="ui-cv-pick" {
							span class="text-sm font-medium" { "Connect to" }
							select class=(SELECT) "data-on:change"=(change) {
								button { selectedcontent {} }
								@for key in &targets {
									option value=(key) selected[Some(*key) == to] { (name(c.graph, *key)) }
								}
							}
						}
					}
					@if !rows.is_empty() {
						ul class="ui-cv-change" {
							@for (label, body) in &rows { li { b { (label) } span { (body) } } }
						}
						p class="ui-cv-note" { "Nothing changes until you confirm. Secret values stay write-only." }
					}
					(form_error())
				}
				div class="ui-dialog-foot" {
					button type="button" class=(BUTTON_OUTLINE) popovertarget="cv-pop" popovertargetaction="hide" autofocus[unlink] { "Cancel" }
					@if let Some(plan) = plan {
						button type="submit" class=(if unlink { BUTTON_DANGER } else { BUTTON }) autofocus[!unlink]
							"data-indicator:_busy" "data-attr:disabled"="$_busy" { (primary(plan, mode)) }
					}
				}
			}
		}
	}
}
