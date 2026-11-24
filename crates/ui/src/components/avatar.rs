//! Avatar: a person as a rounded square, named for assistive technology. It shows the
//! picture they uploaded when there is one and their initials otherwise. The picture sits
//! on top of the initials, so a picture that fails to load leaves the monogram showing.

use maud::{Markup, html};

use podkit_core::avatars;
use podkit_core::ids::UserId;

/// Up to two initials: the first letters of the first two words (`Ada Lovelace` is `AL`,
/// `ada` is `A`), or `?` for a name with no letters or digits.
fn initials(name: &str) -> String {
	let letters: String = name
		.split(|c: char| !c.is_alphanumeric())
		.filter(|word| !word.is_empty())
		.take(2)
		.filter_map(|word| word.chars().next())
		.flat_map(char::to_uppercase)
		.collect();
	if letters.is_empty() {
		"?".to_owned()
	} else {
		letters
	}
}

/// The monogram alone, for a name with no account behind it (the `/_ui` sheet).
pub fn avatar(name: &str) -> Markup {
	html! {
		span class="ui-avatar" data-h=(super::logo::tint(name)) role="img" aria-label=(name) {
			(initials(name))
		}
	}
}

/// A person: their picture when `hash` says they have one, else the monogram.
pub fn user_avatar(name: &str, user: UserId, hash: Option<&str>) -> Markup {
	person(name, user, hash, "ui-avatar ui-avatar-pic-host")
}

/// [`user_avatar`] at 64 px, for the account page.
pub fn user_avatar_lg(name: &str, user: UserId, hash: Option<&str>) -> Markup {
	person(
		name,
		user,
		hash,
		"ui-avatar ui-avatar-pic-host ui-avatar-lg",
	)
}

fn person(name: &str, user: UserId, hash: Option<&str>, class: &'static str) -> Markup {
	html! {
		span class=(class) data-h=(super::logo::tint(name)) role="img" aria-label=(name) {
			(initials(name))
			@if let Some(hash) = hash {
				img class="ui-avatar-pic" src=(avatars::user_url(user, hash)) alt="" width="64" height="64"
					decoding="async";
			}
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn initials_come_from_the_first_two_words() {
		assert_eq!(initials("Ada Lovelace"), "AL");
		assert_eq!(initials("ada"), "A");
		assert_eq!(initials("grace brewster hopper"), "GB");
		assert_eq!(initials("  "), "?");
		assert_eq!(initials("élan vital"), "ÉV");
	}

	#[test]
	fn the_avatar_is_an_image_named_by_the_person_and_escapes_it() {
		let html = avatar("<script>x</script> Z").into_string();
		assert!(html.contains("role=\"img\""), "{html}");
		assert!(!html.contains("<script>"), "{html}");
		assert!(html.contains("aria-label=\"&lt;script&gt;"), "{html}");
	}

	#[test]
	fn the_tint_is_one_of_six_and_stable_per_name() {
		let mut seen = std::collections::BTreeSet::new();
		for name in ["a", "b", "c", "d", "e", "f", "Ada", "Grace Hopper"] {
			let html = avatar(name).into_string();
			assert_eq!(html, avatar(name).into_string());
			let h: u8 = html
				.split("data-h=\"")
				.nth(1)
				.and_then(|r| r.split('"').next())
				.and_then(|n| n.parse().ok())
				.unwrap_or(0);
			assert!((1..=6).contains(&h), "{html}");
			seen.insert(h);
		}
		// Consecutive single letters walk through all six tints.
		assert!(seen.len() >= 6, "{seen:?}");
	}

	#[test]
	fn a_nameless_avatar_falls_back_to_a_question_mark() {
		let html = avatar("  ").into_string();
		assert!(html.contains(">?</span>"), "{html}");
		assert!(html.contains("data-h=\""), "{html}");
	}
}
