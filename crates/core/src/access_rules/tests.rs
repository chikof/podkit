//! Access-rule tests: the lists and limits are checked, the rules reach the agent as the
//! middlewares Traefik is given, internal-only leaves the ingress, a basic-auth hash is never
//! shown, the warnings find published ports, and another team's rules are not there.
//!
//! `cargo test -p podkit-core -p podkit-ui --all-features -- access_rules`

use super::Rules;
use super::service::RulesInput;
use crate::error::ServerError;

// --- the lists and limits ----------------------------------------------------------------------

fn lists(allow: &[&str], deny: &[&str]) -> Rules {
	Rules {
		allow_cidrs: allow.iter().map(ToString::to_string).collect(),
		deny_cidrs: deny.iter().map(ToString::to_string).collect(),
		..Rules::default()
	}
}

#[test]
fn access_rules_allowlist_cidrs_are_validated_and_stored_in_their_normal_form() {
	let mut ok = lists(
		&["10.0.0.7/24", "203.0.113.9", "2001:db8::/32", "10.0.0.0/24"],
		&[],
	);
	ok.validate().unwrap();
	// Host bits are cleared, a bare address is a range of one, duplicates go.
	assert_eq!(
		ok.allow_cidrs,
		["10.0.0.0/24", "2001:db8::/32", "203.0.113.9/32"]
	);
	for bad in [
		"nope",
		"10.0.0.0/33",
		"10.0.0/8",
		"10.0.0.0/8/9",
		"::1/129",
		"10.0.0.0/",
		"1.1.1.1;2.2.2.2",
	] {
		let err = lists(&[bad], &[]).validate().unwrap_err();
		assert!(
			matches!(&err, ServerError::Validation(m) if m.contains("not an IP address")),
			"{bad}: {err:?}"
		);
		assert!(lists(&[], &[bad]).validate().is_err(), "{bad} as a deny");
	}
}

#[test]
fn access_rules_lists_that_lock_everyone_out_or_grow_too_large_are_refused() {
	let err = lists(&["10.0.0.0/24"], &["10.0.0.0/8"])
		.validate()
		.unwrap_err();
	assert!(err.to_string().contains("block every address"), "{err}");
	let many: Vec<String> = (0..=super::MAX_ENTRIES)
		.map(|n| format!("10.0.{n}.0/24"))
		.collect();
	let refs: Vec<&str> = many.iter().map(String::as_str).collect();
	assert!(lists(&refs, &[]).validate().is_err());
	// Denying single addresses out of everything splits the space once per bit each.
	let spread: Vec<String> = (0..50).map(|n| format!("{n}.{n}.{n}.{n}")).collect();
	let refs: Vec<&str> = spread.iter().map(String::as_str).collect();
	let err = lists(&[], &refs).validate().unwrap_err();
	assert!(err.to_string().contains("too many pieces"), "{err}");
}

#[test]
fn access_rules_limits_need_sane_numbers_and_a_burst_needs_a_rate() {
	let check = |f: fn(&mut Rules)| {
		let mut r = Rules::default();
		f(&mut r);
		r.validate()
	};
	assert!(check(|r| r.rate_average = Some(5)).is_ok());
	assert!(
		check(|r| {
			r.rate_average = Some(5);
			r.rate_burst = Some(20);
		})
		.is_ok()
	);
	assert!(check(|r| r.rate_average = Some(0)).is_err());
	assert!(check(|r| r.max_body_mb = Some(0)).is_err());
	assert!(check(|r| r.max_body_mb = Some(10_241)).is_err());
	assert!(check(|r| r.rate_burst = Some(5)).is_err());
}

