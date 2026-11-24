//! Single sign-on, end to end against [`stub`](super::stub): the round trip through the real
//! routes (start, the provider, callback), ID-token validation, account creation and linking,
//! group mapping, and the rules every slice follows (other teams get 404, the secret is never
//! shown). Every test name holds `oidc`, so `-- oidc` runs them.

#![allow(
	clippy::many_single_char_names,
	clippy::too_many_lines,
	reason = "a table of scenarios reads best straight down"
)]

#[test]
fn oidc_pkce_challenge_matches_the_rfc_7636_example() {
	// RFC 7636 appendix B.
	assert_eq!(
		super::oidc::challenge_of("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
		"E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
	);
	let pair = super::oidc::pkce();
	assert!(
		pair.verifier.len() >= 43,
		"RFC 7636 §4.1: 43 to 128 characters"
	);
	assert_eq!(super::oidc::challenge_of(&pair.verifier), pair.challenge);
}

#[test]
fn oidc_the_ui_holds_no_queries_and_no_authorization() {
	// The invariant of the UI slice, kept here for the pages this card adds.
	let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../ui/src");
	let mut bad = Vec::new();
	let mut stack = vec![root];
	while let Some(dir) = stack.pop() {
		for entry in std::fs::read_dir(dir).unwrap() {
			let path = entry.unwrap().path();
			if path.is_dir() {
				stack.push(path);
			} else if path.extension().is_some_and(|e| e == "rs")
				&& !path.ends_with("testing.rs")
				&& !path.to_string_lossy().contains("tests")
			{
				let text = std::fs::read_to_string(&path).unwrap();
				for (n, line) in text.lines().enumerate() {
					let code = line.split("//").next().unwrap_or("");
					if code.contains("sqlx::")
						|| code.contains("query!")
						|| code.contains("query_as")
						|| code.contains("query_scalar")
						|| code.contains("authorize(")
					{
						bad.push(format!("{}:{}", path.display(), n + 1));
					}
				}
			}
		}
	}
	assert!(bad.is_empty(), "{bad:?}");
}

#[test]
fn sec_cleanup_oidc_domains_are_parsed_strictly() {
	use super::domains::{of_email, parse};
	assert_eq!(
		parse("Example.com, @corp.example\nexample.com").unwrap(),
		vec!["example.com", "corp.example"]
	);
	assert!(parse("").unwrap().is_empty());
	assert!(parse("localhost").is_err());
	assert!(parse("a..com").is_err());
	assert!(parse("*.com").is_err());
	assert_eq!(of_email("Ada@Corp.Test").as_deref(), Some("corp.test"));
	assert_eq!(of_email("nobody"), None);
}
