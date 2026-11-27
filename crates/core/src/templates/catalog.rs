//! The template catalog on disk, and fetching a new one.
//!
//! Templates are not in the binary. They come from one place, the podkit-templates
//! repository, whose website publishes `index.json` and `catalog.tar.gz`
//! (its `docs/catalog.md`). An administrator asks for a refresh; podkit downloads both,
//! checks the archive against the hash in the index, unpacks it under
//! `<data dir>/catalog` and reads the gallery from there, with no network, until the next
//! refresh.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use flate2::read::GzDecoder;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::registry;
use crate::error::ServerError;
use crate::net::outbound::read_capped;
use crate::state::AppState;

/// The index format this podkit reads.
const FORMAT: u32 = 1;
const MAX_INDEX_BYTES: usize = 8 << 20;
const MAX_ARCHIVE_BYTES: usize = 64 << 20;
/// What the archive may unpack to: a small archive can claim to hold far more.
const MAX_UNPACKED_BYTES: u64 = 256 << 20;
/// Files in an archive: three a template, with room for ten times today's catalog.
const MAX_ENTRIES: usize = 20_000;
const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

#[derive(Deserialize)]
struct Index {
	format: u32,
	commit: String,
	archive: Archive,
	#[serde(default)]
	templates: Vec<Entry>,
	/// Carried whole in the index: a preset is one small file.
	#[serde(default)]
	presets: Vec<registry::CatalogPreset>,
}

#[derive(Deserialize)]
struct Entry {
	id: String,
	revision: String,
}

impl Index {
	fn read(self, dir: &Path) -> registry::Registry {
		let revisions = self
			.templates
			.into_iter()
			// `template_deploys.revision` holds 32 characters; a longer one is no revision.
			.filter(|e| e.revision.len() <= 32)
			.map(|e| (e.id, e.revision))
			.collect();
		let mut registry = registry::read(dir, self.commit, &revisions);
		for preset in self.presets {
			let id_like = (1..=64).contains(&preset.id.len())
				&& preset
					.id
					.bytes()
					.all(|b| b.is_ascii_alphanumeric() || b == b'-');
			let refused = if !id_like {
				Err("its id is not one".to_owned())
			} else if preset.file.len() > crate::presets::service::MAX_FILE {
				Err("its file is larger than a kept preset may be".to_owned())
			} else if registry.presets.iter().any(|p| p.id == preset.id) {
				Err("another preset has this id".to_owned())
			} else {
				// The same reading an apply does: one this podkit cannot apply is left out.
				crate::iac::doc::parse(&preset.file)
					.map_err(|e| e.to_string())
					.and_then(|doc| crate::presets::needs(&doc).map(drop))
			};
			match refused {
				Ok(()) => registry.presets.push(preset),
				// A parser quotes the line it stopped at, and an id is whatever was sent:
				// neither is repeated at length to whoever reads the refresh's answer.
				Err(why) => registry.errors.push((
					format!("preset {:?}", short(&preset.id, 80)),
					short(&why, 300),
				)),
			}
		}
		registry
	}
}

/// `text`, cut at `max` characters.
fn short(text: &str, max: usize) -> String {
	match text.char_indices().nth(max) {
		Some((end, _)) => format!("{}\u{2026}", &text[..end]),
		None => text.to_owned(),
	}
}

#[derive(Deserialize)]
struct Archive {
	name: String,
	sha256: String,
}

/// What a refresh brought in.
#[derive(Debug, Serialize)]
pub struct Refreshed {
	/// Commit of the template repository.
	pub commit: String,
	/// Templates in the gallery.
	pub templates: usize,
	/// Templates this podkit could not load, with why. They are left out.
	pub skipped: Vec<(String, String)>,
}

fn dir(data_dir: &Path) -> PathBuf {
	data_dir.join("catalog")
}

/// Reads the catalog a previous refresh left in `data_dir`, if any. Called once at start.
pub fn open(data_dir: &Path) {
	let dir = dir(data_dir);
	recover(&dir);
	if let Some(index) = index_of(&dir) {
		registry::install(index.read(&dir));
	}
}

