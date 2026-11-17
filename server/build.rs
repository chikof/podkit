//! Generates the web UI stylesheet: encre-css scans the templates under
//! `src/ui` for Tailwind-compatible utility classes and emits only the CSS
//! they use, into `$OUT_DIR/app.css`, after the tokens in `src/ui/theme.css`.

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
	let tokens = color_tokens(&theme);
	let full = encre_config(&tokens, encre_css::Preflight::default());
	let check = encre_config(&tokens, encre_css::Preflight::None);

	let mut unknown: Vec<&str> = files
		.iter()
		.zip(&sources)
		.flat_map(|(path, src)| class_lists(path, src))
		.flat_map(str::split_whitespace)
		.filter(|class| encre_css::generate([*class], &check).trim().is_empty())
		.collect();
	unknown.sort_unstable();
	unknown.dedup();
	assert!(
		unknown.is_empty(),
		"encre-css generates no CSS for these classes in src/ui: {unknown:?}"
	);

	let utilities = encre_css::generate(sources.iter().map(String::as_str), &full);
	let out = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR")).join("app.css");
	fs::write(out, format!("{theme}\n{utilities}")).expect("write app.css");
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

/// Class lists that must all produce CSS: `class="..."` attributes in every
/// template, and every string literal in `components.rs` (its constants and
/// `concat!` parts are all class lists).
fn class_lists<'a>(path: &Path, src: &'a str) -> Vec<&'a str> {
	if path.file_name().is_some_and(|f| f == "components.rs") {
		src.split('"').skip(1).step_by(2).collect()
	} else {
		src.split("class=\"")
			.skip(1)
			.filter_map(|rest| rest.split('"').next())
			.collect()
	}
}
