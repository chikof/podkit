//! Small pieces the projects grid and the environment view share: a row of resource
//! logos, a resource's status line, relative times.

use maud::{Markup, html};
use time::{OffsetDateTime, UtcOffset};

use crate::projects::overview::{ResourceCard, ResourceLogo, ResourceStatus};
use crate::ui::components::{LogoSize, Priority, Tone, image_logo, logo};
use crate::ui::status::Status;

/// How many logos a project card shows before "+N".
const LOGOS_SHOWN: usize = 5;

/// The logo of a resource: its mark, or the monogram of its name.
pub fn resource_logo(mark: Option<&str>, name: &str, size: LogoSize) -> Markup {
	logo(mark.unwrap_or(name), size)
}

/// A resource card's logo: a template stack's own image, else its mark or monogram.
pub fn card_logo(card: &ResourceCard) -> Markup {
	match &card.logo_url {
		Some(url) => image_logo(url, LogoSize::Md, Priority::Normal),
		None => resource_logo(card.mark, &card.name, LogoSize::Md),
	}
}

/// Up to five logos, one per resource, then "+N". The names are there for a screen reader,
/// since the tiles themselves are decorative.
pub fn logo_row(logos: &[ResourceLogo]) -> Markup {
	let more = logos.len().saturating_sub(LOGOS_SHOWN);
	html! {
		div class="flex items-center gap-1.5" {
			@for l in logos.iter().take(LOGOS_SHOWN) {
				(resource_logo(l.mark, &l.name, LogoSize::Sm))
			}
			@if more > 0 {
				span class="text-xs font-medium text-muted-foreground" { "+" (more) }
			}
			span class="sr-only" {
				"Contains "
				@for (i, l) in logos.iter().enumerate() {
					@if i > 0 { ", " }
					(l.name)
				}
			}
		}
	}
}

fn parts(status: ResourceStatus) -> (&'static str, Tone, bool) {
	match status {
		ResourceStatus::NotDeployed => ("Not deployed", Tone::Neutral, false),
		ResourceStatus::Deployment(s) => (s.label(), s.tone(), s.in_flight()),
		ResourceStatus::Database(s) => (s.label(), s.tone(), s.in_flight()),
		ResourceStatus::Model(s) => (s.label(), s.tone(), s.in_flight()),
		ResourceStatus::Stack(s) => (s.label(), s.tone(), s.in_flight()),
	}
}

/// Whether the status is expected to change by itself, so the page keeps polling.
pub fn in_flight(status: ResourceStatus) -> bool {
	parts(status).2
}

/// The `data-status` key the stylesheet draws a shape and a color from. A kind's own words
/// (Queued, Starting, Removing) fold into the seven Quiet statuses by what they mean.
fn status_key(label: &str, tone: Tone) -> &'static str {
	match (label, tone) {
		("Deploying", _) => "deploying",
		("Pending" | "Queued" | "Not deployed", _) => "pending",
		(_, Tone::Success) => "running",
		(_, Tone::Progress | Tone::Warning) => "building",
		(_, Tone::Danger) => "failed",
		(_, Tone::Neutral) => "stopped",
	}
}

/// The status key of a resource, for `data-status` on its row.
pub fn status_data(status: ResourceStatus) -> &'static str {
	let (label, tone, _) = parts(status);
	status_key(label, tone)
}

/// A shaped dot and the status word. The word is always there, so color is never the only
/// signal. `flip` plays the short change animation: it marks a status that just changed.
pub fn status_badge(status: ResourceStatus, flip: bool) -> Markup {
	let (label, tone, _) = parts(status);
	html! {
		span class="ui-badge" data-status=(status_key(label, tone)) data-flip[flip] {
			span class="ui-dot" aria-hidden="true" {}
			span { (label) }
		}
	}
}

/// A dot and the status word, at rest.
pub fn status_line(status: ResourceStatus) -> Markup {
	status_badge(status, false)
}

/// `2026-11-18 03:00 UTC`, for a tooltip.
fn absolute(t: OffsetDateTime) -> String {
	let t = t.to_offset(UtcOffset::UTC);
	format!(
		"{:04}-{:02}-{:02} {:02}:{:02} UTC",
		t.year(),
		u8::from(t.month()),
		t.day(),
		t.hour(),
		t.minute()
	)
}

