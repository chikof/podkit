//! The layer cache of image builds, and keeping it to a size.
//!
//! Podman keeps the layers of every build as untagged intermediate images; a later build
//! with the same steps and files reuses them. They are the cache. A layer that belongs to a
//! tagged image or to a container is part of that image and is not the cache's to remove
//! (Podman would refuse to remove it anyway). What is left, layers nothing uses any more,
//! is the cache: [`info`] measures it and [`prune`] removes the oldest of it until it fits a
//! budget.
//!
//! The size of a layer image in Podman's list is the size of the whole stack beneath it, so
//! a layer counts for its own part: its size less its parent's.

use std::collections::{HashMap, HashSet};

use bollard::models::ImageSummary;
use proto::{BuildCacheInfo, BuildCachePruned};

use crate::podman::{Podman, PodmanError};

/// A layer nothing uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layer {
	/// Image id.
	pub id: String,
	/// Parent image id; empty for a base.
	pub parent: String,
	/// Bytes this layer adds on top of its parent.
	pub bytes: u64,
	/// Unix seconds.
	pub created: i64,
}

fn is_tagged(image: &ImageSummary) -> bool {
	image
		.repo_tags
		.iter()
		.any(|t| !t.is_empty() && t != "<none>:<none>")
		|| image
			.repo_digests
			.iter()
			.any(|d| !d.is_empty() && !d.starts_with("<none>"))
}

/// The layers nothing uses: untagged, not used by a container, and not beneath a tagged or
/// used image.
#[must_use]
pub fn unused_layers(images: &[ImageSummary]) -> Vec<Layer> {
	let by_id: HashMap<&str, &ImageSummary> = images.iter().map(|i| (i.id.as_str(), i)).collect();
	// Everything beneath an image that is kept is kept with it.
	let mut pinned: HashSet<&str> = HashSet::new();
	for image in images.iter().filter(|i| is_tagged(i) || i.containers > 0) {
		let mut at = image.id.as_str();
		while pinned.insert(at) {
			match by_id.get(at).map(|i| i.parent_id.as_str()) {
				Some(parent) if !parent.is_empty() => at = parent,
				_ => break,
			}
		}
	}
	images
		.iter()
		.filter(|i| !pinned.contains(i.id.as_str()))
		.map(|i| {
			let below = by_id
				.get(i.parent_id.as_str())
				.map_or(0, |parent| parent.size);
			Layer {
				id: i.id.clone(),
				parent: i.parent_id.clone(),
				bytes: u64::try_from((i.size - below).max(0)).unwrap_or(0),
				created: i.created,
			}
		})
		.collect()
}

/// Which layers to remove so that at most `max_bytes` remain: the oldest first, as many as it
/// takes. They come back newest first, which is the order to remove them in: a layer is
/// younger than its parent, and Podman refuses to remove a parent that still has a child.
#[must_use]
pub fn prune_plan(layers: &[Layer], max_bytes: u64) -> Vec<Layer> {
	let mut total: u64 = layers.iter().map(|l| l.bytes).sum();
	let mut oldest_first: Vec<&Layer> = layers.iter().collect();
	oldest_first.sort_by(|a, b| a.created.cmp(&b.created).then_with(|| a.id.cmp(&b.id)));
	let mut chosen = Vec::new();
	for layer in oldest_first {
		if total <= max_bytes {
			break;
		}
		total = total.saturating_sub(layer.bytes);
		chosen.push(layer.clone());
	}
	chosen.sort_by(|a, b| b.created.cmp(&a.created).then_with(|| b.id.cmp(&a.id)));
	chosen
}

/// What the cache takes now.
///
/// # Errors
/// Podman is unreachable.
pub async fn info(podman: &Podman) -> Result<BuildCacheInfo, PodmanError> {
	let layers = unused_layers(&podman.all_images().await?);
	Ok(BuildCacheInfo {
		bytes: layers.iter().map(|l| l.bytes).sum(),
		layers: u32::try_from(layers.len()).unwrap_or(u32::MAX),
	})
}