/// `server check-templates <dir>`: loads every template under `<dir>/templates` and every
/// preset under `<dir>/presets` the way a refresh does, and says which do not load. The template repository's checks cannot know
/// that; this is how its catalog is tried against a podkit before it is published.
#[must_use]
pub fn check_cli() -> Option<i32> {
	let args: Vec<String> = std::env::args().collect();
	if args.get(1).map(String::as_str) != Some("check-templates") {
		return None;
	}
	let Some(dir) = args.get(2) else {
		eprintln!("usage: server check-templates <dir with a templates/ folder>");
		return Some(2);
	};
	let registry = registry::read(
		Path::new(dir),
		String::new(),
		&std::collections::BTreeMap::new(),
	);
	for (id, why) in &registry.errors {
		eprintln!("{id}: {why}");
	}
	println!(
		"{} templates load, {} do not",
		registry.templates.len(),
		registry.errors.len()
	);
	// Presets too, as an apply takes them: `presets/<id>/preset.toml`.
	let (mut presets, mut unreadable) = (0, 0);
	let mut folders: Vec<_> = std::fs::read_dir(Path::new(dir).join("presets"))
		.map(|d| d.flatten().map(|e| e.path()).collect())
		.unwrap_or_default();
	folders.sort();
	for folder in folders.iter().filter(|f| f.is_dir()) {
		// Read, and planned as a new project: a parser lets through what the plan refuses.
		let read = std::fs::read_to_string(folder.join("preset.toml"))
			.map_err(|e| e.to_string())
			.and_then(|text| crate::presets::dry_run(&text))
			.and_then(|issues| {
				if issues.is_empty() {
					Ok(())
				} else {
					Err(issues.join("; "))
				}
			});
		match read {
			Ok(()) => presets += 1,
			Err(why) => {
				unreadable += 1;
				eprintln!("preset {}: {why}", folder.display());
			}
		}
	}
	println!("{presets} presets load, {unreadable} do not");
	Some(i32::from(
		!registry.errors.is_empty() || registry.templates.is_empty() || unreadable > 0,
	))
}

/// A refresh that died between its two renames left the catalog under its old name.
fn recover(live: &Path) {
	let previous = live.with_extension("old");
	if !live.exists() && previous.exists() {
		let _ = std::fs::rename(&previous, live);
	}
}

fn index_of(dir: &Path) -> Option<Index> {
	serde_json::from_slice(&std::fs::read(dir.join("index.json")).ok()?).ok()
}

fn failed(what: &str, why: impl std::fmt::Display) -> ServerError {
	ServerError::Import(format!("The template catalog {what}: {why}."))
}

async fn fetch(s: &AppState, url: &str, max: usize) -> Result<Vec<u8>, ServerError> {
	let response = s
		.outbound
		.get(url)
		.await
		.map_err(|d| failed("was refused by the outbound policy", d.reason()))?
		.timeout(TIMEOUT)
		.send()
		.await
		.map_err(|e| failed("could not be reached", e.without_url()))?;
	if !response.status().is_success() {
		return Err(failed("answered", response.status()));
	}
	read_capped(response, max)
		.await
		.map(|b| b.to_vec())
		.map_err(|e| failed("could not be read", format!("{e:?}")))
}

/// Unpacks the archive into the empty `into`. Only plain files below `templates/` are
/// taken, and none may leave `into`.
fn unpack(archive: &[u8], into: &Path) -> std::io::Result<()> {
	let bad = |why: String| std::io::Error::new(std::io::ErrorKind::InvalidData, why);
	let mut unpacked = 0u64;
	// The reader is cut off too: `tar` reads a long-name or extension record whole into
	// memory before this loop sees an entry.
	let bounded = std::io::Read::take(GzDecoder::new(archive), MAX_UNPACKED_BYTES);
	for (count, entry) in tar::Archive::new(bounded).entries()?.enumerate() {
		if count >= MAX_ENTRIES {
			return Err(bad(format!("more than {MAX_ENTRIES} files")));
		}
		let mut entry = entry?;
		unpacked = unpacked.saturating_add(entry.size());
		if unpacked > MAX_UNPACKED_BYTES {
			return Err(bad("the archive unpacks to too much".to_owned()));
		}
		let path = entry.path()?.into_owned();
		if entry.header().entry_type() != tar::EntryType::Regular || !path.starts_with("templates")
		{
			return Err(bad(format!("unexpected entry {}", path.display())));
		}
		if !entry.unpack_in(into)? {
			return Err(bad(format!("{} leaves the catalog", path.display())));
		}
	}
	Ok(())
}