/// "5 min ago" and the like; a date once it is more than a month old. Negative gaps
/// (clock skew) read as "just now".
fn relative_text(at: OffsetDateTime, now: OffsetDateTime) -> String {
	let secs = (now - at).whole_seconds();
	match secs {
		..60 => "just now".to_owned(),
		60..3600 => format!("{} min ago", secs / 60),
		3600..86_400 => format!("{} h ago", secs / 3600),
		86_400..2_592_000 => format!("{} d ago", secs / 86_400),
		_ => absolute(at)[..10].to_owned(),
	}
}

/// A `<time>` with the relative text, the exact time as its tooltip.
pub fn relative_time(at: OffsetDateTime, now: OffsetDateTime) -> Markup {
	let utc = at.to_offset(UtcOffset::UTC);
	let iso = format!(
		"{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
		utc.year(),
		u8::from(utc.month()),
		utc.day(),
		utc.hour(),
		utc.minute(),
		utc.second()
	);
	html! {
		time datetime=(iso) title=(absolute(at)) { (relative_text(at, now)) }
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::databases::DatabaseStatus;
	use crate::deployments::DeploymentStatus;
	use crate::projects::overview::ResourceKind;
	use time::Duration;

	fn logos(n: usize) -> Vec<ResourceLogo> {
		(0..n)
			.map(|i| ResourceLogo {
				kind: ResourceKind::Application,
				name: format!("app{i}"),
				mark: None,
			})
			.collect()
	}

	#[test]
	fn five_logos_then_a_count_of_the_rest() {
		let html = logo_row(&logos(8)).into_string();
		assert_eq!(html.matches("data-logo=").count(), 5, "{html}");
		assert!(html.contains("+3"), "{html}");
		assert!(
			html.contains("app7"),
			"the names stay for screen readers: {html}"
		);
		let html = logo_row(&logos(5)).into_string();
		assert!(!html.contains('+'), "{html}");
	}

	#[test]
	fn a_resource_without_a_mark_gets_its_monogram_and_the_name_is_escaped() {
		let l = ResourceLogo {
			kind: ResourceKind::Application,
			name: "<script>alert(1)</script>".into(),
			mark: None,
		};
		let html = logo_row(&[l]).into_string();
		assert!(html.contains("data-logo=\"monogram\""), "{html}");
		assert!(!html.contains("<script>"), "{html}");
	}

	#[test]
	fn the_status_always_carries_its_word() {
		let running =
			status_line(ResourceStatus::Deployment(DeploymentStatus::Running)).into_string();
		assert!(
			running.contains("data-status=\"running\"") && running.contains("Running"),
			"{running}"
		);
		assert!(!running.contains("data-flip"), "{running}");
		let none = status_line(ResourceStatus::NotDeployed).into_string();
		assert!(none.contains("Not deployed"), "{none}");
		assert!(none.contains("data-status=\"pending\""), "{none}");
		let flipped =
			status_badge(ResourceStatus::Deployment(DeploymentStatus::Running), true).into_string();
		assert!(flipped.contains("data-flip"), "{flipped}");
		let building =
			status_line(ResourceStatus::Deployment(DeploymentStatus::Building)).into_string();
		assert!(building.contains("data-status=\"building\""), "{building}");
		let deploying =
			status_line(ResourceStatus::Deployment(DeploymentStatus::Deploying)).into_string();
		assert!(
			deploying.contains("data-status=\"deploying\""),
			"{deploying}"
		);
		let failed =
			status_line(ResourceStatus::Deployment(DeploymentStatus::Crashed)).into_string();
		assert!(failed.contains("data-status=\"failed\""), "{failed}");
		assert!(in_flight(ResourceStatus::Database(DatabaseStatus::Pending)));
		assert!(!in_flight(ResourceStatus::Database(
			DatabaseStatus::Running
		)));
		assert!(!in_flight(ResourceStatus::NotDeployed));
	}

	#[test]
	fn times_read_relatively_and_fall_back_to_a_date() {
		let now = OffsetDateTime::from_unix_timestamp(1_800_000_000).unwrap();
		let ago = |d: Duration| relative_text(now - d, now);
		assert_eq!(ago(Duration::seconds(5)), "just now");
		assert_eq!(ago(Duration::minutes(5)), "5 min ago");
		assert_eq!(ago(Duration::hours(3)), "3 h ago");
		assert_eq!(ago(Duration::days(2)), "2 d ago");
		assert_eq!(ago(Duration::days(60)).len(), 10);
		assert_eq!(relative_text(now + Duration::hours(1), now), "just now");
		let html = relative_time(now - Duration::minutes(5), now).into_string();
		assert!(html.contains("datetime=\"2027-01-15T"), "{html}");
		assert!(html.contains("title=\"2027-01-15"), "{html}");
	}
}
