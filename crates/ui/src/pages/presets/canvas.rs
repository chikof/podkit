//! A preset drawn as the project canvas draws a project: environments as frames, what each
//! holds as tiles, networks and model links as lines. It is the canvas's layout and its CSS
//! with nothing to click: none of it exists until the preset is applied.

use maud::{Markup, html};

use crate::components::{LogoSize, icon};
use crate::pages::projects::cards::{logo_row, resource_logo};
use podkit_core::projects::graph::layout::{COL_GAP, layout};
use podkit_core::projects::graph::{EdgeKind, Graph};
use podkit_core::projects::overview::ResourceLogo;

/// The canvas of the preset `file`; nothing when the file is not a preset or holds nothing.
pub fn canvas(file: &str) -> Markup {
	podkit_core::iac::doc::parse(file)
		.ok()
		.and_then(|doc| podkit_core::presets::graph::graph(&doc))
		.filter(|g| !g.nodes.is_empty())
		.map_or_else(|| html! {}, |graph| world(&graph))
}

/// What a preset holds, for its card in the list: the logo of each part, and the count of
/// its environments and of each kind of part ("2 environments · 4 databases"). `None` when
/// the file is not a preset or holds nothing.
pub fn held(file: &str) -> Option<(Markup, String)> {
	let graph = podkit_core::iac::doc::parse(file)
		.ok()
		.and_then(|doc| podkit_core::presets::graph::graph(&doc))
		.filter(|g| !g.nodes.is_empty())?;
	let logos: Vec<ResourceLogo> = graph
		.nodes
		.iter()
		.map(|n| ResourceLogo {
			kind: n.key.kind,
			name: n.name.clone(),
			mark: n.mark,
		})
		.collect();
	let count = |n: usize, one: &str| format!("{n} {one}{}", if n == 1 { "" } else { "s" });
	let mut counts = vec![count(graph.environments.len(), "environment")];
	let mut kinds: Vec<_> = graph.nodes.iter().map(|n| n.key.kind).collect();
	kinds.sort();
	kinds.dedup();
	for kind in kinds {
		let n = graph.nodes.iter().filter(|n| n.key.kind == kind).count();
		counts.push(count(n, &kind.label().to_lowercase()));
	}
	Some((logo_row(&logos), counts.join(" \u{b7} ")))
}

fn world(graph: &Graph) -> Markup {
	let l = layout(graph);
	let rect = |r: &podkit_core::projects::graph::layout::Rect| {
		format!("--x:{}px;--y:{}px;--w:{}px;--h:{}px", r.x, r.y, r.w, r.h)
	};
	html! {
		section class="ui-cv ui-cv-static" aria-label="What this preset makes" data-labels="on" data-nets="on" {
			div class="ui-cv-view" tabindex="0" {
				div class="ui-cv-world"
					style={ "width:" (l.width) "px;height:" (l.height) "px;--cv-gap:" (COL_GAP) "px" } {
					@for frame in &l.envs {
						@if let Some(env) = graph.environment(frame.env) {
							section class="ui-cv-env" aria-label={ (env.name) " environment" } style=(rect(&frame.rect)) {
								div class="ui-cv-env-h" { h3 class="ui-cv-env-t" { (env.name) } }
							}
						}
					}
					@for frame in &l.nets {
						div class="ui-cv-net" style=(rect(&frame.rect)) {
							span class="ui-cv-net-l" { (icon("network")) (frame.net.label) }
						}
					}
					svg class="ui-cv-edges" width=(l.width) height=(l.height) aria-hidden="true" {
						defs {
							marker id="cv-arrowhead" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="7" markerHeight="7" orient="auto" {
								path class="ui-cv-arrow" d="M1 1L9 5L1 9" fill="none" stroke="context-stroke" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" {}
							}
						}
						@for (e, geom) in graph.edges.iter().zip(&l.edges) {
							@if !geom.d.is_empty() {
								path class="ui-cv-edge" d=(geom.d) data-kind=(e.kind.slug()) {}
							}
						}
					}
					ul class="ui-cv-nodes" aria-label="What it makes" {
						@for n in &graph.nodes {
							@let (x, y) = l.pos.get(&n.key).copied().unwrap_or((0, 0));
							li class="ui-cv-node" style={ "--x:" (x) "px;--y:" (y) "px" } {
								div class="ui-cv-head" {
									span class="ui-cv-ic" { (resource_logo(n.mark, &n.name, LogoSize::Sm)) }
									span class="ui-cv-open" { (n.name) }
								}
								span class="ui-cv-sub" {
									span class="sr-only" { (n.key.kind.label()) ". " }
									(n.detail)
								}
							}
						}
					}
					div class="ui-cv-elabels" {
						@for (e, geom) in graph.edges.iter().zip(&l.edges) {
							@if !geom.d.is_empty() {
								span class="ui-cv-elabel" style={ "--x:" (geom.mid.0) "px;--y:" (geom.mid.1) "px" } {
									@if e.kind == EdgeKind::Gateway { (icon("brain")) }
									(e.label)
								}
							}
						}
					}
				}
			}
		}
	}
}