/// Unpacks a checked archive beside the catalog, reads it, and only then puts it in the
/// catalog's place. Nothing is replaced by a catalog this podkit cannot read.
fn replace(
	live: &Path,
	archive: &[u8],
	index_bytes: &[u8],
	index: Index,
) -> std::io::Result<registry::Registry> {
	let next = live.with_extension("new");
	let previous = live.with_extension("old");
	for stale in [&next, &previous] {
		if stale.exists() {
			std::fs::remove_dir_all(stale)?;
		}
	}
	let unpacked = (|| {
		std::fs::create_dir_all(&next)?;
		unpack(archive, &next)?;
		std::fs::write(next.join("index.json"), index_bytes)?;
		let registry = index.read(&next);
		if registry.templates.is_empty() {
			let why = registry.errors.first().map_or_else(
				|| "it holds no template".to_owned(),
				|(id, why)| format!("this podkit can read none of its templates ({id}: {why})"),
			);
			return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, why));
		}
		Ok(registry)
	})();
	let mut registry = match unpacked {
		Ok(registry) => registry,
		Err(e) => {
			// A refused catalog leaves nothing behind.
			let _ = std::fs::remove_dir_all(&next);
			return Err(e);
		}
	};
	if live.exists() {
		std::fs::rename(live, &previous)?;
	}
	if let Err(e) = std::fs::rename(&next, live) {
		let _ = std::fs::rename(&previous, live);
		return Err(e);
	}
	live.clone_into(&mut registry.dir);
	// The new catalog is in place: failing to delete the old one must not undo that.
	let _ = std::fs::remove_dir_all(&previous);
	Ok(registry)
}

/// Downloads the catalog from `Config::template_catalog_url` and makes it the gallery.
/// The old one stays in place when anything fails, and when this podkit can read none of
/// the new one's templates.
///
/// # Errors
/// `Import` when the catalog cannot be fetched, is for a newer podkit, does not match its
/// hash or cannot be read; I/O errors from the data directory.
pub async fn refresh(s: &AppState) -> Result<Refreshed, ServerError> {
	// A caller that goes away (a closed tab) must not stop this half-way: disk and memory
	// would disagree, and the next refresh would start under this one's renames.
	let s = s.clone();
	tokio::spawn(async move { refresh_now(&s).await })
		.await
		.map_err(|e| failed("could not be replaced", e))?
}

async fn refresh_now(s: &AppState) -> Result<Refreshed, ServerError> {
	// Two refreshes at once would move the same folders under each other.
	static RUNNING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
	let _one_at_a_time = RUNNING.lock().await;
	let base = s.config.template_catalog_url.trim_end_matches('/');
	let index_bytes = fetch(s, &format!("{base}/index.json"), MAX_INDEX_BYTES).await?;
	let index: Index =
		serde_json::from_slice(&index_bytes).map_err(|e| failed("has an unreadable index", e))?;
	if index.format != FORMAT {
		return Err(failed(
			"needs another podkit",
			format!(
				"it is format {}, this podkit reads format {FORMAT}",
				index.format
			),
		));
	}
	if index.archive.name.contains('/') {
		return Err(failed(
			"has an unusable index",
			"the archive name is a path",
		));
	}
	let archive = fetch(
		s,
		&format!("{base}/{}", index.archive.name),
		MAX_ARCHIVE_BYTES,
	)
	.await?;
	let hash = Sha256::digest(&archive)
		.iter()
		.fold(String::new(), |mut s, b| {
			let _ = write!(s, "{b:02x}");
			s
		});
	if hash != index.archive.sha256 {
		return Err(failed(
			"was not accepted",
			"the archive does not match the hash in the index",
		));
	}

	let live = dir(&s.config.data_dir);
	let registry =
		tokio::task::spawn_blocking(move || replace(&live, &archive, &index_bytes, index))
			.await
			.map_err(|e| failed("could not be unpacked", e))?
			.map_err(|e| failed("was not accepted", e))?;

	let refreshed = Refreshed {
		commit: registry.commit.clone(),
		templates: registry.templates.len(),
		skipped: registry.errors.clone(),
	};
	registry::install(registry);
	Ok(refreshed)
}

#[cfg(test)]
mod tests {
	use std::io::Write as _;

	use super::*;

	/// A catalog archive of `files` (path, content), as the template repository builds it.
	fn archive(files: &[(&str, &[u8])]) -> Vec<u8> {
		let mut tar = tar::Builder::new(Vec::new());
		for (path, content) in files {
			let mut header = tar::Header::new_gnu();
			header.set_size(content.len() as u64);
			header.set_mode(0o644);
			header.set_cksum();
			// `append_data` refuses `..`; a hostile archive is not made with it.
			header.as_mut_bytes()[..path.len()].copy_from_slice(path.as_bytes());
			header.set_cksum();
			tar.append(&header, *content).unwrap();
		}
		let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
		gz.write_all(&tar.into_inner().unwrap()).unwrap();
		gz.finish().unwrap()
	}

