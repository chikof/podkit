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

/// Projects, templates, servers, models and S3 destinations.
pub fn main_items(ctx: &TeamCtx) -> Vec<NavItem> {
	let base = format!("/teams/{}", ctx.team.id);
	let can = |resource| ctx.perms.can(Action::Read, resource);
	let mut items = Vec::new();
	if can(Resource::Project) {
		items.push(item("Projects", format!("{base}/projects"), "layout-grid"));
		items.push(item("Templates", format!("{base}/templates"), "package"));
	}
	if can(Resource::Server) {
		items.push(item("Servers", format!("{base}/servers"), "server"));
	}
	if can(Resource::Model) {
		items.push(item("Models", format!("{base}/models"), "brain"));
	}
	if can(Resource::Scan) {
		items.push(item("Security", format!("{base}/security"), "shield"));
	}
	if can(Resource::Backup) {
		items.push(item(
			"S3 Destinations",
			format!("{base}/backup-targets"),
			"cloud",
		));
	}
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
		"Appearance",
		format!("{base}/settings/appearance"),
		"sun",
	));
	items
}

/// What the sidebar and the mobile tab bar list: the places above, then one "Settings"
/// entry that stands for the settings pages (which the settings page lists in full).
pub fn primary_items(ctx: &TeamCtx) -> Vec<NavItem> {
	let mut items = main_items(ctx);
	items.push(item(
		"Settings",
		format!("/teams/{}/settings", ctx.team.id),
		"settings",
	));
	items
}

/// The entry of [`primary_items`] the request is on. Every settings page marks "Settings".
pub fn primary_current(ctx: &TeamCtx) -> Option<String> {
	let main = main_items(ctx);
	let settings = settings_items(ctx);
	let best = current(main.iter().chain(&settings), &ctx.path)?;
	if settings.iter().any(|i| i.href == best) {
		Some(format!("/teams/{}/settings", ctx.team.id))
	} else {
		Some(best.to_owned())
	}
}

/// How many entries the tab bar shows.
pub const TAB_SLOTS: usize = 5;

/// The mobile tab bar: [`primary_items`] cut to [`TAB_SLOTS`], keeping "Settings" last. What
/// does not fit (S3 destinations, for a user who sees everything) stays in the sidebar of a
/// wider screen and in the command palette.
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
