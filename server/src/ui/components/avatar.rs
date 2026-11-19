//! Avatar: a person or team as a round monogram (initials), named for assistive
//! technology. podkit stores no profile pictures, so there is no image variant.

use maud::{Markup, html};

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

pub fn avatar(name: &str) -> Markup {
	html! {
		span class="ui-avatar" data-h=(super::logo::tint(name)) role="img" aria-label=(name) {
			(initials(name))
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
}
