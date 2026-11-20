//! `GET /v1/events?since=&until=`: restarts, OOM kills and crashes of podkit containers, read
//! from Podman's event log for a window of time.
//!
//! The agent keeps no subscription and no buffer: the control plane asks for the window since
//! its last question, and Podman answers from its own log. Event shapes follow the Docker
//! Engine API (<https://docs.docker.com/reference/api/engine/version/v1.43/#tag/System/operation/SystemEvents>),
//! which Podman's compat API mirrors: `Action` is `die` (Podman's native name is `died`),
//! `oom` or `restart`, and `Actor.Attributes` carries the container's labels, its `name` and,
//! for a death, `exitCode`.

use bollard::models::EventMessage;
use proto::{ContainerEvent, ContainerEventKind};

use crate::podman::Podman;
use crate::stats::resource_of;

const SERVICE_LABEL: &str = "podkit.service";
/// Exit statuses of a deliberate stop (SIGTERM). A user stopping a container is not a crash.
const STOPPED_BY_REQUEST: i64 = 143;

/// What `event` says happened to a podkit container, if the charts mark it. A death with
/// status 0 or 143 is a normal stop, not an event.
#[must_use]
pub fn parse_event(event: &EventMessage) -> Option<ContainerEvent> {
	let actor = event.actor.as_ref()?;
	let attributes = actor.attributes.as_ref()?;
	let resource = resource_of(attributes.get(SERVICE_LABEL)?)?;
	let (kind, exit_code) = match event.action.as_deref()? {
		"die" | "died" => {
			let code: i64 = attributes.get("exitCode")?.trim().parse().ok()?;
			if code == 0 || code == STOPPED_BY_REQUEST {
				return None;
			}
			(ContainerEventKind::Exit, Some(code))
		}
		"oom" => (ContainerEventKind::Oom, None),
		"restart" => (ContainerEventKind::Restart, None),
		_ => return None,
	};
	Some(ContainerEvent {
		at: event
			.time
			.or_else(|| event.time_nano.map(|n| n / 1_000_000_000))?,
		kind,
		resource,
		container: attributes.get("name").cloned().unwrap_or_default(),
		exit_code,
	})
}

/// The marked events `since`..`until`, oldest first.
///
/// # Errors
/// Podman is unreachable.
pub async fn between(
	podman: &Podman,
	since: i64,
	until: i64,
) -> Result<Vec<ContainerEvent>, crate::podman::PodmanError> {
	let mut events: Vec<ContainerEvent> = podman
		.events_between(since, until)
		.await?
		.iter()
		.filter_map(parse_event)
		.collect();
	events.sort_by_key(|e| e.at);
	Ok(events)
}

#[cfg(test)]
mod tests {
	use std::collections::HashMap;

	use bollard::models::EventActor;

	use super::*;

	fn event(action: &str, attributes: &[(&str, &str)]) -> EventMessage {
		EventMessage {
			action: Some(action.to_owned()),
			actor: Some(EventActor {
				id: Some("abc".to_owned()),
				attributes: Some(
					attributes
						.iter()
						.map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
						.collect::<HashMap<_, _>>(),
				),
			}),
			time: Some(1_700_000_000),
			..Default::default()
		}
	}

	const APP: [(&str, &str); 2] = [("podkit.service", "app-7"), ("name", "podkit-app-7-12")];

	#[test]
	fn events_stream_reports_a_crash_with_its_exit_code() {
		let mut attrs = APP.to_vec();
		attrs.push(("exitCode", "1"));
		for action in ["die", "died"] {
			let parsed = parse_event(&event(action, &attrs)).unwrap();
			assert_eq!(parsed.kind, ContainerEventKind::Exit);
			assert_eq!(parsed.exit_code, Some(1));
			assert_eq!(parsed.at, 1_700_000_000);
			assert_eq!(parsed.container, "podkit-app-7-12");
			assert_eq!(parsed.resource.kind, "application");
			assert_eq!(parsed.resource.id, 7);
		}
	}

	#[test]
	fn events_stream_ignores_a_clean_exit_and_a_requested_stop() {
		for code in ["0", "143"] {
			let mut attrs = APP.to_vec();
			attrs.push(("exitCode", code));
			assert_eq!(parse_event(&event("die", &attrs)), None, "exit {code}");
		}
	}

	#[test]
	fn events_stream_reports_oom_kills_and_restarts() {
		let oom = parse_event(&event("oom", &APP)).unwrap();
		assert_eq!(oom.kind, ContainerEventKind::Oom);
		assert_eq!(oom.exit_code, None);
		let restart = parse_event(&event("restart", &APP)).unwrap();
		assert_eq!(restart.kind, ContainerEventKind::Restart);
		// A 137 after an OOM kill is a real exit and is kept next to the oom event.
		let mut attrs = APP.to_vec();
		attrs.push(("exitCode", "137"));
		assert_eq!(
			parse_event(&event("die", &attrs)).unwrap().exit_code,
			Some(137)
		);
	}

	#[test]
	fn events_stream_skips_containers_that_are_not_podkits_and_other_actions() {
		assert_eq!(
			parse_event(&event("oom", &[("name", "someone-elses")])),
			None
		);
		assert_eq!(parse_event(&event("start", &APP)), None);
		let mut stack = vec![
			("podkit.service", "stack-3-web"),
			("name", "podkit-stack-3-web-1"),
		];
		stack.push(("exitCode", "2"));
		let parsed = parse_event(&event("die", &stack)).unwrap();
		assert_eq!(parsed.resource.kind, "stack");
		assert_eq!(parsed.resource.service.as_deref(), Some("web"));
		// Nanosecond timestamps work when the second one is absent.
		let mut nano = event("oom", &APP);
		nano.time = None;
		nano.time_nano = Some(1_700_000_005_000_000_000);
		assert_eq!(parse_event(&nano).unwrap().at, 1_700_000_005);
	}
}