/// Removes the oldest unused layers until at most `max_bytes` remain. A layer that cannot be
/// removed (a build is using it) is left; the answer says what was.
///
/// # Errors
/// Podman is unreachable.
pub async fn prune(podman: &Podman, max_bytes: u64) -> Result<BuildCachePruned, PodmanError> {
	let mut removed = 0_u32;
	let mut freed = 0_u64;
	// Removing a layer can free its parent for the next pass.
	for _ in 0..8 {
		let layers = unused_layers(&podman.all_images().await?);
		let plan = prune_plan(&layers, max_bytes);
		if plan.is_empty() {
			break;
		}
		let mut progress = false;
		for layer in plan {
			match podman.remove_image(&layer.id).await {
				Ok(()) => {
					removed += 1;
					freed += layer.bytes;
					progress = true;
				}
				Err(e) => tracing::debug!(layer = %layer.id, error = %e, "cache layer kept"),
			}
		}
		if !progress {
			break;
		}
	}
	let remaining = info(podman).await?.bytes;
	Ok(BuildCachePruned {
		removed,
		freed_bytes: freed,
		remaining_bytes: remaining,
	})
}

#[cfg(test)]
mod tests {
	use super::*;

	fn image(id: &str, parent: &str, tags: &[&str], size: i64, created: i64) -> ImageSummary {
		ImageSummary {
			id: id.into(),
			parent_id: parent.into(),
			repo_tags: tags.iter().map(|t| (*t).to_string()).collect(),
			size,
			created,
			containers: 0,
			..Default::default()
		}
	}

	fn layer(id: &str, bytes: u64, created: i64) -> Layer {
		Layer {
			id: id.into(),
			parent: String::new(),
			bytes,
			created,
		}
	}

	#[test]
	fn build_cache_counts_only_what_nothing_uses() {
		let images = vec![
			// A tagged image and the layers it stands on.
			image("app", "mid", &["podkit-app-1:abc"], 300, 30),
			image("mid", "base", &[], 200, 20),
			image("base", "", &[], 100, 10),
			// An older build whose tag is gone: a stack of two layers.
			image("old2", "old1", &[], 250, 25),
			image("old1", "base", &[], 160, 15),
		];
		let mut unused = unused_layers(&images);
		unused.sort_by(|a, b| a.id.cmp(&b.id));
		let ids: Vec<&str> = unused.iter().map(|l| l.id.as_str()).collect();
		assert_eq!(ids, ["old1", "old2"], "the tagged image's own stack stays");
		// Each counts for what it adds to its parent.
		assert_eq!(unused[0].bytes, 60);
		assert_eq!(unused[1].bytes, 90);
	}

	#[test]
	fn build_cache_a_layer_of_a_container_is_pinned() {
		let mut used = image("c", "b", &[], 50, 5);
		used.containers = 1;
		let images = vec![
			used,
			image("b", "", &[], 40, 4),
			image("free", "", &[], 10, 6),
		];
		let ids: Vec<String> = unused_layers(&images).into_iter().map(|l| l.id).collect();
		assert_eq!(ids, ["free"]);
	}

	#[test]
	fn build_cache_prune_drops_the_oldest_first_until_it_fits() {
		let layers = vec![layer("a", 100, 1), layer("b", 100, 2), layer("c", 100, 3)];
		let plan = prune_plan(&layers, 150);
		// Two must go to fit 150, and the two oldest are the ones.
		let ids: Vec<&str> = plan.iter().map(|l| l.id.as_str()).collect();
		assert_eq!(ids.len(), 2);
		assert!(ids.contains(&"a") && ids.contains(&"b"), "{ids:?}");
		// Removal order is newest first so children go before their parents.
		assert_eq!(ids, ["b", "a"]);
	}

	#[test]
	fn build_cache_prune_leaves_a_cache_that_already_fits() {
		let layers = vec![layer("a", 100, 1), layer("b", 100, 2)];
		assert!(prune_plan(&layers, 200).is_empty());
		assert!(prune_plan(&layers, 500).is_empty());
		assert!(prune_plan(&[], 0).is_empty());
	}

	#[test]
	fn build_cache_prune_to_zero_takes_everything() {
		let layers = vec![layer("a", 10, 1), layer("b", 20, 2)];
		assert_eq!(prune_plan(&layers, 0).len(), 2);
	}
}
