//! Where the command line finds podkit and its token: flags, then the environment, then the
//! config file that `podkit login` writes.
//!
//! The file is JSON at `$PODKIT_CONFIG`, or `podkit/config.json` under the user's config
//! directory (`$XDG_CONFIG_HOME`, else `~/.config`; `%APPDATA%` on Windows). It holds a
//! token, so it is written with mode 0600 inside a 0700 directory, and a file that others
//! can read is reported.

use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

/// podkit's address when nothing says otherwise.
pub const DEFAULT_URL: &str = "http://127.0.0.1:8080";

/// What the config file holds. Every field is optional.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Config {
	/// podkit's base address, `https://podkit.example.com`.
	pub url: Option<String>,
	/// The API token, `pk_…`.
	pub token: Option<String>,
	/// The default team, by slug or id.
	pub team: Option<String>,
	/// The default project, by slug or id.
	pub project: Option<String>,
}

impl Config {
	/// Reads the file's JSON. Unknown keys are ignored, so an older command line can read
	/// a newer file.
	///
	/// # Errors
	/// When the text is not a JSON object.
	pub fn from_json(text: &str) -> Result<Self, String> {
		let value: Value =
			serde_json::from_str(text).map_err(|e| format!("not valid JSON: {e}"))?;
		let Value::Object(map) = value else {
			return Err("not a JSON object".to_owned());
		};
		let field = |k: &str| {
			map.get(k)
				.and_then(Value::as_str)
				.map(str::trim)
				.filter(|s| !s.is_empty())
				.map(str::to_owned)
		};
		Ok(Self {
			url: field("url"),
			token: field("token"),
			team: field("team"),
			project: field("project"),
		})
	}

	/// The file's JSON, pretty-printed, with only the fields that are set.
	#[must_use]
	pub fn to_json(&self) -> String {
		let mut map = Map::new();
		for (k, v) in [
			("url", &self.url),
			("token", &self.token),
			("team", &self.team),
			("project", &self.project),
		] {
			if let Some(v) = v {
				map.insert(k.to_owned(), Value::String(v.clone()));
			}
		}
		let mut text = serde_json::to_string_pretty(&Value::Object(map)).unwrap_or_default();
		text.push('\n');
		text
	}
}

/// Where the config file lives, from the environment `env`; `None` when no home or config
/// directory is known.
pub fn path(env: impl Fn(&str) -> Option<String>) -> Option<PathBuf> {
	let set = |k: &str| env(k).filter(|v| !v.is_empty());
	if let Some(p) = set("PODKIT_CONFIG") {
		return Some(PathBuf::from(p));
	}
	let dir = if cfg!(windows) {
		set("APPDATA").map(PathBuf::from)
	} else {
		set("XDG_CONFIG_HOME")
			.map(PathBuf::from)
			.or_else(|| set("HOME").map(|h| Path::new(&h).join(".config")))
	}?;
	Some(dir.join("podkit").join("config.json"))
}

/// Reads the config file; a file that does not exist is an empty config.
///
/// # Errors
/// When the file exists but cannot be read or is not a config.
pub fn load(path: &Path) -> Result<Config, String> {
	match fs::read_to_string(path) {
		Ok(text) => {
			warn_if_open(path);
			Config::from_json(&text).map_err(|e| format!("{}: {e}", path.display()))
		}
		Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Config::default()),
		Err(e) => Err(format!("could not read {}: {e}", path.display())),
	}
}

/// Writes the config file readable by its owner only, replacing it in one step.
///
/// # Errors
/// When the directory or the file cannot be written.
pub fn save(path: &Path, config: &Config) -> Result<(), String> {
	let fail = |e: io::Error| format!("could not write {}: {e}", path.display());
	if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
		create_private_dir(dir).map_err(fail)?;
	}
	let tmp = path.with_extension("json.tmp");
	write_private(&tmp, config.to_json().as_bytes()).map_err(fail)?;
	fs::rename(&tmp, path).map_err(fail)
}

/// Deletes the config file; one that is not there is fine.
///
/// # Errors
/// When it exists and cannot be deleted.
pub fn remove(path: &Path) -> Result<bool, String> {
	match fs::remove_file(path) {
		Ok(()) => Ok(true),
		Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
		Err(e) => Err(format!("could not delete {}: {e}", path.display())),
	}
}

