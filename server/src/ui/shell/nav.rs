//! The places a team member can go from anywhere: what the sidebar lists and what the
//! command palette offers as pages. One list, so the two never disagree. A link the user
//! may not use is not in it.

use crate::auth::permission::{Action, Resource};
use crate::ui::extract::TeamCtx;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NavItem {
	pub label: &'static str,
	pub href: String,
	pub icon: &'static str,
}

fn item(label: &'static str, href: String, icon: &'static str) -> NavItem {
	NavItem { label, href, icon }
}

/// Projects, templates and models: the places a team works in every day.
pub fn main_items(ctx: &TeamCtx) -> Vec<NavItem> {
	let base = format!("/teams/{}", ctx.team.id);
	let can = |resource| ctx.perms.can(Action::Read, resource);
	let mut items = Vec::new();
	if can(Resource::Project) {
		items.push(item("Projects", format!("{base}/projects"), "layout-grid"));
		items.push(item("Templates", format!("{base}/templates"), "package"));
	}
	if can(Resource::Model) {
		items.push(item("Models", format!("{base}/models"), "brain"));
	}
	items
}

/// The Infrastructure section: the overview, then what it is made of. One list, so the
/// sidebar and the command palette never disagree.
pub fn infra_items(ctx: &TeamCtx) -> Vec<NavItem> {
	let base = format!("/teams/{}", ctx.team.id);
	let can = |resource| ctx.perms.can(Action::Read, resource);
	let mut items = Vec::new();
	if can(Resource::Server) {
		items.push(item(
			"Overview",
			format!("{base}/infrastructure"),
			"activity",
		));
		items.push(item("Servers", format!("{base}/servers"), "server"));
		items.push(item(
			"Networks",
			format!("{base}/infrastructure/networks"),
			"network",
		));
	}
	if can(Resource::SshKey) {
		items.push(item("Keys", format!("{base}/keys"), "key-round"));
	}
	if can(Resource::Registry) {
		items.push(item(
			"Registries",
			format!("{base}/registries"),
			"container",
		));
	}
	if can(Resource::GitProvider) {
		items.push(item(
			"Git providers",
			format!("{base}/git-providers"),
			"git-branch",
		));
	}
	if can(Resource::Scan) {
		items.push(item("Security", format!("{base}/security"), "shield"));
	}
	if can(Resource::Backup) {
		items.push(item(
			"S3 destinations",
			format!("{base}/backup-targets"),
			"cloud",
		));
	}
	items
}

/// Every place the palette offers: the daily ones, then the Infrastructure pages.
pub fn places(ctx: &TeamCtx) -> Vec<NavItem> {
	let mut items = main_items(ctx);
	items.extend(infra_items(ctx));
	items
}

/// The team's settings pages: team, members, registration, Hugging Face token, appearance.
pub fn settings_items(ctx: &TeamCtx) -> Vec<NavItem> {
	let base = format!("/teams/{}", ctx.team.id);
	let can = |resource| ctx.perms.can(Action::Read, resource);
	let mut items = vec![item("Team", format!("{base}/settings"), "settings")];
	if can(Resource::Membership) {
		items.push(item("Members", format!("{base}/members"), "users"));
	}
	items.push(item(
		"Registration",
		format!("{base}/settings/registration"),
		"shield",
	));
	if can(Resource::Model) {
		items.push(item(
			"Hugging Face token",
			format!("{base}/models/settings"),
			"key-round",
		));
	}
	items.push(item(
		"API tokens",
		format!("{base}/settings/api-tokens"),
		"terminal",
	));
	items.push(item(
		"Appearance",
		format!("{base}/settings/appearance"),
		"sun",
	));
	items
}

/// The one "Settings" entry that stands for the settings pages (which the settings page
/// lists in full).
pub fn settings_entry(ctx: &TeamCtx) -> NavItem {
	item(
		"Settings",
		format!("/teams/{}/settings", ctx.team.id),
		"settings",
	)
}

/// The one "Infrastructure" entry that stands for the section in the tab bar, which has
/// no room for its seven pages: it opens the overview, or the first page the person may
/// read when they cannot read servers.
pub fn infra_entry(ctx: &TeamCtx) -> Option<NavItem> {
	let first = infra_items(ctx).into_iter().next()?;
	Some(item("Infrastructure", first.href, "boxes"))
}

/// What the mobile tab bar lists: the daily places, Infrastructure, then "Settings".
pub fn primary_items(ctx: &TeamCtx) -> Vec<NavItem> {
	let mut items = main_items(ctx);
	items.extend(infra_entry(ctx));
	items.push(settings_entry(ctx));
	items
}

/// The entry of the sidebar the request is on: the page itself, with every settings page
/// marking "Settings".
pub fn sidebar_current(ctx: &TeamCtx) -> Option<String> {
	let places = places(ctx);
	let settings = settings_items(ctx);
	let best = current(places.iter().chain(&settings), &ctx.path)?;
	if settings.iter().any(|i| i.href == best) {
		Some(settings_entry(ctx).href)
	} else {
		Some(best.to_owned())
	}
}

/// The entry of [`primary_items`] the request is on: every settings page marks "Settings"
/// and every Infrastructure page marks "Infrastructure".
pub fn primary_current(ctx: &TeamCtx) -> Option<String> {
	let best = sidebar_current(ctx)?;
	if infra_items(ctx).iter().any(|i| i.href == best) {
		infra_entry(ctx).map(|i| i.href)
	} else {
		Some(best)
	}
}

/// How many entries the tab bar shows.
pub const TAB_SLOTS: usize = 5;

/// The mobile tab bar: [`primary_items`] cut to [`TAB_SLOTS`], keeping "Settings" last.
pub fn tab_items(ctx: &TeamCtx) -> Vec<NavItem> {
	let mut items = primary_items(ctx);
	if items.len() > TAB_SLOTS
		&& let Some(settings) = items.pop()
	{
		items.truncate(TAB_SLOTS - 1);
		items.push(settings);
	}
	items
}

/// The item whose page `path` is on: the longest href that is `path` or a parent of it,
/// so `/models/settings` marks the token page and not Models.
pub fn current<'a>(items: impl IntoIterator<Item = &'a NavItem>, path: &str) -> Option<&'a str> {
	items
		.into_iter()
		.filter(|i| {
			path == i.href
				|| path
					.strip_prefix(i.href.as_str())
					.is_some_and(|rest| rest.starts_with('/'))
		})
		.max_by_key(|i| i.href.len())
		.map(|i| i.href.as_str())
}

#[cfg(test)]
mod tests {
	use super::*;

	fn items() -> Vec<NavItem> {
		vec![
			item("Models", "/teams/1/models".to_owned(), "brain"),
			item("Token", "/teams/1/models/settings".to_owned(), "key-round"),
			item("Projects", "/teams/1/projects".to_owned(), "folder"),
		]
	}

	#[test]
	fn the_longest_matching_href_is_current() {
		let items = items();
		assert_eq!(
			current(&items, "/teams/1/models/settings"),
			Some("/teams/1/models/settings")
		);
		assert_eq!(
			current(&items, "/teams/1/models/catalog/x"),
			Some("/teams/1/models")
		);
		assert_eq!(
			current(&items, "/teams/1/projects/7/apps/3"),
			Some("/teams/1/projects")
		);
	}

	#[test]
	fn a_longer_name_sharing_a_prefix_is_not_a_match() {
		let items = items();
		assert_eq!(current(&items, "/teams/1/projects-old"), None);
		assert_eq!(current(&items, "/teams/10/projects"), None);
	}
}
