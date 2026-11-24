//! The command line's grammar: which command, with what. Parsing only; nothing here
//! touches the network or the disk.

/// Which team and project a command works in, from `--team` and `--project`. Unset ones
/// come from the environment or the config file, or are worked out.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Place {
	/// `--team`: a slug or an id.
	pub team: Option<String>,
	/// `--project`: a slug or an id.
	pub project: Option<String>,
}

/// How long `--wait` follows a deploy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Wait {
	/// Follow the build log until the deploy ends.
	pub on: bool,
	/// Once running, keep watching this many seconds for the health gate to roll it back.
	pub settle: u64,
	/// Give up after this many seconds.
	pub timeout: u64,
}

/// Default `--timeout` of `--wait`: 30 minutes.
pub const DEFAULT_TIMEOUT: u64 = 30 * 60;

/// A parsed command line. Each variant is the command of its name, its fields the options
/// of the same name; [`help`] says what they do.
#[allow(
	missing_docs,
	reason = "the variants and fields are the documented options"
)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
	/// The help of one command, or the overview.
	Help(Option<String>),
	Version,
	Mcp,
	/// Handed to `import::parse` as it is.
	Import(Vec<String>),
	Login {
		url: Option<String>,
		/// From `--token`; otherwise read from stdin.
		token: Option<String>,
		place: Place,
	},
	Logout,
	Whoami {
		json: bool,
	},
	Apps {
		place: Place,
		json: bool,
	},
	Deployments {
		app: String,
		place: Place,
		limit: u32,
		json: bool,
	},
	Deploy {
		app: String,
		place: Place,
		wait: Wait,
		json: bool,
	},
	Rollback {
		app: String,
		place: Place,
		/// The deployment to go back to; the previous good one when unset.
		to: Option<i64>,
		wait: Wait,
		json: bool,
	},
	Logs {
		app: String,
		place: Place,
		follow: bool,
		tail: u32,
	},
	EnvPull {
		app: String,
		place: Place,
		/// `-` is stdout.
		file: String,
		force: bool,
	},
	EnvPush {
		app: String,
		place: Place,
		file: String,
		yes: bool,
		prune: bool,
		allow_empty: bool,
		dry_run: bool,
	},
}

/// The overview printed by `podkit --help`.
pub const USAGE: &str = "podkit: the command line

Usage: podkit <command> [options]

  login                  save podkit's address and an API token
  logout                 forget them
  whoami                 who the token acts as, and what it may do
  apps                   list applications
  deploy <app>           deploy an application (--wait follows the build)
  deployments <app>      list an application's deployments
  rollback <app>         put the previous good deployment back
  logs <app>             print an application's logs (-f follows them)
  env pull <app> [file]  write the application's variables to a .env file
  env push <app> <file>  set the application's variables from a .env file
  import <platform>      bring projects over from Coolify, Dokploy or CapRover
  mcp                    serve podkit's MCP tools over stdio

Run podkit <command> --help for its options.

<app> is an application's slug or id. Options shared by the commands that take one:
  --team <slug|id>       the team (default: PODKIT_TEAM, the saved one, or your only team)
  --project <slug|id>    the project (default: PODKIT_PROJECT, the saved one, or the one
                         that has an application of that name)

Environment (wins over the config file):
  PODKIT_URL      podkit's address (default http://127.0.0.1:8080)
  PODKIT_TOKEN    an API token (pk_...), made in Settings -> API tokens
  PODKIT_TEAM, PODKIT_PROJECT
  PODKIT_CONFIG   the config file (default ~/.config/podkit/config.json)

Exit status: 0 done, 1 failed, 2 usage or configuration mistake, 3 the deploy failed,
was cancelled or was rolled back.
";

