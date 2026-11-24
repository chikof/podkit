//! The follow-ups that cut across slices (F5). The rest of this card's tests sit with the
//! slice they test, and all carry `followups` in their names:
//! `cargo test -p podkit-core -p podkit-ui --all-features -- followups`.
//!
//! * clone copies scheduled tasks: `environments::clone::tests::followups_clone_*`
//! * the MCP tool `run_cron_task`: `mcp::tests::followups_mcp_*`
//! * the resolver in every builder: `shared_env::tests::followups_shared_env_*`
//! * skip TLS verification: `uptime::tests::followups_uptime_*`,
//!   `podkit_ui::pages::uptime::tests::followups_uptime_*`
//! * the picker table: `podkit_ui::pages::projects::picker::tests::followups_picker_*`
//! * the foreign-team 404 table: `tests::foreign_team_404`
//! * live tests that skip: here.

use super::{live_podman_socket, socket_in};

#[test]
fn followups_live_tests_skip_when_there_is_no_private_podman_socket() {
	assert_eq!(socket_in(None), None);
	assert_eq!(socket_in(Some(String::new())), None);
	assert_eq!(socket_in(Some("  ".to_owned())), None);
	assert_eq!(
		socket_in(Some("/tmp/pk-x/podman.sock".to_owned())).as_deref(),
		Some("/tmp/pk-x/podman.sock")
	);
	// Whatever this machine has set, asking is never a panic.
	let _ = live_podman_socket("followups_live_tests_skip");
}

/// No live test may `expect` the variable: each asks [`live_podman_socket`] and returns.
#[test]
fn followups_no_live_test_panics_without_the_socket() {
	for (file, source) in [
		("cron/live_tests.rs", include_str!("../cron/live_tests.rs")),
		(
			"terminal/live_tests.rs",
			include_str!("../terminal/live_tests.rs"),
		),
		(
			"networks/tests/live.rs",
			include_str!("../networks/tests/live.rs"),
		),
		(
			"networks/tests/canvas_live.rs",
			include_str!("../networks/tests/canvas_live.rs"),
		),
		(
			"networks/tests/shared_env_live.rs",
			include_str!("../networks/tests/shared_env_live.rs"),
		),
	] {
		assert!(
			source.contains("live_podman_socket("),
			"{file} does not skip politely"
		);
		for line in source.lines() {
			assert!(
				!(line.contains("PODKIT_LIVE_PODMAN_SOCKET") && line.contains("env::var(") && {
					// The one that reads it for a failure message may use a default.
					!line.contains("unwrap_or_default")
				}),
				"{file}: reads the variable directly: {line}"
			);
		}
	}
}
