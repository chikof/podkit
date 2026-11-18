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
		items.push(item("Projects", format!("{base}/projects"), "folder"));
		items.push(item("Templates", format!("{base}/templates"), "layers"));
	}
	if can(Resource::Server) {
		items.push(item("Servers", format!("{base}/servers"), "server"));
	}
	if can(Resource::Model) {
		items.push(item("Models", format!("{base}/models"), "brain"));
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