/// The help of one command.
#[must_use]
pub fn help(command: &str) -> Option<&'static str> {
	Some(match command {
		"login" => {
			"Usage: podkit login [--url <address>] [--token <pk_...>] [--team <t>] [--project <p>]

Checks the token against podkit, then saves the address, the token and the default team
and project in the config file (mode 0600). Without --token the token is read from
stdin, so it stays out of your shell history:

  printf %s \"$TOKEN\" | podkit login --url https://podkit.example.com

PODKIT_URL and PODKIT_TOKEN, when set, win over the saved values.
"
		}
		"logout" => {
			"Usage: podkit logout\n\nDeletes the config file. The token itself stays valid: revoke it in Settings -> API tokens.\n"
		}
		"whoami" => {
			"Usage: podkit whoami [--json]\n\nShows podkit's address, the account the token acts as, its scopes and limits.\n"
		}
		"apps" => {
			"Usage: podkit apps [--team <t>] [--project <p>] [--json]

Lists the applications of a project, or of every project you can see.
"
		}
		"deployments" => {
			"Usage: podkit deployments <app> [--limit <n>] [--json]\n\nLists the newest deployments (default 10), with the health gate's verdict.\n"
		}
		"deploy" => {
			"Usage: podkit deploy <app> [--wait] [--settle <time>] [--timeout <time>] [--json]

Queues a deploy of the application's configured source (its branch or image).

  --wait            follow the build log until the deploy ends; exit 3 if it failed,
                    was cancelled or the health gate kept the old container
  --settle <time>   with --wait: once running, keep watching this long and exit 3 if
                    the health gate rolls the release back (e.g. 90s, 5m)
  --timeout <time>  with --wait: give up after this long (default 30m), exit 1
  --json            print the final deployment as JSON (build output goes to stderr)

Times are seconds, or a number with s, m or h.
"
		}
		"rollback" => {
			"Usage: podkit rollback <app> [--to <deployment id>] [--wait] [--settle <time>] [--json]

Redeploys an earlier deployment's image (no rebuild). Without --to it picks the newest
deployment before the running one that built an image and was not found unhealthy.
--wait, --settle and --timeout work as for deploy.
"
		}
		"logs" => {
			"Usage: podkit logs <app> [-f|--follow] [-n|--tail <lines>]

Prints the running container's output: the last lines (default 200, at most 5000), and
with --follow everything after until the container stops or you press Ctrl-C.
"
		}
		"env" => {
			"Usage: podkit env pull <app> [file] [--force]
       podkit env push <app> <file> [--yes] [--prune] [--allow-empty] [--dry-run]

pull writes the application's variables to file (default .env, - for stdout), mode 0600.
Values are write-only in podkit: no token can read them back, so the file lists the
names with empty values. It refuses to overwrite a file without --force.

push sets the variables in file with one request, after printing what changes (names
only) and asking. Variables not in the file are kept, unless --prune deletes them.
  --yes          do not ask (needed when stdin is not a terminal)
  --prune        delete the application's variables that are not in the file
  --allow-empty  set a variable to an empty value; otherwise an empty line for a
                 variable the application has is skipped, so a pulled file pushed back
                 blanks nothing
  --dry-run      print what would change and stop
"
		}
		_ => return None,
	})
}

/// Options and positional arguments of one command.
#[derive(Debug, Default)]
struct Opts {
	positional: Vec<String>,
	switches: Vec<&'static str>,
	values: Vec<(&'static str, String)>,
}

impl Opts {
	fn has(&self, name: &str) -> bool {
		self.switches.contains(&name)
	}

	fn value(&self, name: &str) -> Option<String> {
		self.values
			.iter()
			.rev()
			.find(|(k, _)| *k == name)
			.map(|(_, v)| v.clone())
	}

