//! Generates `$OUT_DIR/embedded.rs`: the registry logos under `static/logos`, so the binary
//! carries them.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::{env, fs};

fn main() {
	embed_templates();
}

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
	println!("cargo::rerun-if-changed={LOGOS_DIR}");
	let root = env::current_dir().expect("current dir");
	let include = |path: &Path| {
		format!(
			"include_bytes!({:?})",
			root.join(path).to_string_lossy().as_ref()
		)
	};

	let mut out = String::from("/// Registry logos: file name and content.\n");
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