	fn fixture(id: &str, file: &str) -> Vec<u8> {
		let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("templates");
		std::fs::read(dir.join(id).join(file)).unwrap()
	}

	fn index(revision: &str) -> (Vec<u8>, Index) {
		let bytes = serde_json::json!({
			"format": FORMAT,
			"commit": "abc1234",
			"archive": { "name": "catalog.tar.gz", "sha256": "" },
			"templates": [{ "id": "gotify", "revision": revision }],
		})
		.to_string()
		.into_bytes();
		let index = serde_json::from_slice(&bytes).unwrap();
		(bytes, index)
	}

	fn gotify() -> Vec<u8> {
		let toml = fixture("gotify", "template.toml");
		let compose = fixture("gotify", "docker-compose.yml");
		let logo = fixture("gotify", "logo.svg");
		archive(&[
			("templates/gotify/template.toml", &toml),
			("templates/gotify/docker-compose.yml", &compose),
			("templates/gotify/logo.svg", &logo),
		])
	}

	#[test]
	fn a_catalog_replaces_the_one_there_and_is_read_back_with_its_revisions() {
		let tmp = tempfile::tempdir().unwrap();
		let live = tmp.path().join("catalog");
		std::fs::create_dir_all(live.join("templates/old")).unwrap();

		let (bytes, parsed) = index("0123456789abcdef");
		let registry = replace(&live, &gotify(), &bytes, parsed).unwrap();
		assert_eq!(registry.commit, "abc1234");
		assert_eq!(registry.templates["gotify"].revision, "0123456789abcdef");
		assert!(!live.join("templates/old").exists());
		assert!(!live.with_extension("old").exists());
		assert!(!live.with_extension("new").exists());
		// It was read in the staging folder: its logo is found where the catalog is now.
		let logo = fixture("gotify", "logo.svg");
		let served = |r: &registry::Registry| {
			let bytes = std::fs::read(r.logo_file("gotify").unwrap()).unwrap();
			assert_eq!(
				registry::short_hash(&bytes),
				r.templates["gotify"].logo.hash
			);
			bytes
		};
		assert_eq!(served(&registry), logo);
		// And again when it replaces a catalog that was itself put there by a refresh.
		let (bytes, parsed) = index("0123456789abcdef");
		assert_eq!(
			served(&replace(&live, &gotify(), &bytes, parsed).unwrap()),
			logo
		);

		// What a restart reads is the same catalog.
		let again = index_of(&live).unwrap().read(&live);
		assert_eq!(again.templates["gotify"].revision, "0123456789abcdef");
		assert!(again.errors.is_empty());
	}

	#[test]
	fn a_catalog_this_podkit_cannot_read_leaves_the_one_there() {
		let tmp = tempfile::tempdir().unwrap();
		let live = tmp.path().join("catalog");
		std::fs::create_dir_all(live.join("templates/kept")).unwrap();

		let unreadable = archive(&[
			(
				"templates/x/template.toml",
				b"id = \"x\"\nfrom_the_future = true\n",
			),
			("templates/x/docker-compose.yml", b"services: {}\n"),
		]);
		let (bytes, parsed) = index("");
		let refused = replace(&live, &unreadable, &bytes, parsed).err().unwrap();
		assert!(refused.to_string().contains("can read none"), "{refused}");
		assert!(live.join("templates/kept").exists());
		assert!(!live.with_extension("new").exists());
	}

	#[test]
	fn an_archive_may_only_hold_plain_files_under_templates() {
		let tmp = tempfile::tempdir().unwrap();
		let live = tmp.path().join("catalog");
		for path in [
			"templates/../../escaped",
			"elsewhere/file",
			"/etc/passwd-like",
		] {
			let (bytes, parsed) = index("");
			let hostile = archive(&[(path, b"x")]);
			assert!(replace(&live, &hostile, &bytes, parsed).is_err(), "{path}");
			assert!(!live.with_extension("new").exists(), "{path} left a folder");
		}
		assert!(!tmp.path().join("escaped").exists());
		assert!(!live.exists());
	}

