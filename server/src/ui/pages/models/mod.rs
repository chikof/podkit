//! Model pages: the team's models and downloads, the catalog, the create
//! flow (model, server, recipe, download, start), a model's page with its
//! logs, links and playground, and the Hugging Face token.
//!
//! Handlers here call the `catalog`, `model_files`, `models` and `links`
//! services, which authorize, scope and validate; nothing here reads the
//! database or checks RBAC itself. Model output and Hugging Face names are
//! untrusted and only ever pass through maud's escaping.

use axum::Router;
use maud::{Markup, html};
use serde::Deserialize;

use crate::catalog::fit::{Fit, FitKind};
use crate::ids::{ModelFileId, ModelId, ProjectId, TeamId};
use crate::state::AppState;
use crate::ui::components::{LogoSize, Tone, logo};
use crate::ui::extract::TeamCtx;
use crate::ui::layout::Crumb;

mod catalog;
mod detail;
mod flow;
mod index;
mod playground;
mod settings;
mod setup;
#[cfg(test)]
mod tests;

pub fn router() -> Router<AppState> {
	Router::new()
		.merge(index::router())
		.merge(catalog::router())
		.merge(setup::router())
		.merge(flow::router())
		.merge(detail::router())
		.merge(playground::router())
		.merge(settings::router())
}

fn models_url(team: TeamId) -> String {
	format!("/teams/{team}/models")
}

fn catalog_url(team: TeamId) -> String {
	format!("/teams/{team}/models/catalog")
}

fn download_url(team: TeamId, file: ModelFileId) -> String {
	format!("/teams/{team}/models/downloads/{file}")
}

pub(in crate::ui) fn model_url(team: TeamId, project: ProjectId, model: ModelId) -> String {
	format!("/teams/{team}/projects/{project}/models/{model}")
}

fn crumbs<'a>(ctx: &TeamCtx, trail: Vec<Crumb<'a>>) -> Vec<Crumb<'a>> {
	let mut crumbs: Vec<Crumb<'a>> = vec![("Models", Some(models_url(ctx.team.id)))];
	crumbs.extend(trail);
	crumbs
}

/// Where a new model goes, when the create-resource picker said so: it links to the
/// catalog with `?project=<id>&environment=<id>`, and the flow carries both through to
/// the model it creates. Both are parsed leniently; the service checks that the
/// environment belongs to the project and to the caller's team.
#[derive(Debug, Default, Clone, Deserialize)]
pub(super) struct Target {
	pub project: Option<String>,
	pub environment: Option<String>,
}

impl Target {
	pub fn project_id(&self) -> Option<i64> {
		self.project.as_deref().and_then(|p| p.parse().ok())
	}

	pub fn environment_id(&self) -> Option<i64> {
		self.environment.as_deref().and_then(|e| e.parse().ok())
	}

	/// `project=3`, `environment=7`: the parts that are set, for a URL's query.
	pub fn pairs(&self) -> Vec<String> {
		let mut out = Vec::new();
		if let Some(p) = self.project_id() {
			out.push(format!("project={p}"));
		}
		if let Some(e) = self.environment_id() {
			out.push(format!("environment={e}"));
		}
		out
	}
}

/// The steps of creating a model, in order: the first four are the set-up (a dialog on the
/// catalog), then the download's progress page, then the start form.
fn flow_steps() -> [&'static str; 6] {
	[
		"Model",
		"Server",
		"Recipe",
		"Overrides",
		"Download",
		"Start",
	]
}

/// The mark of a model's vendor (`Qwen`, `Mistral`, ...): a monogram until podkit has the
/// vendor's logo. When the logo registry lands, this is the one place that maps a family
/// to its mark.
fn family_logo(family: &str, size: LogoSize) -> Markup {
	logo(family, size)
}

/// `1.5 GB`, `340 MB`: decimal units, like Hugging Face and `df`.
fn human_bytes(bytes: u64) -> String {
	const UNITS: [(&str, f64); 3] = [("GB", 1e9), ("MB", 1e6), ("KB", 1e3)];
	let n = bytes as f64;
	for (unit, size) in UNITS {
		if n >= size {
			let value = n / size;
			return if value >= 100.0 || unit == "MB" && value >= 10.0 {
				format!("{value:.0} {unit}")
			} else {
				format!("{value:.1} {unit}")
			};
		}
	}
	format!("{bytes} B")
}

/// The words and tone of a fit verdict.
fn fit_label(kind: FitKind) -> (&'static str, Tone) {
	match kind {
		FitKind::GpuFull => ("Fits on GPU", Tone::Success),
		FitKind::GpuPlusRam => ("GPU + RAM", Tone::Warning),
		FitKind::CpuOnly => ("CPU only", Tone::Progress),
		FitKind::DoesNotFit => ("Won't fit", Tone::Danger),
	}
}

/// A pill with the verdict; the reason is its tooltip.
fn fit_badge(fit: &Fit) -> Markup {
	let (label, tone) = fit_label(fit.kind);
	html! {
		span class=(tone.badge_class()) title=(fit.explanation) { (label) }
	}
}

/// Parameters as written on a card: `4B`, `35B · 3B active`.
fn params_text(params_b: f64, active_b: Option<f64>) -> String {
	let n = |v: f64| {
		if (v - v.round()).abs() < 0.05 {
			format!("{v:.0}B")
		} else {
			format!("{v:.1}B")
		}
	};
	match active_b {
		Some(active) => format!("{} · {} active", n(params_b), n(active)),
		None => n(params_b),
	}
}

#[cfg(test)]
mod unit {
	use super::*;

	#[test]
	fn sizes_and_parameters_read_naturally() {
		assert_eq!(human_bytes(0), "0 B");
		assert_eq!(human_bytes(1_500), "1.5 KB");
		assert_eq!(human_bytes(340_000_000), "340 MB");
		assert_eq!(human_bytes(2_497_280_256), "2.5 GB");
		assert_eq!(human_bytes(180_000_000_000), "180 GB");
		assert_eq!(params_text(4.0, None), "4B");
		assert_eq!(params_text(30.5, Some(3.0)), "30.5B · 3B active");
	}
}
