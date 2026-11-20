//! Generates the web UI stylesheet into `$OUT_DIR/app.css`, in three parts:
//!
//! 1. `src/ui/theme.css`: the design tokens and fonts;
//! 2. `src/ui/components.css`: hand-written rules for what utilities cannot express
//!    (top layer, anchor positioning, `@starting-style`, view transitions, ...);
//! 3. the utilities: encre-css scans the templates under `src/ui` for
//!    Tailwind-compatible classes and emits only the CSS they use.
//!
//! A class in a template must be a utility encre-css knows or a `.class` defined in
//! `components.css`; anything else fails the build instead of silently styling nothing.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::{env, fs};

const UI_DIR: &str = "src/ui";

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
	for entry in fs::read_dir(dir).expect("read src/ui") {
		let path = entry.expect("read src/ui entry").path();
		if path.is_dir() {
			rust_files(&path, out);
		} else if path.extension().is_some_and(|e| e == "rs") {
			out.push(path);
		}
	}
}

fn main() {
	embed_templates();
	// A directory makes Cargo rerun this script when any file below it changes.
	println!("cargo::rerun-if-changed={UI_DIR}");

	let mut files = Vec::new();
	rust_files(Path::new(UI_DIR), &mut files);
	files.sort();
	let sources: Vec<String> = files
		.iter()
		.map(|f| fs::read_to_string(f).expect("read template source"))
		.collect();

	let theme = fs::read_to_string(Path::new(UI_DIR).join("theme.css")).expect("read theme.css");
	let components =
		fs::read_to_string(Path::new(UI_DIR).join("components.css")).expect("read components.css");
	let own_classes = css_classes(&components);
	let tokens = color_tokens(&theme);
	let full = encre_config(&tokens, encre_css::Preflight::default());
	let check = encre_config(&tokens, encre_css::Preflight::None);

	let mut unknown: Vec<&str> = files
		.iter()
		.zip(&sources)
		.flat_map(|(path, src)| class_lists(path, src))
		.flat_map(str::split_whitespace)
		.filter(|class| !own_classes.contains(*class))
		.filter(|class| encre_css::generate([*class], &check).trim().is_empty())
		.collect();
	unknown.sort_unstable();
	unknown.dedup();
	assert!(
		unknown.is_empty(),
		"neither encre-css nor components.css generates CSS for these classes in src/ui: {unknown:?}"
	);

	let utilities = without_moz_focusring(&encre_css::generate(
		sources.iter().map(String::as_str),
		&full,
	));
	let out = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR")).join("app.css");
	fs::write(
		out,
		format!("{}\n{}\n{utilities}", squeeze(&theme), squeeze(&components)),
	)
	.expect("write app.css");
}

/// Removes the preflight rule `:-moz-focusring { outline: auto }`. Only Firefox knows that
/// pseudo-class, and the utilities come after `theme.css` and `components.css`, so at equal
/// specificity the rule beat every focus style of ours there: the command palette input got
/// a thick native ring inside the palette's own, and `outline: none` was ignored. Without it
/// the cascade is the same in every engine (`:focus-visible` in `theme.css`).
fn without_moz_focusring(css: &str) -> String {
	let mut out = String::with_capacity(css.len());
	let mut rest = css;
	while let Some(at) = rest.find(":-moz-focusring") {
		let Some(end) = rest[at..].find('}') else {
			break;
		};
		out.push_str(&rest[..at]);
		rest = &rest[at + end + 1..];
	}
	out.push_str(rest);
	out
}

/// Drops comments and the whitespace that carries no meaning (around `{`, `}`, `;`, `,` and
/// after `:`), so the hand-written parts weigh what they say. Strings are copied as they are.
fn squeeze(css: &str) -> String {
	let mut out = String::with_capacity(css.len() / 2);
	let mut chars = css.chars().peekable();
	let mut pending_space = false;
	while let Some(c) = chars.next() {
		if c == '/' && chars.peek() == Some(&'*') {
			chars.next();
			let mut prev = ' ';
			for d in chars.by_ref() {
				if prev == '*' && d == '/' {
					break;
				}
				prev = d;
			}
			pending_space = true;
			continue;
		}
		if c.is_whitespace() {
			pending_space = true;
			continue;
		}
		if pending_space {
			let tight_before = out.chars().last().is_none_or(|l| "{};,:".contains(l));
			if !tight_before && !"{};,".contains(c) {
				out.push(' ');
			}
			pending_space = false;
		}
		out.push(c);
		if c == '"' || c == '\'' {
			while let Some(d) = chars.next() {
				out.push(d);
				if d == '\\' {
					out.extend(chars.next());
				} else if d == c {
					break;
				}
			}
		}
	}
	out
}