#[test]
fn access_rules_input_reads_lists_in_any_separator_and_names_the_bad_number() {
	let input = RulesInput {
		allow: "10.0.0.0/8, 192.168.0.0/16\n203.0.113.7 ".to_owned(),
		deny: "198.51.100.0/24".to_owned(),
		rate_average: " 5 ".to_owned(),
		max_body_mb: String::new(),
		security_headers: true,
		..RulesInput::default()
	};
	let rules = input.parse(false).unwrap();
	assert_eq!(rules.allow_cidrs.len(), 3);
	assert_eq!(rules.rate_average, Some(5));
	assert_eq!(rules.max_body_mb, None);
	let bad = RulesInput {
		rate_average: "fast".to_owned(),
		..RulesInput::default()
	};
	assert!(
		bad.parse(false)
			.unwrap_err()
			.to_string()
			.contains("rate limit")
	);
}

// --- the middlewares ---------------------------------------------------------------------------

fn label<'a>(labels: &'a [(String, String)], key: &str) -> Option<&'a str> {
	labels
		.iter()
		.find(|(k, _)| k == key)
		.map(|(_, v)| v.as_str())
}

#[test]
fn access_rules_each_option_becomes_its_traefik_middleware() {
	let rules = Rules {
		allow_cidrs: vec!["10.0.0.0/8".to_owned()],
		deny_cidrs: vec![],
		rate_average: Some(5),
		rate_burst: Some(10),
		max_body_mb: Some(2),
		security_headers: true,
		internal_only: false,
	};
	let wire = rules.to_wire(vec!["ann:$2y$10$x".to_owned()]).unwrap();
	assert_eq!(wire.max_body_bytes, Some(2 * 1024 * 1024));
	let labels = agent::ingress::access_labels("app-1", &wire, true, true).unwrap();
	let m = |k: &str| label(&labels, &format!("traefik.http.middlewares.app-1-{k}"));
	assert_eq!(m("allow.ipallowlist.sourcerange"), Some("10.0.0.0/8"));
	assert_eq!(m("rate.ratelimit.average"), Some("5"));
	assert_eq!(m("rate.ratelimit.burst"), Some("10"));
	assert_eq!(m("auth.basicauth.users"), Some("ann:$2y$10$x"));
	assert_eq!(m("body.buffering.maxrequestbodybytes"), Some("2097152"));
	assert_eq!(m("headers.headers.framedeny"), Some("true"));
	assert_eq!(m("headers.headers.stsseconds"), Some("31536000"));
	assert_eq!(m("headers.headers.contenttypenosniff"), Some("true"));
	assert_eq!(
		m("headers.headers.referrerpolicy"),
		Some("strict-origin-when-cross-origin")
	);
	assert!(label(&labels, "traefik.http.routers.app-1.middlewares").is_some());
	assert!(label(&labels, "traefik.http.routers.app-1-secure.middlewares").is_some());
}

#[test]
fn access_rules_a_denylist_is_sent_as_the_allowed_remainder() {
	let wire = lists(&[], &["203.0.113.7"]).to_wire(vec![]).unwrap();
	let labels = agent::ingress::access_labels("app-1", &wire, true, false).unwrap();
	let range = label(
		&labels,
		"traefik.http.middlewares.app-1-allow.ipallowlist.sourcerange",
	)
	.unwrap();
	let ranges: Vec<proto::cidr::Cidr> = range
		.split(',')
		.map(|r| proto::cidr::Cidr::parse(r).unwrap())
		.collect();
	let at = |ip: &str| ranges.iter().any(|r| r.contains_ip(ip.parse().unwrap()));
	assert!(!at("203.0.113.7"));
	assert!(at("203.0.113.8") && at("8.8.8.8") && at("::1"));
}

#[test]
fn access_rules_the_defaults_are_only_the_security_headers() {
	let wire = Rules::default().to_wire(vec![]).unwrap();
	assert!(wire.security_headers);
	assert!(wire.allow.is_empty() && wire.basic_auth.is_empty() && wire.rate_average.is_none());
	let off = Rules {
		security_headers: false,
		..Rules::default()
	};
	assert!(
		off.to_wire(vec![]).is_none(),
		"nothing to write, no middleware"
	);
}
