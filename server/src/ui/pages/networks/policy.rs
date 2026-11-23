//! The network policy view of the Networks page and the "bypasses the proxy" warnings.
//!
//! The policy is read off the network memberships: everything on one network reaches
//! everything else on it by name, and nothing reaches across networks. Beside each resource
//! is how it can be reached from outside the server. Nothing here reads the database:
//! `access_rules::policy` hands over a [`Policy`].

use maud::{Markup, html};

use crate::access_rules::policy::{Exposure, NetworkPolicy, Policy, PortWarning};
use crate::ids::TeamId;
use crate::networks::ResourceKind;
use crate::ui::components::{BannerKind, Tone, badge, banner, card, card_content, card_section};

/// Where the resource's own page is, which is where its public port is changed.
fn resource_url(team: TeamId, kind: ResourceKind, project: i64, id: i64) -> String {
	let base = format!("/teams/{team}/projects/{project}");
	match kind {
		ResourceKind::Application => format!("{base}/apps/{id}"),
		ResourceKind::Database => format!("{base}/databases/{id}"),
		ResourceKind::Model => format!("{base}/models/{id}"),
		ResourceKind::Stack => format!("{base}/stacks/{id}"),
	}
}

fn exposure_badge(exposure: Exposure) -> Markup {
	match exposure {
		Exposure::Public { restricted: true } => badge(Tone::Success, "Public, with rules"),
		Exposure::Public { restricted: false } => badge(Tone::Neutral, "Public"),
		Exposure::InternalOnly => badge(Tone::Success, "Internal only"),
		Exposure::PublishedPort(port) => badge(Tone::Warning, &format!("Port {port} open")),
		Exposure::Private => badge(Tone::Neutral, "Private"),
	}
}

/// One "bypasses the proxy" warning, with the link to the fix.
fn warning(team: TeamId, w: &PortWarning) -> Markup {
	let fix = resource_url(team, w.kind, w.project_id, w.id);
	let action = html! {
		a class="whitespace-nowrap text-sm font-medium hover:underline" href=(fix) { "Change the public port" }
	};
	banner(
		BannerKind::Warning,
		Some(&format!("{} publishes host port {}.", w.name, w.port)),
		"Traffic to it goes straight to the container, around the proxy and every access rule. Anyone who can reach the server can reach it.",
		Some(&action),
	)
}

/// The warnings, one banner each; nothing when there are none.
pub fn warnings(team: TeamId, list: &[PortWarning]) -> Markup {
	html! {
		@if !list.is_empty() {
			div class="flex flex-col gap-3" id="exposure-warnings" {
				@for w in list { (warning(team, w)) }
			}
		}
	}
}

fn network_card(team: TeamId, n: &NetworkPolicy) -> Markup {
	let count = n.members.len();
	card(&html! {
		(card_section(&n.network.name, Some(&if count == 1 {
			"One resource. Nothing else shares this network.".to_owned()
		} else {
			format!("These {count} resources reach each other by name.")
		}), None))
		(card_content(&html! {
			ul class="m-0 flex list-none flex-col gap-2 p-0" aria-label={ "Resources on " (n.network.name) } {
				@for m in &n.members {
					li class="flex flex-wrap items-center justify-between gap-2 text-sm" {
						span class="flex min-w-0 items-center gap-2" {
							a class="font-medium hover:underline" href=(resource_url(team, m.member.kind().unwrap_or(ResourceKind::Application), m.member.project_id, m.member.resource_id)) { (m.member.name) }
							span class="text-muted-foreground" { (m.member.resource_kind) }
						}
						(exposure_badge(m.exposure))
					}
				}
			}
		}))
	})
}

/// The summary of who can reach whom on this server, one card per network that has members.
pub fn section(team: TeamId, policy: &Policy) -> Markup {
	let shown: Vec<&NetworkPolicy> = policy
		.networks
		.iter()
		.filter(|n| !n.members.is_empty())
		.collect();
	html! {
		section class="ui-stack" aria-labelledby="policy-title" id="network-policy" {
			div {
				h2 id="policy-title" class="ui-sec-h" { "Who can reach what" }
				p class="ui-lede" {
					"Resources on the same network reach each other; nothing reaches across networks. "
					"The badge says how each is reached from outside the server. Set access rules and internal only on an application's Domains tab."
				}
			}
			@if shown.is_empty() {
				p class="text-sm text-muted-foreground" { "Nothing is on a network yet." }
			} @else {
				div class="ui-two" {
					@for n in shown { (network_card(team, n)) }
				}
			}
		}
	}
}

/// The warning for a database that publishes a port, on its own Security tab.
pub fn database_warning(
	team: TeamId,
	project: i64,
	id: i64,
	name: &str,
	port: Option<i32>,
) -> Markup {
	html! {
		@if let Some(port) = port {
			(warning(team, &PortWarning { kind: ResourceKind::Database, id, project_id: project, name: name.to_owned(), port }))
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn a_warning_names_the_port_and_links_to_the_fix() {
		let html = warnings(
			TeamId(7),
			&[PortWarning {
				kind: ResourceKind::Database,
				id: 3,
				project_id: 2,
				name: "orders <b>".to_owned(),
				port: 5432,
			}],
		)
		.into_string();
		assert!(html.contains("host port 5432"), "{html}");
		assert!(html.contains("/teams/7/projects/2/databases/3"), "{html}");
		assert!(html.contains("orders &lt;b&gt;"), "{html}");
		assert!(warnings(TeamId(7), &[]).into_string().is_empty());
	}
}