/// Writes `bytes` to a new file only its owner can read (0600 on Unix), replacing any
/// file there. Also used for a pulled `.env`.
///
/// # Errors
/// When the file cannot be written.
pub fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
	let mut options = fs::OpenOptions::new();
	options.write(true).create(true).truncate(true);
	#[cfg(unix)]
	{
		use std::os::unix::fs::OpenOptionsExt as _;
		options.mode(0o600);
	}
	let mut file = options.open(path)?;
	// `mode` only applies to a file it creates: an existing one keeps its bits otherwise.
	#[cfg(unix)]
	{
		use std::os::unix::fs::PermissionsExt as _;
		file.set_permissions(fs::Permissions::from_mode(0o600))?;
	}
	file.write_all(bytes)?;
	file.sync_all()
}

fn create_private_dir(dir: &Path) -> io::Result<()> {
	let mut builder = fs::DirBuilder::new();
	builder.recursive(true);
	#[cfg(unix)]
	{
		use std::os::unix::fs::DirBuilderExt as _;
		builder.mode(0o700);
	}
	builder.create(dir)
}

#[cfg(unix)]
fn warn_if_open(path: &Path) {
	use std::os::unix::fs::PermissionsExt as _;
	if let Ok(meta) = fs::metadata(path)
		&& meta.permissions().mode() & 0o077 != 0
	{
		eprintln!(
			"podkit: warning: {} holds a token and others can read it; run chmod 600 on it",
			path.display()
		);
	}
}

#[cfg(not(unix))]
fn warn_if_open(_: &Path) {}

/// Where a setting came from, so `whoami` can say.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
	/// A command line flag.
	Flag,
	/// An environment variable.
	Env,
	/// The config file.
	File,
	/// Nothing set it.
	Default,
}

impl Source {
	/// How `whoami` names it.
	#[must_use]
	pub fn label(self) -> &'static str {
		match self {
			Self::Flag => "flag",
			Self::Env => "environment",
			Self::File => "config file",
			Self::Default => "default",
		}
	}
}

/// The settings a command runs with, after flags, environment and file are merged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
	/// podkit's address, without a trailing slash.
	pub url: String,
	/// Where the address came from.
	pub url_from: Source,
	/// The token, if anything set one.
	pub token: Option<String>,
	/// Where the token came from.
	pub token_from: Source,
	/// The team to work in, by slug or id.
	pub team: Option<String>,
	/// The project to work in, by slug or id.
	pub project: Option<String>,
}

/// Merges what the flags say (`team`, `project`), the environment (`PODKIT_URL`,
/// `PODKIT_TOKEN`, `PODKIT_TEAM`, `PODKIT_PROJECT`) and the file, in that order.
pub fn resolve(
	file: &Config,
	env: impl Fn(&str) -> Option<String>,
	team: Option<&str>,
	project: Option<&str>,
) -> Settings {
	let env = |k: &str| {
		env(k)
			.map(|v| v.trim().to_owned())
			.filter(|v| !v.is_empty())
	};
	let pick = |flag: Option<&str>, var: &str, file: &Option<String>| {
		if let Some(v) = flag.map(str::trim).filter(|v| !v.is_empty()) {
			(Some(v.to_owned()), Source::Flag)
		} else if let Some(v) = env(var) {
			(Some(v), Source::Env)
		} else if let Some(v) = file {
			(Some(v.clone()), Source::File)
		} else {
			(None, Source::Default)
		}
	};
	let (url, url_from) = pick(None, "PODKIT_URL", &file.url);
	let (token, token_from) = pick(None, "PODKIT_TOKEN", &file.token);
	Settings {
		url: url.map_or_else(
			|| DEFAULT_URL.to_owned(),
			|u| u.trim_end_matches('/').to_owned(),
		),
		url_from,
		token,
		token_from,
		team: pick(team, "PODKIT_TEAM", &file.team).0,
		project: pick(project, "PODKIT_PROJECT", &file.project).0,
	}
}

#[cfg(test)]
mod tests {
	use std::collections::HashMap;

	use super::*;

	fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
		let map: HashMap<String, String> = pairs
			.iter()
			.map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
			.collect();
		move |k| map.get(k).cloned()
	}

	#[test]
	fn the_path_follows_podkit_config_then_xdg_then_home() {
		assert_eq!(
			path(env(&[("PODKIT_CONFIG", "/x/c.json"), ("HOME", "/h")])),
			Some(PathBuf::from("/x/c.json"))
		);
		if !cfg!(windows) {
			assert_eq!(
				path(env(&[("XDG_CONFIG_HOME", "/cfg"), ("HOME", "/h")])),
				Some(PathBuf::from("/cfg/podkit/config.json"))
			);
			assert_eq!(
				path(env(&[("HOME", "/h"), ("XDG_CONFIG_HOME", "")])),
				Some(PathBuf::from("/h/.config/podkit/config.json"))
			);
			assert_eq!(path(env(&[])), None);
		}
	}

	#[test]
	fn json_round_trips_and_ignores_blanks_and_unknown_keys() {
		let c = Config {
			url: Some("https://pk.example.com".into()),
			token: Some("pk_ab_cd".into()),
			team: Some("chikof".into()),
			project: None,
		};
		assert_eq!(Config::from_json(&c.to_json()).unwrap(), c);
		assert!(!c.to_json().contains("project"));
		let read = Config::from_json(r#"{"url":" ","team":"7","later":1}"#).unwrap();
		assert_eq!(read.url, None);
		assert_eq!(read.team.as_deref(), Some("7"));
		assert!(Config::from_json("[]").is_err());
		assert!(Config::from_json("nope").is_err());
	}

	#[test]
	fn flags_beat_the_environment_which_beats_the_file() {
		let file = Config {
			url: Some("https://file.example.com/".into()),
			token: Some("pk_file".into()),
			team: Some("file-team".into()),
			project: Some("file-project".into()),
		};
		let s = resolve(&file, env(&[]), None, None);
		assert_eq!(s.url, "https://file.example.com");
		assert_eq!((s.url_from, s.token_from), (Source::File, Source::File));
		assert_eq!(s.team.as_deref(), Some("file-team"));

		let e = env(&[
			("PODKIT_URL", "http://env:8080"),
			("PODKIT_TOKEN", "pk_env"),
			("PODKIT_TEAM", "env-team"),
			("PODKIT_PROJECT", ""),
		]);
		let s = resolve(&file, e, Some("flag-team"), None);
		assert_eq!(s.url, "http://env:8080");
		assert_eq!(s.token.as_deref(), Some("pk_env"));
		assert_eq!(s.token_from, Source::Env);
		assert_eq!(s.team.as_deref(), Some("flag-team"));
		// An empty variable does not hide the file.
		assert_eq!(s.project.as_deref(), Some("file-project"));

		let s = resolve(&Config::default(), env(&[]), None, None);
		assert_eq!((s.url.as_str(), s.url_from), (DEFAULT_URL, Source::Default));
		assert_eq!(s.token, None);
	}

	#[test]
	fn save_writes_a_private_file_and_remove_forgets_it() {
		let dir = std::env::temp_dir().join(format!("podkit-cli-test-{}", std::process::id()));
		let p = dir.join("nested").join("config.json");
		let c = Config {
			token: Some("pk_x_y".into()),
			..Config::default()
		};
		save(&p, &c).unwrap();
		assert_eq!(load(&p).unwrap(), c);
		#[cfg(unix)]
		{
			use std::os::unix::fs::PermissionsExt as _;
			let mode = fs::metadata(&p).unwrap().permissions().mode();
			assert_eq!(mode & 0o777, 0o600);
			let dmode = fs::metadata(p.parent().unwrap())
				.unwrap()
				.permissions()
				.mode();
			assert_eq!(dmode & 0o777, 0o700);
			// An existing, too open file is tightened when written again.
			fs::set_permissions(&p, fs::Permissions::from_mode(0o644)).unwrap();
			save(&p, &c).unwrap();
			let mode = fs::metadata(&p).unwrap().permissions().mode();
			assert_eq!(mode & 0o777, 0o600);
		}
		assert!(remove(&p).unwrap());
		assert!(!remove(&p).unwrap());
		assert_eq!(load(&p).unwrap(), Config::default());
		fs::remove_dir_all(&dir).unwrap();
	}
}