	#[test]
	fn an_archive_that_claims_more_than_it_may_hold_is_cut_off() {
		let tmp = tempfile::tempdir().unwrap();
		let live = tmp.path().join("catalog");
		// A long-name record is read whole into memory by `tar`: this one says 1 GiB.
		let mut header = tar::Header::new_gnu();
		header.set_entry_type(tar::EntryType::GNULongName);
		header.set_size(1 << 30);
		header.as_mut_bytes()[..13].copy_from_slice(b"././@LongLink");
		header.set_cksum();
		let mut raw = header.as_bytes().to_vec();
		raw.resize(raw.len() + (1 << 30), 0);
		let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
		gz.write_all(&raw).unwrap();
		let (bytes, parsed) = index("");
		assert!(replace(&live, &gz.finish().unwrap(), &bytes, parsed).is_err());
	}

	#[test]
	fn the_catalog_s_presets_come_with_the_index_and_survive_a_restart() {
		let tmp = tempfile::tempdir().unwrap();
		let live = tmp.path().join("catalog");
		let good = "version = 1\n[projects.app]\nname = \"App\"\n\
			[projects.app.environments.production]\nname = \"Production\"\n\
			[projects.app.environments.production.databases.main]\nserver = \"${choose}\"\nengine = \"postgres\"\n";
		let bytes = serde_json::json!({
			"format": FORMAT,
			"commit": "abc1234",
			"archive": { "name": "catalog.tar.gz", "sha256": "" },
			"templates": [],
			"presets": [
				{ "id": "starter", "name": "Starter", "description": "d", "tags": ["x"],
				  "maintainers": [], "secrets": {}, "sha256": "ignored", "file": good },
				{ "id": "from-the-future", "name": "F", "description": "d",
				  "file": "version = 1\n[projects.a]\nname = \"A\"\nnot_yet_invented = true\n" },
				{ "id": "two", "name": "T", "description": "d",
				  "file": "version = 1\n[projects.a]\nname = \"A\"\n[projects.b]\nname = \"B\"\n" },
				{ "id": "../x", "name": "X", "description": "d", "file": good },
				{ "id": "starter", "name": "Again", "description": "d", "file": good },
				{ "id": "big", "name": "Big", "description": "d",
				  "file": format!("{good}# {}\n", "x".repeat(2 << 20)) },
				{ "id": "long-line", "name": "L", "description": "d",
				  "file": format!("version = 1\n[projects.a]\nname = {}\n", "x".repeat(5000)) },
			],
		})
		.to_string()
		.into_bytes();
		let parsed: Index = serde_json::from_slice(&bytes).unwrap();
		let registry = replace(&live, &gotify(), &bytes, parsed).unwrap();
		let ids =
			|r: &registry::Registry| r.presets.iter().map(|p| p.id.clone()).collect::<Vec<_>>();
		assert_eq!(ids(&registry), ["starter"], "{:?}", registry.errors);
		assert_eq!(registry.presets[0].file, good);
		// The six it cannot use are said, beside the templates it cannot read, and briefly.
		assert_eq!(registry.errors.len(), 6, "{:?}", registry.errors);
		let why = |id: &str| {
			let named = format!("preset {id:?}");
			&registry.errors.iter().find(|e| e.0 == named).unwrap().1
		};
		assert_eq!(why("starter"), "another preset has this id");
		assert_eq!(why("big"), "its file is larger than a kept preset may be");
		assert!(
			why("long-line").chars().count() <= 301,
			"{}",
			why("long-line")
		);

		let again = index_of(&live).unwrap().read(&live);
		assert_eq!(ids(&again), ["starter"]);
	}

	#[test]
	fn a_revision_too_long_to_record_counts_as_none() {
		let tmp = tempfile::tempdir().unwrap();
		let live = tmp.path().join("catalog");
		let (bytes, parsed) = index(&"f".repeat(64));
		let registry = replace(&live, &gotify(), &bytes, parsed).unwrap();
		assert_eq!(registry.templates["gotify"].revision, "");
	}

	#[test]
	fn a_refresh_that_died_between_its_renames_is_put_back_at_start() {
		let tmp = tempfile::tempdir().unwrap();
		let live = tmp.path().join("catalog");
		std::fs::create_dir_all(live.with_extension("old").join("templates")).unwrap();
		recover(&live);
		assert!(live.join("templates").exists());

		// A catalog that is there is never overwritten by a leftover.
		std::fs::create_dir_all(live.with_extension("old").join("stale")).unwrap();
		recover(&live);
		assert!(!live.join("stale").exists());
	}
}
