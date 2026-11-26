//! Lays the start-up code of the release binary out in one block.
//!
//! `scripts/build-ordered.sh` sets `PODKIT_SYMBOL_ORDER` to a file of symbol names and links
//! twice: the first link writes a map next to it, from which the script fills the file. A
//! plain `cargo build` sets nothing and links as usual. See docs/footprint.md, "Start-up order".

fn main() {
	println!("cargo:rerun-if-env-changed=PODKIT_SYMBOL_ORDER");
	let Some(order) = std::env::var_os("PODKIT_SYMBOL_ORDER") else {
		return;
	};
	let order = order.to_string_lossy();
	println!("cargo:rerun-if-changed={order}");
	for arg in [
		format!("--symbol-ordering-file={order}"),
		"--no-warn-symbol-ordering".to_owned(),
		format!("-Map={order}.map"),
	] {
		println!("cargo:rustc-link-arg-bin=server=-Wl,{arg}");
	}
}
