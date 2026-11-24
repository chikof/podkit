//! Shared variables: precedence, references, the resolver in every spec builder, secrecy,
//! affected resources and tenancy. Names start with `shared_env` so
//! `cargo test -p podkit-core -p podkit-ui -- shared_env` runs them all.

#[test]
fn followups_shared_env_every_builder_of_a_spec_calls_the_resolver() {
	// The ones that make a `DeploySpec` for a user's workload. The Tailscale sidecar's is
	// podkit's own (its auth key and state path), so it has nothing to inherit. Previews and
	// clones deploy through the application builder, which is listed.
	for (name, source) in [
		("deployments/run.rs", include_str!("../deployments/run.rs")),
		("stacks/run.rs", include_str!("../stacks/run.rs")),
		("databases/run.rs", include_str!("../databases/run.rs")),
		("models/run.rs", include_str!("../models/run.rs")),
		("cron/run.rs", include_str!("../cron/run.rs")),
	] {
		assert!(
			source.contains("shared_env::resolve::"),
			"{name} does not use the shared variable resolver"
		);
	}
	// A builder of a `DeploySpec` that is not listed above is a new one: add it, or say why.
	let mut builders = Vec::new();
	for (name, source) in [
		("deployments/run.rs", include_str!("../deployments/run.rs")),
		(
			"stacks/translate.rs",
			include_str!("../stacks/translate.rs"),
		),
		("databases/run.rs", include_str!("../databases/run.rs")),
		("models/run.rs", include_str!("../models/run.rs")),
		("tailscale/spec.rs", include_str!("../tailscale/spec.rs")),
	] {
		if source.contains("DeploySpec {") {
			builders.push(name);
		}
	}
	assert_eq!(
		builders,
		[
			"deployments/run.rs",
			"stacks/translate.rs",
			"databases/run.rs",
			"models/run.rs",
			"tailscale/spec.rs"
		]
	);
}
