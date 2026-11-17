//! Generates the web UI stylesheet: encre-css scans the templates under
//! `src/ui` for Tailwind-compatible utility classes and emits only the CSS
//! they use, into `$OUT_DIR/app.css`.

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

	let css = encre_css::generate(
		sources.iter().map(String::as_str),
		&encre_css::Config::default(),
	);
	let out = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR")).join("app.css");
	fs::write(out, css).expect("write app.css");
}