	fn place(&self) -> Place {
		Place {
			team: self.value("--team"),
			project: self.value("--project"),
		}
	}
}

fn long(arg: &str) -> &str {
	match arg {
		"-f" => "--follow",
		"-n" => "--tail",
		"-y" => "--yes",
		"-t" => "--team",
		"-p" => "--project",
		other => other,
	}
}

/// Splits `args` into positional ones, the `switches` given and the `valued` options with
/// their values (`--name value` or `--name=value`).
fn split(
	args: Vec<String>,
	switches: &[&'static str],
	valued: &[&'static str],
) -> Result<Opts, String> {
	let mut out = Opts::default();
	let mut it = args.into_iter();
	while let Some(arg) = it.next() {
		if arg == "--" {
			out.positional.extend(it.by_ref());
			break;
		}
		if !arg.starts_with('-') || arg == "-" {
			out.positional.push(arg);
			continue;
		}
		let (name, inline) = match arg.split_once('=') {
			Some((n, v)) if n.starts_with("--") => (n.to_owned(), Some(v.to_owned())),
			_ => (arg.clone(), None),
		};
		let name = long(&name);
		if let Some(s) = switches.iter().find(|s| **s == name) {
			if inline.is_some() {
				return Err(format!("{name} takes no value"));
			}
			out.switches.push(s);
		} else if let Some(v) = valued.iter().find(|v| **v == name) {
			let value = match inline {
				Some(v) => v,
				None => it.next().ok_or(format!("{name} needs a value"))?,
			};
			out.values.push((v, value));
		} else {
			return Err(format!("unknown option {arg}"));
		}
	}
	Ok(out)
}

/// Seconds from `90`, `90s`, `5m` or `1h`.
///
/// # Errors
/// When it is none of those.
pub fn seconds(raw: &str) -> Result<u64, String> {
	let raw = raw.trim();
	let (digits, unit) = match raw.char_indices().find(|(_, c)| !c.is_ascii_digit()) {
		Some((at, _)) => raw.split_at(at),
		None => (raw, ""),
	};
	let n: u64 = digits
		.parse()
		.map_err(|_| format!("{raw:?} is not a time; use seconds, or e.g. 90s, 5m, 1h"))?;
	let factor = match unit {
		"" | "s" => 1,
		"m" => 60,
		"h" => 3600,
		_ => {
			return Err(format!(
				"{raw:?} is not a time; use seconds, or e.g. 90s, 5m, 1h"
			));
		}
	};
	Ok(n.saturating_mul(factor))
}

fn number<T: std::str::FromStr>(name: &str, raw: Option<String>, default: T) -> Result<T, String> {
	match raw {
		None => Ok(default),
		Some(r) => r
			.trim()
			.parse()
			.map_err(|_| format!("{name} wants a number, not {r:?}")),
	}
}

const PLACE: [&str; 2] = ["--team", "--project"];

fn with_place(extra: &[&'static str]) -> Vec<&'static str> {
	PLACE.iter().chain(extra).copied().collect()
}

/// The one application a command acts on.
fn app(o: &Opts, usage: &str) -> Result<String, String> {
	match o.positional.as_slice() {
		[app] if !app.trim().is_empty() => Ok(app.clone()),
		[] => Err(format!("name the application\n\n{usage}")),
		_ => Err(format!("too many arguments\n\n{usage}")),
	}
}

fn wait(o: &Opts) -> Result<Wait, String> {
	let wait = Wait {
		on: o.has("--wait"),
		settle: o
			.value("--settle")
			.as_deref()
			.map(seconds)
			.transpose()?
			.unwrap_or(0),
		timeout: o
			.value("--timeout")
			.as_deref()
			.map(seconds)
			.transpose()?
			.unwrap_or(DEFAULT_TIMEOUT),
	};
	if !wait.on && (o.value("--settle").is_some() || o.value("--timeout").is_some()) {
		return Err("--settle and --timeout only mean something with --wait".to_owned());
	}
	if wait.timeout == 0 {
		return Err("--timeout must be more than 0".to_owned());
	}
	Ok(wait)
}

/// Parses everything after `podkit`.
///
/// # Errors
/// A message naming what is wrong; the caller prints it and exits with status 2.
#[allow(clippy::too_many_lines, reason = "one table of commands")]
pub fn parse(args: Vec<String>) -> Result<Command, String> {
	let mut it = args.into_iter();
	let Some(command) = it.next() else {
		return Ok(Command::Help(None));
	};
	let rest: Vec<String> = it.collect();
	if command != "import" && rest.iter().any(|a| a == "--help" || a == "-h") {
		return Ok(Command::Help(Some(command)));
	}
	let usage = |c: &str| help(c).unwrap_or(USAGE);
	match command.as_str() {
		"--help" | "-h" | "help" => Ok(Command::Help(rest.into_iter().next())),
		"--version" | "-V" | "version" => Ok(Command::Version),
		"mcp" => {
			if rest.is_empty() {
				Ok(Command::Mcp)
			} else {
				Err("podkit mcp takes no arguments".to_owned())
			}
		}
		"import" => Ok(Command::Import(rest)),
		"login" => {
			let o = split(rest, &[], &with_place(&["--url", "--token"]))?;
			if !o.positional.is_empty() {
				return Err(format!(
					"unexpected {}\n\n{}",
					o.positional[0],
					usage("login")
				));
			}
			Ok(Command::Login {
				url: o.value("--url"),
				token: o.value("--token"),
				place: o.place(),
			})
		}
		"logout" => {
			let o = split(rest, &[], &[])?;
			if o.positional.is_empty() {
				Ok(Command::Logout)
			} else {
				Err("podkit logout takes no arguments".to_owned())
			}
		}
		"whoami" => {
			let o = split(rest, &["--json"], &[])?;
			if !o.positional.is_empty() {
				return Err("podkit whoami takes no arguments".to_owned());
			}
			Ok(Command::Whoami {
				json: o.has("--json"),
			})
		}
		"apps" => {
			let o = split(rest, &["--json"], &PLACE)?;
			if !o.positional.is_empty() {
				return Err(format!(
					"unexpected {}\n\n{}",
					o.positional[0],
					usage("apps")
				));
			}
			Ok(Command::Apps {
				place: o.place(),
				json: o.has("--json"),
			})
		}
		"deployments" => {
			let o = split(rest, &["--json"], &with_place(&["--limit"]))?;
			let limit = number("--limit", o.value("--limit"), 10_u32)?;
			if !(1..=200).contains(&limit) {
				return Err("--limit must be between 1 and 200".to_owned());
			}
			Ok(Command::Deployments {
				app: app(&o, usage("deployments"))?,
				place: o.place(),
				limit,
				json: o.has("--json"),
			})
		}
		"deploy" => {
			let o = split(
				rest,
				&["--wait", "--json"],
				&with_place(&["--settle", "--timeout"]),
			)?;
			Ok(Command::Deploy {
				app: app(&o, usage("deploy"))?,
				place: o.place(),
				wait: wait(&o)?,
				json: o.has("--json"),
			})
		}
		"rollback" => {
			let o = split(
				rest,
				&["--wait", "--json"],
				&with_place(&["--to", "--settle", "--timeout"]),
			)?;
			let to = o
				.value("--to")
				.map(|v| {
					v.trim()
						.parse::<i64>()
						.map_err(|_| format!("--to wants a deployment id, not {v:?}"))
				})
				.transpose()?;
			Ok(Command::Rollback {
				app: app(&o, usage("rollback"))?,
				place: o.place(),
				to,
				wait: wait(&o)?,
				json: o.has("--json"),
			})
		}
		"logs" => {
			let o = split(rest, &["--follow"], &with_place(&["--tail"]))?;
			let tail = number("--tail", o.value("--tail"), 200_u32)?;
			if tail > 5000 {
				return Err("--tail is at most 5000".to_owned());
			}
			Ok(Command::Logs {
				app: app(&o, usage("logs"))?,
				place: o.place(),
				follow: o.has("--follow"),
				tail,
			})
		}
		"env" => env(rest),
		other => Err(format!("unknown command {other}\n\n{USAGE}")),
	}
}

fn env(rest: Vec<String>) -> Result<Command, String> {
	let usage = help("env").unwrap_or(USAGE);
	let mut it = rest.into_iter();
	let action = it.next();
	let rest: Vec<String> = it.collect();
	match action.as_deref() {
		Some("pull") => {
			let o = split(rest, &["--force"], &PLACE)?;
			let (app, file) = match o.positional.as_slice() {
				[app] => (app.clone(), ".env".to_owned()),
				[app, file] => (app.clone(), file.clone()),
				[] => return Err(format!("name the application\n\n{usage}")),
				_ => return Err(format!("too many arguments\n\n{usage}")),
			};
			Ok(Command::EnvPull {
				app,
				place: o.place(),
				file,
				force: o.has("--force"),
			})
		}
		Some("push") => {
			let o = split(
				rest,
				&["--yes", "--prune", "--allow-empty", "--dry-run"],
				&PLACE,
			)?;
			let [app, file] = o.positional.as_slice() else {
				return Err(format!("name the application and the file\n\n{usage}"));
			};
			Ok(Command::EnvPush {
				app: app.clone(),
				place: o.place(),
				file: file.clone(),
				yes: o.has("--yes"),
				prune: o.has("--prune"),
				allow_empty: o.has("--allow-empty"),
				dry_run: o.has("--dry-run"),
			})
		}
		Some(other) => Err(format!("unknown env command {other}\n\n{usage}")),
		None => Err(format!("say pull or push\n\n{usage}")),
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn p(line: &str) -> Result<Command, String> {
		parse(line.split_whitespace().map(str::to_owned).collect())
	}

	fn place(team: Option<&str>, project: Option<&str>) -> Place {
		Place {
			team: team.map(str::to_owned),
			project: project.map(str::to_owned),
		}
	}

	#[test]
	fn no_command_and_help_flags_give_help() {
		assert_eq!(p(""), Ok(Command::Help(None)));
		assert_eq!(p("--help"), Ok(Command::Help(None)));
		assert_eq!(p("help env"), Ok(Command::Help(Some("env".into()))));
		assert_eq!(
			p("deploy web --help"),
			Ok(Command::Help(Some("deploy".into())))
		);
		assert_eq!(p("-V"), Ok(Command::Version));
		// import has its own help, handled by its parser.
		assert_eq!(
			p("import coolify --help"),
			Ok(Command::Import(vec!["coolify".into(), "--help".into()]))
		);
		for c in [
			"login",
			"logout",
			"whoami",
			"apps",
			"deploy",
			"deployments",
			"rollback",
			"logs",
			"env",
		] {
			assert!(help(c).is_some(), "{c}");
		}
	}

	#[test]
	fn deploy_takes_the_app_place_and_wait_options() {
		assert_eq!(
			p("deploy web"),
			Ok(Command::Deploy {
				app: "web".into(),
				place: Place::default(),
				wait: Wait {
					on: false,
					settle: 0,
					timeout: DEFAULT_TIMEOUT
				},
				json: false,
			})
		);
		assert_eq!(
			p("deploy --team=chikof -p chiko-web web --wait --settle 5m --timeout 600 --json"),
			Ok(Command::Deploy {
				app: "web".into(),
				place: place(Some("chikof"), Some("chiko-web")),
				wait: Wait {
					on: true,
					settle: 300,
					timeout: 600
				},
				json: true,
			})
		);
		assert!(p("deploy").unwrap_err().contains("name the application"));
		assert!(p("deploy a b").unwrap_err().contains("too many"));
		assert!(p("deploy web --settle 5m").unwrap_err().contains("--wait"));
		assert!(p("deploy web --wait --timeout 0").is_err());
		assert!(
			p("deploy web --ref main")
				.unwrap_err()
				.contains("unknown option --ref")
		);
		assert!(
			p("deploy web --wait=yes")
				.unwrap_err()
				.contains("takes no value")
		);
		assert!(
			p("deploy web --team")
				.unwrap_err()
				.contains("needs a value")
		);
	}

	#[test]
	fn rollback_logs_and_deployments() {
		assert_eq!(
			p("rollback api --to 42 --wait"),
			Ok(Command::Rollback {
				app: "api".into(),
				place: Place::default(),
				to: Some(42),
				wait: Wait {
					on: true,
					settle: 0,
					timeout: DEFAULT_TIMEOUT
				},
				json: false,
			})
		);
		assert!(p("rollback api --to x").is_err());
		assert_eq!(
			p("logs api -f -n 50"),
			Ok(Command::Logs {
				app: "api".into(),
				place: Place::default(),
				follow: true,
				tail: 50,
			})
		);
		assert!(matches!(
			p("logs api"),
			Ok(Command::Logs {
				follow: false,
				tail: 200,
				..
			})
		));
		assert!(p("logs api --tail 9999").is_err());
		assert!(p("logs api --tail many").is_err());
		assert!(matches!(
			p("deployments api --limit 3 --json"),
			Ok(Command::Deployments {
				limit: 3,
				json: true,
				..
			})
		));
		assert!(p("deployments api --limit 0").is_err());
	}

	#[test]
	fn env_pull_and_push() {
		assert_eq!(
			p("env pull web"),
			Ok(Command::EnvPull {
				app: "web".into(),
				place: Place::default(),
				file: ".env".into(),
				force: false,
			})
		);
		assert_eq!(
			p("env pull web - --force"),
			Ok(Command::EnvPull {
				app: "web".into(),
				place: Place::default(),
				file: "-".into(),
				force: true,
			})
		);
		assert_eq!(
			p("env push web .env.prod -y --prune --dry-run --project 3"),
			Ok(Command::EnvPush {
				app: "web".into(),
				place: place(None, Some("3")),
				file: ".env.prod".into(),
				yes: true,
				prune: true,
				allow_empty: false,
				dry_run: true,
			})
		);
		assert!(p("env push web").is_err());
		assert!(p("env").unwrap_err().contains("pull or push"));
		assert!(
			p("env sync web")
				.unwrap_err()
				.contains("unknown env command")
		);
	}

	#[test]
	fn login_whoami_apps_and_unknowns() {
		assert_eq!(
			p("login --url https://pk.example.com --team chikof"),
			Ok(Command::Login {
				url: Some("https://pk.example.com".into()),
				token: None,
				place: place(Some("chikof"), None),
			})
		);
		assert!(p("login extra").is_err());
		assert_eq!(p("whoami --json"), Ok(Command::Whoami { json: true }));
		assert_eq!(
			p("apps --project chiko-web"),
			Ok(Command::Apps {
				place: place(None, Some("chiko-web")),
				json: false,
			})
		);
		assert!(p("frobnicate").unwrap_err().contains("unknown command"));
		assert!(p("mcp now").is_err());
		// `--` ends the options: a slug that starts with a dash is still an app.
		assert!(matches!(p("deploy -- -odd"), Ok(Command::Deploy { app, .. }) if app == "-odd"));
	}

	#[test]
	fn times_take_seconds_minutes_and_hours() {
		assert_eq!(seconds("90"), Ok(90));
		assert_eq!(seconds("90s"), Ok(90));
		assert_eq!(seconds("5m"), Ok(300));
		assert_eq!(seconds("2h"), Ok(7200));
		assert!(seconds("5d").is_err());
		assert!(seconds("m").is_err());
		assert!(seconds("-5").is_err());
	}
}
