//! Tests of the Security pages: the tab of a resource, the overview, the badge, the ignore
//! flow, the SBOM download, the server settings and who may see or do what.
//!
//! `cargo test -p podkit-ui --all-features -- scanning`

use axum::http::{StatusCode, header};
use serde_json::json;

use crate::testing::{
	assert_contains, assert_not_contains, get_html, page_main, post_signals, ui_login,
};
use crate::tests::scanning::{
	Fx, IMAGE, SBOM, fake_agent, first_ignore_id, first_scan_id, server_scan_settings,
	set_finding_package, set_finding_url,
};
use crate::tests::{PgPool, count, db_test, member_role, revoke_permission, user_with_team};

struct Page {
	f: Fx,
	cookie: String,
}

/// [`Fx`] over the whole router: these tests read the pages.
async fn fx(db: PgPool) -> Fx {
	let state = crate::tests::state(db.clone());
	let app = crate::routes(state.clone()).await.unwrap();
	Fx::over(db, state, app).await
}

impl Page {
	/// A team with a scanned application (`Storefront`, id 5001) and a signed-in owner.
	async fn scanned(db: PgPool) -> Self {
		let f = fx(db).await;
		fake_agent(&f.state);
		let scan = f.queue(IMAGE).await;
		f.run(scan).await.unwrap();
		let cookie = ui_login(&f.app, "alice").await;
		Self { f, cookie }
	}

	fn app(&self, tail: &str) -> String {
		format!(
			"/teams/{}/projects/{}/apps/5001{tail}",
			self.f.team, self.f.project
		)
	}

	fn tab(&self, query: &str) -> String {
		self.app(&format!("/security{query}"))
	}

	fn security(&self, tail: &str) -> String {
		format!("/teams/{}/security{tail}", self.f.team)
	}

	async fn get(&self, uri: &str) -> (StatusCode, String) {
		let (s, _, body) = get_html(&self.f.app, uri, Some(&self.cookie)).await;
		(s, body)
	}

	async fn post(&self, uri: &str, signals: &serde_json::Value) -> (StatusCode, String) {
		let (s, _, body) = post_signals(&self.f.app, uri, Some(&self.cookie), signals).await;
		(s, body)
	}

	async fn revoke(&self, action: &str, resource: &str) {
		let role = member_role(&self.f.db, self.f.team, "alice").await;
		revoke_permission(&self.f.db, role, action, resource).await;
	}

	async fn scan_id(&self) -> i64 {
		first_scan_id(&self.f.db).await
	}
}