/// Every `--name:` declared in the first `:root` block of `theme.css`, so a
/// token added there becomes `bg-name`, `text-name`, ... with no other edit.
fn color_tokens(theme: &str) -> Vec<&str> {
	let block = theme
		.split_once(":root {")
		.and_then(|(_, rest)| rest.split_once('}'))
		.expect("theme.css has a :root block")
		.0;
	block
		.lines()
		.filter_map(|line| line.trim().strip_prefix("--")?.split_once(':'))
		.map(|(name, _)| name)
		.collect()
}

/// Class names that `css` defines as selectors (`.ui-dialog`). A dot after a digit
/// is a decimal point, not a class.
fn css_classes(css: &str) -> BTreeSet<&str> {
	let bytes = css.as_bytes();
	let mut classes = BTreeSet::new();
	for (i, _) in css.match_indices('.') {
		if i > 0 && bytes[i - 1].is_ascii_digit() {
			continue;
		}
		let rest = &css[i + 1..];
		let end = rest
			.find(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'))
			.unwrap_or(rest.len());
		if end > 0 {
			classes.insert(&rest[..end]);
		}
	}
	classes
}

fn encre_config(tokens: &[&str], preflight: encre_css::Preflight) -> encre_css::Config {
	let mut config = encre_css::Config::default();
	config.preflight = preflight;
	for token in tokens {
		config
			.theme
			.colors
			.add(token.to_string(), format!("var(--{token})"));
	}
	config
}

/// Class lists that must all produce CSS:
///
/// - `class="..."` attributes in every template;
/// - in `components.rs` and `components/*.rs`, every string literal of a
///   `const NAME: &str = ...;` item (including `concat!` parts). Constants in those
///   files are class lists by convention, so anything else (paths, ids) lives
///   outside them.
fn class_lists<'a>(path: &Path, src: &'a str) -> Vec<&'a str> {
	let mut lists: Vec<&str> = src
		.split("class=\"")
		.skip(1)
		.filter_map(|rest| rest.split('"').next())
		.collect();
	let in_components = path.file_name().is_some_and(|f| f == "components.rs")
		|| path
			.parent()
			.and_then(Path::file_name)
			.is_some_and(|d| d == "components");
	if in_components {
		for (i, _) in src.match_indices("const ") {
			let item = &src[i..];
			let Some(end) = item.find(';') else { continue };
			let item = &item[..end];
			// `const X: &str = ...` only; skip `const fn`, arrays and the like.
			if !item
				.split('=')
				.next()
				.is_some_and(|head| head.contains(": &str"))
			{
				continue;
			}
			let Some((_, value)) = item.split_once('=') else {
				continue;
			};
			lists.extend(value.split('"').skip(1).step_by(2));
		}
	}
	lists
}

const TEMPLATES_DIR: &str = "templates";
const LOGOS_DIR: &str = "static/logos";

fn files_in(dir: &str, dirs: bool) -> Vec<PathBuf> {
	let mut paths: Vec<PathBuf> = fs::read_dir(dir)
		.map(|d| {
			d.flatten()
				.map(|e| e.path())
				.filter(|p| p.is_dir() == dirs)
				.collect()
		})
		.unwrap_or_default();
	paths.sort();
	paths
}

fn file_name(path: &Path) -> String {
	path.file_name()
		.expect("file name")
		.to_string_lossy()
		.to_string()
}

fn is_image(name: &str) -> bool {
	Path::new(name)
		.extension()
		.is_some_and(|e| e == "svg" || e == "png")
}

/// Writes `$OUT_DIR/embedded.rs`: every file of every template under
/// `templates/<id>/` and every logo under `static/logos`, as `include_bytes!`,
/// so the binary carries them and nothing is read from disk at run time.
fn embed_templates() {
	println!("cargo::rerun-if-changed={TEMPLATES_DIR}");
	println!("cargo::rerun-if-changed={LOGOS_DIR}");
	let root = env::current_dir().expect("current dir");
	let include = |path: &Path| {
		format!(
			"include_bytes!({:?})",
			root.join(path).to_string_lossy().as_ref()
		)
	};

	let mut out = String::from("/// Files of each template, by id.\n");
	out.push_str("/// A template's files: name and content.\npub type Files = &'static [(&'static str, &'static [u8])];\n");
	out.push_str("pub static TEMPLATES: &[(&str, Files)] = &[\n");
	for dir in files_in(TEMPLATES_DIR, true) {
		let _ = writeln!(out, "\t({:?}, &[", file_name(&dir));
		for file in files_in(&dir.to_string_lossy(), false) {
			let _ = writeln!(out, "\t\t({:?}, {}),", file_name(&file), include(&file));
		}
		out.push_str("\t]),\n");
	}
	out.push_str("];\n\n/// Registry logos: file name and content.\n");
	out.push_str("pub static LOGOS: &[(&str, &[u8])] = &[\n");
	for file in files_in(LOGOS_DIR, false) {
		let name = file_name(&file);
		if is_image(&name) {
			let _ = writeln!(out, "\t({name:?}, {}),", include(&file));
		}
	}
	out.push_str("];\n");
	let path = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR")).join("embedded.rs");
	fs::write(path, out).expect("write embedded.rs");
}
