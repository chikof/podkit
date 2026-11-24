//! Servers: the team's podman hosts as cards with live figures, adding one in a stepped
//! dialog, and a server's pages: Overview (status, hardware, bootstrap log, drain, actions)
//! and Monitoring (charts from the sampler).
//!
//! The SSH private key is write-only: it travels from the form to the create action and is
//! never rendered back, into HTML or into a signal.

use axum::Router;
use maud::{Markup, html};

use crate::components::{TabItem, Tone, tabs};
use crate::extract::TeamCtx;
use crate::layout::Crumb;
use podkit_core::ids::{ServerId, TeamId};
use podkit_core::servers::Server;
use podkit_core::state::AppState;

mod detail;
pub(crate) use detail::header;
mod drain;
mod key_picker;
mod list;
mod monitoring;
#[cfg(test)]
mod tests;

pub fn router() -> Router<AppState> {
	Router::new()
		.merge(list::router())
		.merge(detail::router())
		.merge(drain::router())
		.merge(monitoring::router())
}

pub(crate) fn list_url(team: TeamId) -> String {
	format!("/teams/{team}/servers")
}

pub(crate) fn detail_url(team: TeamId, server: ServerId) -> String {
	format!("/teams/{team}/servers/{server}")
}

fn monitoring_url(team: TeamId, server: ServerId) -> String {
	format!("/teams/{team}/servers/{server}/monitoring")
}

pub(crate) fn crumbs<'a>(ctx: &TeamCtx) -> Vec<Crumb<'a>> {
	vec![("Servers", Some(list_url(ctx.team.id)))]
}

fn host_port(server: &Server) -> String {
	if server.is_local {
		"this host".to_owned()
	} else {
		format!("{}:{}", server.hostname, server.ssh_port)
	}
}

pub(crate) fn local_badge() -> Markup {
	html! {
		span class=(Tone::Neutral.badge_class()) { "Local" }
	}
}

/// `512 MB`, `16 GB`, `1.5 TB`: sizes as the agent reports them, in MB.
fn human_mb(mb: u64) -> String {
	const GB: f64 = 1024.0;
	let v = mb as f64;
	let fmt = |x: f64, unit: &str| {
		let s = format!("{x:.1}");
		format!("{} {unit}", s.trim_end_matches('0').trim_end_matches('.'))
	};
	if v >= GB * GB {
		fmt(v / (GB * GB), "TB")
	} else if v >= GB {
		fmt(v / GB, "GB")
	} else {
		format!("{mb} MB")
	}
}

/// The hardware the agent last reported, one short phrase each: threads, RAM, disk and
/// GPUs. Empty until the agent has reported.
pub(crate) fn hardware_parts(server: &Server) -> Vec<String> {
	let Some(hw) = server.hardware.as_ref().map(|h| &h.0) else {
		return Vec::new();
	};
	let mut parts = Vec::new();
	if hw.cpu.threads > 0 {
		parts.push(format!("{} threads", hw.cpu.threads));
	}
	if hw.memory.total_mb > 0 {
		parts.push(format!("{} RAM", human_mb(hw.memory.total_mb)));
	}
	if let Some(disk) = hw.disks.iter().max_by_key(|d| d.total_mb) {
		parts.push(format!("{} disk", human_mb(disk.total_mb)));
	}
	for gpu in &hw.gpus {
		parts.push(format!("{} {}", gpu.name, human_mb(gpu.vram_mb)));
	}
	parts
}

/// The sections of a server's page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ServerTab {
	Overview,
	Monitoring,
	Networks,
	Firewall,
}

fn server_tabs(team: TeamId, server: ServerId, active: ServerTab) -> Markup {
	let overview = detail_url(team, server);
	let charts = monitoring_url(team, server);
	let networks = format!("{overview}/networks");
	let firewall = format!("{overview}/firewall");
	tabs(
		"Server sections",
		&[
			TabItem {
				label: "Overview",
				href: &overview,
				current: active == ServerTab::Overview,
			},
			TabItem {
				label: "Monitoring",
				href: &charts,
				current: active == ServerTab::Monitoring,
			},
			TabItem {
				label: "Networks",
				href: &networks,
				current: active == ServerTab::Networks,
			},
			TabItem {
				label: "Firewall",
				href: &firewall,
				current: active == ServerTab::Firewall,
			},
		],
	)
}

#[cfg(test)]
mod unit {
	use super::*;

	#[test]
	fn sizes_read_in_the_largest_whole_unit() {
		assert_eq!(human_mb(512), "512 MB");
		assert_eq!(human_mb(16 * 1024), "16 GB");
		assert_eq!(human_mb(1536), "1.5 GB");
		assert_eq!(human_mb(2 * 1024 * 1024), "2 TB");
	}
}