fn ignore_form(vuln: &str, scope: &str, reason: &str, days: i64) -> serde_json::Value {
	json!({
		"ig_vuln": vuln, "ig_pkg": "libcrypto1.1", "ig_scope": scope,
		"ig_reason": reason, "ig_days": days.to_string(),
	})
}

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn scanning_ui_the_tab_lists_findings_worst_first_with_act_now_and_the_sbom(db: PgPool) {
	let p = Page::scanned(db).await;
	let (s, body) = p.get(&p.tab("")).await;
	assert_eq!(s, StatusCode::OK, "{body}");
	assert_eq!(body.matches("<h1").count(), 1, "one h1 per page");
	let main = page_main(&body);
	assert_contains(main, "Findings");
	assert_contains(main, "CVE-2022-0001");
	assert_contains(main, "Act now");
	assert_contains(main, "libcrypto1.1");
	// Worst first: the act-now critical before the medium.
	let critical = main.find("CVE-2022-0001").unwrap();
	let medium = main.find("CVE-2023-2650").unwrap();
	assert!(critical < medium, "act-now findings come first");
	// Informational only, and the SBOM is one click away.
	assert_contains(main, "never stops a deploy");
	assert_contains(main, &format!("/teams/{}/security/scans/", p.f.team));
	assert_contains(main, "/sbom");
	assert_contains(main, "Scan again");
	assert_contains(main, IMAGE);
}

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn scanning_ui_filters_narrow_the_table_by_link(db: PgPool) {
	let p = Page::scanned(db).await;
	let (_, body) = p.get(&p.tab("?severity=low")).await;
	let main = page_main(&body);
	assert_contains(main, "CVE-2026-54872");
	assert_not_contains(main, "CVE-2022-0001");

	let (_, body) = p.get(&p.tab("?act_now=true")).await;
	let main = page_main(&body);
	assert_contains(main, "CVE-2022-0001");
	assert_not_contains(main, "CVE-2023-2650");

	let (_, body) = p.get(&p.tab("?fixable=true")).await;
	let main = page_main(&body);
	assert_contains(main, "CVE-2022-0001");
	assert_not_contains(main, "CVE-2021-9999");

	// The chips keep what is already chosen.
	let (_, body) = p.get(&p.tab("?severity=high&fixable=true")).await;
	assert_contains(page_main(&body), "severity=high&amp;fixable=true");
}

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn scanning_ui_scanner_data_never_leaves_as_script_or_markup(db: PgPool) {
	let p = Page::scanned(db).await;
	set_finding_url(&p.f.db, "CVE-2023-2650", "javascript:alert(1)").await;
	set_finding_package(&p.f.db, "CVE-2021-9999", "<img src=x onerror=alert(1)>").await;
	let (_, body) = p.get(&p.tab("")).await;
	assert_not_contains(&body, "javascript:");
	assert_not_contains(&body, "<img src=x");
	assert_contains(&body, "&lt;img src=x");
}

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn scanning_ui_ignoring_hides_a_finding_until_shown_and_can_be_undone(db: PgPool) {
	let p = Page::scanned(db).await;
	let (s, body) = p
		.post(
			&p.security("/ignores"),
			&ignore_form("CVE-2023-2650", "all", "not reachable from the network", 30),
		)
		.await;
	assert_eq!(s, StatusCode::OK);
	assert_contains(&body, "window.location.reload()");

	let (_, body) = p.get(&p.tab("")).await;
	let main = page_main(&body);
	assert_contains(main, "Ignored findings");
	assert_contains(main, "not reachable from the network");
	// The findings table no longer lists it; the ignore list does.
	let table = &main[main.find("id=\"findings\"").unwrap()..main.find("id=\"ignores\"").unwrap()];
	assert_not_contains(table, ">CVE-2023-2650</a>");
	let (_, shown) = p.get(&p.tab("?ignored=true")).await;
	let shown = page_main(&shown);
	let table =
		&shown[shown.find("id=\"findings\"").unwrap()..shown.find("id=\"ignores\"").unwrap()];
	assert_contains(table, ">CVE-2023-2650</a>");

	// Removing it brings the finding back.
	let id = first_ignore_id(&p.f.db).await;
	let (s, _) = p
		.post(&p.security(&format!("/ignores/{id}/remove")), &json!({}))
		.await;
	assert_eq!(s, StatusCode::OK);
	assert_eq!(count(&p.f.db, "scan_ignores").await, 0);
	let (_, body) = p.get(&p.tab("")).await;
	assert_contains(page_main(&body), ">CVE-2023-2650</a>");
}

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn scanning_ui_an_ignore_needs_a_reason_a_sane_period_and_update_permission(db: PgPool) {
	let p = Page::scanned(db).await;
	for (reason, days) in [("  ", 30), ("ok", 4000), ("ok", 0)] {
		let (_, body) = p
			.post(
				&p.security("/ignores"),
				&ignore_form("CVE-2023-2650", "all", reason, days),
			)
			.await;
		assert_contains(&body, "_ig_error");
	}
	assert_eq!(count(&p.f.db, "scan_ignores").await, 0);

	// A person who may read but not update sees no Ignore button, and a forged post fails.
	p.revoke("update", "scan").await;
	let (_, body) = p.get(&p.tab("")).await;
	assert_not_contains(page_main(&body), "aria-haspopup=\"dialog\"");
	assert_not_contains(page_main(&body), "id=\"ignore-dialog\"");
	let (_, body) = p
		.post(
			&p.security("/ignores"),
			&ignore_form("CVE-2023-2650", "all", "forged", 30),
		)
		.await;
	assert_contains(&body, "_ig_error");
	assert_eq!(count(&p.f.db, "scan_ignores").await, 0);
}

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn scanning_ui_the_sbom_downloads_as_cyclonedx_and_needs_read_permission(db: PgPool) {
	let p = Page::scanned(db).await;
	let uri = p.security(&format!("/scans/{}/sbom", p.scan_id().await));
	let (s, headers, body) = get_html(&p.f.app, &uri, Some(&p.cookie)).await;
	assert_eq!(s, StatusCode::OK);
	assert_eq!(
		headers[header::CONTENT_TYPE],
		"application/vnd.cyclonedx+json"
	);
	assert!(
		headers[header::CONTENT_DISPOSITION]
			.to_str()
			.unwrap()
			.starts_with("attachment; filename=\"sbom-")
	);
	assert_eq!(body, SBOM);

	p.revoke("read", "scan").await;
	let (s, _, body) = get_html(&p.f.app, &uri, Some(&p.cookie)).await;
	assert_eq!(s, StatusCode::FORBIDDEN);
	assert_not_contains(&body, "bomFormat");
}

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn scanning_ui_another_team_gets_a_404_for_every_scan_page_and_action(db: PgPool) {
	let p = Page::scanned(db).await;
	let (_, bob_team) = user_with_team(&p.f.app, "bob").await;
	let bob = ui_login(&p.f.app, "bob").await;
	let scan = p.scan_id().await;

	// Alice's application, through Bob's team and through hers.
	let theirs = format!(
		"/teams/{bob_team}/projects/{}/apps/5001/security",
		p.f.project
	);
	let (s, _, _) = get_html(&p.f.app, &theirs, Some(&bob)).await;
	assert_eq!(s, StatusCode::NOT_FOUND);
	let (s, _, _) = get_html(&p.f.app, &p.tab(""), Some(&bob)).await;
	assert_eq!(
		s,
		StatusCode::NOT_FOUND,
		"a team's id in the path is checked"
	);

	// Her scan's SBOM and status, asked for under Bob's team.
	for tail in [
		format!("/scans/{scan}/sbom"),
		format!("/scans/{scan}/status"),
	] {
		let uri = format!("/teams/{bob_team}/security{tail}");
		let (s, _, body) = get_html(&p.f.app, &uri, Some(&bob)).await;
		assert_eq!(s, StatusCode::NOT_FOUND, "{tail}");
		assert_not_contains(&body, "bomFormat");
	}

	// Bob's overview lists nothing of hers.
	let (s, _, body) = get_html(&p.f.app, &format!("/teams/{bob_team}/security"), Some(&bob)).await;
	assert_eq!(s, StatusCode::OK);
	assert_not_contains(&body, "Storefront");

	// An ignore of hers cannot be removed from his team.
	p.post(
		&p.security("/ignores"),
		&ignore_form("CVE-2023-2650", "all", "mine", 30),
	)
	.await;
	let id = first_ignore_id(&p.f.db).await;
	post_signals(
		&p.f.app,
		&format!("/teams/{bob_team}/security/ignores/{id}/remove"),
		Some(&bob),
		&json!({}),
	)
	.await;
	assert_eq!(count(&p.f.db, "scan_ignores").await, 1, "still there");
}

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn scanning_ui_the_overview_lists_resources_by_risk_with_one_h1(db: PgPool) {
	let p = Page::scanned(db).await;
	let (s, body) = p.get(&p.security("")).await;
	assert_eq!(s, StatusCode::OK, "{body}");
	assert_eq!(body.matches("<h1").count(), 1, "one h1 per page");
	let main = page_main(&body);
	assert_contains(main, "Storefront");
	assert_contains(main, "orders");
	assert_contains(main, "Not scanned yet");
	assert_contains(main, "Critical");
	assert_contains(main, "Act now");
	// The scanned, risky one is first; the never-scanned one is last.
	assert!(main.find("Storefront").unwrap() < main.find("orders").unwrap());
	// The sidebar links to it, and a row goes to the resource's own tab.
	assert_contains(&body, &format!("href=\"/teams/{}/security\"", p.f.team));
	assert_contains(main, "/apps/5001/security");
}

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn scanning_ui_without_read_permission_there_is_no_tab_badge_or_nav_entry(db: PgPool) {
	let p = Page::scanned(db).await;
	let (_, with) = p.get(&p.app("")).await;
	assert_contains(page_main(&with), "ui-sev-group");
	assert_contains(page_main(&with), ">Security</a>");

	p.revoke("read", "scan").await;
	let (s, body) = p.get(&p.tab("")).await;
	assert_eq!(s, StatusCode::FORBIDDEN);
	assert_not_contains(&body, "CVE-2022-0001");
	let (s, _) = p.get(&p.security("")).await;
	assert_eq!(s, StatusCode::FORBIDDEN);
	let (_, without) = p.get(&p.app("")).await;
	assert_not_contains(&without, "ui-sev-group");
	assert_not_contains(page_main(&without), ">Security</a>");
	assert_not_contains(&without, &format!("href=\"/teams/{}/security\"", p.f.team));
}

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn scanning_ui_a_resource_never_scanned_says_so_and_a_scan_in_flight_polls(db: PgPool) {
	let f = fx(db).await;
	fake_agent(&f.state);
	let cookie = ui_login(&f.app, "alice").await;
	let p = Page { f, cookie };

	let (s, body) = p.get(&p.tab("")).await;
	assert_eq!(s, StatusCode::OK, "{body}");
	assert_contains(page_main(&body), "Not scanned yet");
	assert_not_contains(page_main(&body), "ui-sev-group");

	// Queued but not run: informational, and the page asks again by itself.
	let scan = p.f.queue(IMAGE).await;
	let status = p.security(&format!("/scans/{scan}/status"));
	let (_, body) = p.get(&p.tab("")).await;
	assert_contains(page_main(&body), "Scanning image");
	assert_contains(page_main(&body), &format!("/security/scans/{scan}/status"));
	let (_, still) = p.get(&status).await;
	assert_not_contains(&still, "window.location.reload()");

	// Once it is done the same poll reloads the page.
	p.f.run(scan).await.unwrap();
	let (_, done) = p.get(&status).await;
	assert_contains(&done, "window.location.reload()");
}

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn scanning_ui_scan_again_queues_one_and_needs_create_permission(db: PgPool) {
	let p = Page::scanned(db).await;
	let uri = format!(
		"/teams/{}/projects/{}/security/application/5001/scan",
		p.f.team, p.f.project
	);
	let before = count(&p.f.db, "image_scans").await;
	let (s, body) = p.post(&uri, &json!({})).await;
	assert_eq!(s, StatusCode::OK);
	assert_contains(&body, "window.location.reload()");
	assert_eq!(count(&p.f.db, "image_scans").await, before + 1);

	p.revoke("create", "scan").await;
	let (_, body) = p.post(&uri, &json!({})).await;
	assert_not_contains(&body, "window.location.reload()");
	assert_eq!(count(&p.f.db, "image_scans").await, before + 1);
	// And the button is not drawn.
	let (_, body) = p.get(&p.tab("")).await;
	assert_not_contains(page_main(&body), "Scan again");
}

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn scanning_ui_the_server_page_sets_scanning_and_its_limits(db: PgPool) {
	let p = Page::scanned(db).await;
	let server = format!("/teams/{}/servers/{}", p.f.team, p.f.server);
	let (_, body) = p.get(&server).await;
	assert_contains(page_main(&body), "Image scanning");
	assert_contains(page_main(&body), "data-bind:scan_enabled");

	let post = p.security(&format!("/servers/{}", p.f.server));
	let (_, body) = p
		.post(
			&post,
			&json!({ "scan_enabled": false, "scan_cpus": "0.5", "scan_memory": 2048 }),
		)
		.await;
	assert_contains(&body, "Scanning settings saved");
	let (on, cpus, mem) = server_scan_settings(&p.f.db, p.f.server).await;
	assert!(!on);
	assert!((cpus - 0.5).abs() < f64::EPSILON);
	assert_eq!(mem, 2048);
	let (_, body) = p.get(&p.tab("")).await;
	assert_contains(page_main(&body), "Scanning is off for this server");
	assert_not_contains(page_main(&body), "Scan again");

	// Out of range, and without the permission: refused, nothing changes.
	for form in [
		json!({ "scan_enabled": true, "scan_cpus": 64, "scan_memory": 2048 }),
		json!({ "scan_enabled": true, "scan_cpus": 1, "scan_memory": 12 }),
	] {
		let (_, body) = p.post(&post, &form).await;
		assert_contains(&body, "_scan_error");
	}
	p.revoke("update", "scan").await;
	let (_, body) = p
		.post(
			&post,
			&json!({ "scan_enabled": true, "scan_cpus": 1, "scan_memory": 1024 }),
		)
		.await;
	assert_contains(&body, "_scan_error");
	let (on, _, _) = server_scan_settings(&p.f.db, p.f.server).await;
	assert!(!on, "the forged post changed nothing");
	let (_, body) = p.get(&server).await;
	assert_contains(page_main(&body), "Only people who can update scanning");
}

#[test]
fn scanning_ui_the_deploy_line_says_what_the_scan_is_doing() {
	use podkit_core::scanning::{Badge, BadgeState, Counts, Severity};
	let found = |n: i64, act: i64| {
		let mut counts = Counts::default();
		for i in 0..n {
			counts.add(Severity::High, i < act);
		}
		Badge::found(counts)
	};
	let scanning = super::line::line_for_test(&Badge {
		state: BadgeState::Scanning,
		counts: Counts::default(),
	});
	assert!(scanning.contains("Scanning image"), "{scanning}");
	assert!(
		scanning.contains("data-on-interval"),
		"it polls while it runs: {scanning}"
	);

	let done = super::line::line_for_test(&found(2, 1));
	assert!(done.contains("Scanned: 2 high (1 act now)"), "{done}");
	assert!(!done.contains("data-on-interval"), "{done}");

	let failed = super::line::line_for_test(&Badge {
		state: BadgeState::Failed,
		counts: Counts::default(),
	});
	assert!(failed.contains("The deploy was not affected"), "{failed}");

	let never = super::line::line_for_test(&Badge::NONE);
	assert!(never.is_empty(), "{never}");
}
