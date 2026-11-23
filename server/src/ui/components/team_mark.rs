//! Team mark: a team as a small rounded square, the team's picture when it has one and
//! the first letter of its name otherwise. It is decoration beside the team's name, so
//! it is hidden from assistive technology.

use maud::{Markup, html};

use crate::avatars;
use crate::teams::Team;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkSize {
	/// 18 px, in a menu row.
	Small,
	/// 22 px, in the sidebar and the breadcrumbs.
	Medium,
	/// 64 px, on the team settings page.
	Large,
}

impl MarkSize {
	const fn class(self) -> &'static str {
		match self {
			Self::Small => "ui-team-mark ui-team-mark-sm ui-avatar-pic-host",
			Self::Medium => "ui-team-mark ui-avatar-pic-host",
			Self::Large => "ui-team-mark ui-team-mark-lg ui-avatar-pic-host",
		}
	}
}

fn initial(name: &str) -> String {
	name.chars()
		.find(|c| c.is_alphanumeric())
		.map_or_else(|| "?".to_owned(), |c| c.to_uppercase().collect())
}

pub fn team_mark(team: &Team, size: MarkSize) -> Markup {
	html! {
		span class=(size.class()) aria-hidden="true" {
			(initial(&team.name))
			@if let Some(hash) = &team.avatar {
				img class="ui-avatar-pic" src=(avatars::team_url(team.id, hash)) alt="" width="64" height="64"
					decoding="async";
			}
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::ids::TeamId;

	fn team(name: &str, avatar: Option<&str>) -> Team {
		Team {
			id: TeamId(7),
			name: name.to_owned(),
			slug: "t".to_owned(),
			logo: String::new(),
			avatar: avatar.map(str::to_owned),
		}
	}

	#[test]
	fn avatars_team_mark_falls_back_to_the_initial() {
		let html = team_mark(&team("ångström", None), MarkSize::Medium).into_string();
		assert!(html.contains(">Å</span>"), "{html}");
		assert!(!html.contains("<img"), "{html}");
	}

	#[test]
	fn avatars_team_mark_shows_the_picture_over_the_initial() {
		let html = team_mark(&team("Acme", Some("abc123")), MarkSize::Small).into_string();
		assert!(html.contains("src=\"/avatars/team/7?v=abc123\""), "{html}");
		assert!(html.contains("alt=\"\""), "{html}");
		assert!(
			html.contains(">A<img"),
			"the initial stays under the picture: {html}"
		);
	}
}
