//! The fields of an application, shared by the create page and the General tab: what the
//! forms send (`AppSignals`), how that becomes an `AppInput`, and the groups of fields.
//!
//! Credentials are write-only. The forms send them to the server; nothing here puts a stored
//! deploy key, token or registry password into HTML or a signal. The hints say whether one is
//! stored, never what it is.

use maud::{Markup, html};
use serde::Deserialize;
use serde_json::json;

use crate::applications::service::{AppInput, ApplicationView};
use crate::applications::{BuildStrategy, SourceKind};
use crate::auth::permission::{Action, Resource};
use crate::change::Change;
use crate::error::ServerError;
use crate::ids::{GitProviderId, RegistryId, SshKeyId, TeamId};
use crate::registries::Registry;
use crate::servers::{Server, ServerStatus};
use crate::ssh_keys::service::{self as key_service, KeyView};
use crate::state::AppState;
use crate::ui::components::{INPUT, LABEL, SELECT, TEXTAREA, copy_field};
use crate::ui::extract::TeamCtx;
use crate::ui::status::Status;

/// Everything the forms send, as strings: a number input would come back from Datastar as a
/// number or a string depending on use.
#[derive(Deserialize, Default)]
#[serde(default)]
pub(super) struct AppSignals {
	pub name: String,
	pub slug: String,
	pub server_id: String,
	/// Create only: `git` (the default) or `image`. An existing application keeps its own.
	pub source: String,
	/// Create only: the environment's id, empty for the project's first.
	pub environment: String,
	pub repo_url: String,
	/// Create only, with `source` = `provider`: the git provider's id and the repository there.
	pub git_provider_id: String,
	pub git_repo: String,
	pub git_ref: String,
	/// A vault key's id, `import` (the pasted `deploy_key`) or `new`; empty keeps what is stored.
	pub deploy_key_id: String,
	pub deploy_key: String,
	pub git_token: String,
	pub build_strategy: String,
	pub dockerfile_path: String,
	pub image: String,
	pub registry_username: String,
	pub registry_password: String,
	/// A saved registry's id, empty for none.
	pub registry_id: String,
	pub container_port: String,
	pub memory_limit_mb: String,
	pub cpu_limit: String,
	/// The Health section (General tab only): `off`, `http` or `command`, empty when the
	/// page did not have the section, and the checks' numbers as typed.
	pub hc_mode: String,
	pub hc_path: String,
	pub hc_status: String,
	pub hc_command: String,
	pub hc_interval: String,
	pub hc_timeout: String,
	pub hc_retries: String,
	pub hc_start: String,
	pub hc_wait: String,
	pub hc_drain: String,
	pub hc_window: String,
	pub hc_after: String,
}

fn optional<T: std::str::FromStr>(raw: &str, what: &str) -> Result<Option<T>, ServerError> {
	let raw = raw.trim();
	if raw.is_empty() {
		return Ok(None);
	}
	raw.parse()
		.map(Some)
		.map_err(|_| ServerError::Validation(format!("{what} must be a number.")))
}

impl AppSignals {
	/// Whether the form describes an image application. `existing` is the stored kind on a
	/// save, which a client cannot change.
	pub fn is_image(&self, existing: Option<SourceKind>) -> bool {
		match existing {
			Some(kind) => kind == SourceKind::Image,
			None => self.source == "image",
		}
	}

	/// What the registry select asks for. Empty means none: on a save that clears a saved
	/// registry, on create there is nothing to clear.
	fn registry_choice(
		&self,
		image: bool,
		saving: bool,
	) -> Result<Change<RegistryId>, ServerError> {
		let raw = self.registry_id.trim();
		if !image {
			return Ok(Change::Keep);
		}
		if raw.is_empty() {
			return Ok(if saving { Change::Clear } else { Change::Keep });
		}
		raw.parse()
			.map(|id| Change::Set(RegistryId(id)))
			.map_err(|_| ServerError::Validation("Unknown registry.".to_owned()))
	}

	pub fn input(&self, existing: Option<SourceKind>) -> Result<AppInput, ServerError> {
		let container_port = optional::<i32>(&self.container_port, "Container port")?
			.ok_or_else(|| ServerError::Validation("Container port is required.".to_owned()))?;
		let or = |value: &str, default: &str| {
			let value = value.trim();
			if value.is_empty() { default } else { value }.to_owned()
		};
		let secret = |value: &str| (!value.is_empty()).then(|| value.to_owned());
		let image = self.is_image(existing);
		let provider = existing.is_none() && self.source == "provider";
		// (pasted key to import, vault key, make a new one). A pasted key still imports with
		// no choice made, as it did before the vault.
		let https = self.repo_url.trim().starts_with("https://");
		let deploy_key = if image || provider {
			(None, None, false)
		} else {
			match self.deploy_key_id.trim() {
				"new" => (None, None, true),
				"" | "import" => (secret(&self.deploy_key), None, false),
				// A key picked earlier does not apply to an https URL, which takes a token.
				_ if https => (None, None, false),
				id => (None, id.parse::<i64>().ok().map(SshKeyId), false),
			}
		};
		let (repo_url, image_ref) = if image {
			if self.image.trim().is_empty() {
				return Err(ServerError::Validation("Image is required.".to_owned()));
			}
			(String::new(), Some(self.image.trim().to_owned()))
		} else if provider {
			if self.git_repo.trim().is_empty() {
				return Err(ServerError::Validation(
					"Pick a repository from the git provider.".to_owned(),
				));
			}
			(String::new(), None)
		} else {
			if self.repo_url.trim().is_empty() {
				return Err(ServerError::Validation(
					"Repository URL is required.".to_owned(),
				));
			}
			(self.repo_url.trim().to_owned(), None)
		};
		let git_provider_id = if provider {
			Some(
				self.git_provider_id
					.trim()
					.parse::<i64>()
					.map(GitProviderId)
					.map_err(|_| ServerError::Validation("Pick a git provider.".to_owned()))?,
			)
		} else {
			None
		};
		Ok(AppInput {
			name: self.name.clone(),
			slug: self.slug.trim().to_owned(),
			repo_url,
			image: image_ref,
			registry_username: image
				.then(|| self.registry_username.trim())
				.filter(|v| !v.is_empty())
				.map(str::to_owned),
			registry_password: image.then(|| secret(&self.registry_password)).flatten(),
			registry: self.registry_choice(image, existing.is_some())?,
			git_ref: or(&self.git_ref, "main"),
			deploy_key: deploy_key.0,
			deploy_key_id: deploy_key.1,
			generate_deploy_key: deploy_key.2,
			git_token: (!image && !provider)
				.then(|| secret(self.git_token.trim()))
				.flatten(),
			build_strategy: or(&self.build_strategy, "dockerfile"),
			dockerfile_path: or(&self.dockerfile_path, "Dockerfile"),
			container_port,
			memory_limit_mb: optional(&self.memory_limit_mb, "Memory limit")?,
			cpu_limit: optional(&self.cpu_limit, "CPU limit")?,
			environment_id: None,
			git_provider_id,
			git_repo: if provider {
				self.git_repo.trim().to_owned()
			} else {
				String::new()
			},
		})
	}
}

/// The vault keys the deploy-key picker offers, and whether the user may add one. Without
/// `read ssh_key` the list is empty, and only what the role allows is offered.
pub(super) async fn load_keys(s: &AppState, ctx: &TeamCtx) -> (Vec<KeyView>, bool) {
	let keys = if ctx.perms.can(Action::Read, Resource::SshKey) {
		key_service::list(&s.db, ctx.user.id, ctx.team.id)
			.await
			.unwrap_or_default()
	} else {
		Vec::new()
	};
	(keys, ctx.perms.can(Action::Create, Resource::SshKey))
}

/// The signals that hold credentials, emptied after a request that took them.
pub(super) fn clear_credentials() -> serde_json::Value {
	json!({ "deploy_key": "", "git_token": "", "registry_password": "" })
}

/// The signals of an empty create form.
pub(super) fn blank_signals(servers: &[Server], source: &str, environment: &str) -> String {
	let server = servers
		.iter()
		.find(|s| s.status == ServerStatus::Active)
		.map(|s| s.id.to_string())
		.unwrap_or_default();
	json!({
		"name": "", "slug": "", "server_id": server, "source": source, "environment": environment,
		"repo_url": "", "git_provider_id": "", "git_repo": "", "git_ref": "main", "deploy_key_id": "", "deploy_key": "", "git_token": "",
		"build_strategy": "dockerfile", "dockerfile_path": "Dockerfile",
		"image": "", "registry_username": "", "registry_password": "", "registry_id": "",
		"container_port": "", "memory_limit_mb": "", "cpu_limit": "", "_error": "", "_repo_q": "",
		"_slugEdited": false,
	})
	.to_string()
}

/// The signals of the General tab's form.
pub(super) fn prefilled_signals(app: &ApplicationView) -> String {
	json!({
		"name": app.name, "slug": app.slug, "repo_url": app.repo_url.as_deref().unwrap_or_default(),
		"git_ref": app.git_ref, "deploy_key_id": app.deploy_key_id.map(|k| k.0.to_string()).unwrap_or_default(),
		"deploy_key": "", "git_token": "",
		"build_strategy": app.build_strategy.as_str(), "dockerfile_path": app.dockerfile_path,
		"image": app.image.as_deref().unwrap_or_default(),
		"registry_username": "", "registry_password": "",
		"registry_id": app.registry_id.map(|r| r.0.to_string()).unwrap_or_default(),
		"container_port": app.container_port.to_string(),
		"memory_limit_mb": app.memory_limit_mb.map(|m| m.to_string()).unwrap_or_default(),
		"cpu_limit": app.cpu_limit.map(|c| c.to_string()).unwrap_or_default(),
		"_error": "",
	})
	.to_string()
}

pub(super) const HELP: &str = "text-xs font-normal text-muted-foreground";

const NO_CREDENTIAL: &str =
	"Only for private repositories. It is stored encrypted and never shown again.";

/// What the deploy key looks like to the user: which vault key the application uses and its
/// public half to copy to the git provider. The private half is never here. Patched in place
/// after a save.
pub(super) fn deploy_key_hint(app: Option<&ApplicationView>, current: Option<&KeyView>) -> Markup {
	html! {
		div id="deploy-key-hint" class="flex flex-col gap-2" {
			@if let Some(key) = current {
				p class=(HELP) {
					"Uses the key " strong { (key.name) } " (" span class="font-mono" { (key.fingerprint) } "). "
					"Add its public key as a deploy key at your git provider; pick another key to switch."
				}
				(copy_field("deploy-key-public", "Public key", &key.public_key))
			} @else if app.is_some_and(|a| a.has_deploy_key) {
				p class=(HELP) { "A deploy key is set. Pick another key to replace it." }
			} @else {
				p class=(HELP) { "Only for private repositories. The key lives in the vault; its private half is never shown." }
			}
		}
	}
}

/// Same for the git access token.
pub(super) fn git_token_hint(app: Option<&ApplicationView>) -> Markup {
	html! {
		p id="git-token-hint" class=(HELP) {
			@if app.is_some_and(|a| a.has_git_token) {
				"A token is stored. Leave this empty to keep it; enter a new one to replace it."
			} @else { (NO_CREDENTIAL) }
		}
	}
}

/// Same for registry credentials.
pub(super) fn registry_hint(app: Option<&ApplicationView>) -> Markup {
	html! {
		p id="registry-hint" class=(HELP) {
			@if app.is_some_and(|a| a.has_registry_auth) {
				"Registry credentials are stored. Leave both empty to keep them; fill both to replace them."
			} @else {
				"Only for a private registry. Both are stored encrypted and never shown again."
			}
		}
	}
}

/// Name and slug; with `servers` also where to run (a created application cannot move).
/// With `derive_slug` the slug follows the name until someone edits it.
pub(super) fn identity_fields(servers: Option<&[Server]>, derive_slug: bool) -> Markup {
	let derive = "if (!$_slugEdited) $slug = $name.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-+|-+$/g, '').slice(0, 60)";
	html! {
		div class="grid grid-cols-1 gap-4 sm:grid-cols-2" {
			label class=(LABEL) {
				"Name"
				@if derive_slug {
					input class=(INPUT) type="text" required maxlength="60" autocomplete="off" autofocus
						placeholder="My app" "data-bind:name" "data-on:input"=(derive);
				} @else {
					input class=(INPUT) type="text" required maxlength="60" autocomplete="off" "data-bind:name";
				}
			}
			label class=(LABEL) {
				"Slug"
				@if derive_slug {
					input class={ (INPUT) " font-mono" } type="text" required maxlength="60" autocomplete="off" spellcheck="false"
						pattern="[a-z0-9]+(-[a-z0-9]+)*" placeholder="my-app" "data-bind:slug"
						"data-on:input"="$_slugEdited = true";
				} @else {
					input class={ (INPUT) " font-mono" } type="text" required maxlength="60" autocomplete="off" spellcheck="false"
						pattern="[a-z0-9]+(-[a-z0-9]+)*" placeholder="my-app" "data-bind:slug";
				}
				span class=(HELP) { "Part of the public hostname. Lowercase letters, digits and hyphens; unique per server." }
			}
		}
		@if let Some(servers) = servers {
			label class=(LABEL) {
				"Server"
				select class=(SELECT) required "data-bind:server_id" {
					button { selectedcontent {} }
					@for server in servers {
						option value=(server.id.0) disabled[server.status != ServerStatus::Active] {
							(server.name)
							@if server.status != ServerStatus::Active { " (" (server.status.label()) ")" }
						}
					}
				}
				@if !servers.iter().any(|s| s.status == ServerStatus::Active) {
					span class="text-xs font-normal text-destructive" { "No server is active, so nothing can be deployed yet. Check the Servers page." }
				}
			}
		}
	}
}

/// Where a Git application comes from: repository, ref and the credential its URL needs. An
/// ssh URL takes a deploy key from the team's vault (`keys`), a pasted one imported into it,
/// or a new one; an https URL takes a token.
pub(super) fn git_source_fields(
	app: Option<&ApplicationView>,
	keys: &[KeyView],
	can_create_key: bool,
) -> Markup {
	html! {
		label class=(LABEL) {
			"Repository URL"
			input class={ (INPUT) " font-mono" } type="text" required autocomplete="off" spellcheck="false"
				placeholder="https://github.com/you/app.git or git@github.com:you/app.git" "data-bind:repo_url";
		}
		label class=(LABEL) {
			"Branch, tag or ref"
			input class={ (INPUT) " font-mono" } type="text" placeholder="main" autocomplete="off" spellcheck="false" "data-bind:git_ref";
		}
		(deploy_key_field(app, keys, can_create_key))
		label class=(LABEL) "data-show"="$repo_url.startsWith('https://')" style="display: none" {
			"Access token"
			input class=(INPUT) type="password" autocomplete="off" placeholder="Only for private repositories" "data-bind:git_token";
			(git_token_hint(app))
		}
	}
}

/// The deploy key picker of an ssh repository: a vault key, an import or a new key, then the
/// public half of the chosen one to copy. Patched in place after a save, so a key just made
/// shows up in the list.
pub(super) fn deploy_key_field(
	app: Option<&ApplicationView>,
	keys: &[KeyView],
	can_create_key: bool,
) -> Markup {
	let current = app
		.and_then(|a| a.deploy_key_id)
		.and_then(|id| keys.iter().find(|k| k.id == id));
	let keep = app.is_some_and(|a| a.has_deploy_key);
	html! {
		div id="deploy-key-field" class="flex flex-col gap-4"
			"data-show"="$repo_url !== '' && !$repo_url.startsWith('https://')" style="display: none" {
			label class=(LABEL) {
				"Deploy key"
				select class=(SELECT) "data-bind:deploy_key_id" {
					button { selectedcontent {} }
					@if !keep { option value="" { "No deploy key (public repository)" } }
					@for key in keys {
						option value=(key.id.0) selected[current.is_some_and(|c| c.id == key.id)] {
							(key.name) " \u{b7} " (key.algorithm_label())
						}
					}
					@if can_create_key {
						option value="import" { "Import a key\u{2026}" }
						option value="new" { "Generate a new key" }
					}
				}
			}
			label class=(LABEL) "data-show"="$deploy_key_id === 'import'" style="display: none" {
				"Private key"
				textarea class=(TEXTAREA) rows="5" autocomplete="off" spellcheck="false" autocapitalize="off"
					placeholder="Paste an OpenSSH or PEM private key" "data-bind:deploy_key" {}
				span class=(HELP) { "Imported into the vault under this application's name. A key with a passphrase is refused." }
			}
			p class=(HELP) "data-show"="$deploy_key_id === 'new'" style="display: none" {
				"podkit makes an Ed25519 key when you save. Its public key is shown then, to add as a deploy key at your git provider."
			}
			(deploy_key_hint(app, current))
		}
	}
}

/// Where an image application comes from: the image and, for a private registry, a saved
/// registry (or, for this application only, a login of its own).
pub(super) fn image_source_fields(
	app: Option<&ApplicationView>,
	registries: &[Registry],
	team: TeamId,
) -> Markup {
	html! {
		label class=(LABEL) {
			"Image"
			input class={ (INPUT) " font-mono" } type="text" required autocomplete="off" spellcheck="false" autocapitalize="off"
				placeholder="ghcr.io/you/app:1.2 or traefik/whoami" "data-bind:image";
			span class=(HELP) { "A registry reference. Without a tag podkit pulls " code class="font-mono" { "latest" } "." }
		}
		label class=(LABEL) {
			"Registry"
			select class=(SELECT) "data-bind:registry_id" {
				button { selectedcontent {} }
				option value="" { "None (public image)" }
				@for registry in registries {
					option value=(registry.id.0) { (registry.name) " \u{b7} " (registry.url) }
				}
			}
			span class=(HELP) {
				@if registries.is_empty() {
					"No saved registries yet. "
				} @else {
					"The login used to pull the image. "
				}
				a class="underline underline-offset-2" href={ "/teams/" (team) "/registries" } { "Manage registries" }
				"."
			}
		}
		details class="ui-disclosure" {
			summary { "Use a login for this application only" }
			div class="grid grid-cols-1 gap-4 sm:grid-cols-2" {
				label class=(LABEL) {
					"Registry username"
					input class=(INPUT) type="text" autocomplete="off" spellcheck="false" autocapitalize="off" placeholder="Optional" "data-bind:registry_username";
				}
				label class=(LABEL) {
					"Registry password or token"
					input class=(INPUT) type="password" autocomplete="off" placeholder="Optional" "data-bind:registry_password";
				}
			}
		}
		(registry_hint(app))
	}
}

/// How a Git application is built.
pub(super) fn build_fields() -> Markup {
	html! {
		label class=(LABEL) {
			"Build strategy"
			select class=(SELECT) "data-bind:build_strategy" {
				button { selectedcontent {} }
				option value=(BuildStrategy::Dockerfile.as_str()) { "Dockerfile" }
				option value=(BuildStrategy::Nixpacks.as_str()) { "Nixpacks (detect automatically)" }
			}
		}
		label class=(LABEL) "data-show"="$build_strategy === 'dockerfile'" {
			"Dockerfile path"
			input class={ (INPUT) " font-mono" } type="text" placeholder="Dockerfile" autocomplete="off" spellcheck="false" "data-bind:dockerfile_path";
			span class=(HELP) { "Relative to the repository root." }
		}
	}
}

/// The port the application listens on and its resource limits.
pub(super) fn runtime_fields() -> Markup {
	html! {
		label class=(LABEL) {
			"Container port"
			input class=(INPUT) type="text" inputmode="numeric" required pattern="[0-9]{1,5}" placeholder="3000"
				autocomplete="off" "data-bind:container_port";
			span class=(HELP) { "The port the application listens on inside its container." }
		}
		div class="grid grid-cols-1 gap-4 sm:grid-cols-2" {
			label class=(LABEL) {
				"Memory limit (MB)"
				input class=(INPUT) type="text" inputmode="numeric" pattern="[0-9]+" placeholder="No limit" autocomplete="off" "data-bind:memory_limit_mb";
			}
			label class=(LABEL) {
				"CPU limit (cores)"
				input class=(INPUT) type="text" inputmode="decimal" pattern="[0-9]*\\.?[0-9]+" placeholder="No limit" autocomplete="off" "data-bind:cpu_limit";
			}
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn signals(extra: &serde_json::Value) -> AppSignals {
		let mut base = json!({
			"name": "Web", "slug": "web", "repo_url": "https://example.com/web.git",
			"container_port": "3000",
		});
		for (k, v) in extra.as_object().unwrap() {
			base[k] = v.clone();
		}
		serde_json::from_value(base).unwrap()
	}

	#[test]
	fn a_git_form_drops_registry_fields_and_an_image_form_drops_git_ones() {
		let git = signals(&json!({ "registry_username": "u", "registry_password": "p" }))
			.input(None)
			.unwrap();
		assert_eq!(git.repo_url, "https://example.com/web.git");
		assert!(git.image.is_none() && git.registry_username.is_none());
		assert!(git.registry_password.is_none());

		let image = signals(&json!({
			"source": "image", "image": " traefik/whoami ", "deploy_key": "k", "git_token": "t",
			"registry_username": "u", "registry_password": "p",
		}))
		.input(None)
		.unwrap();
		assert_eq!(image.image.as_deref(), Some("traefik/whoami"));
		assert!(image.repo_url.is_empty());
		assert!(image.deploy_key.is_none() && image.git_token.is_none());
		assert_eq!(image.registry_username.as_deref(), Some("u"));
		assert_eq!(image.registry_password.as_deref(), Some("p"));
	}

	#[test]
	fn a_save_keeps_the_stored_kind_whatever_the_client_says() {
		let s = signals(&json!({ "source": "image", "image": "a/b" }));
		assert!(!s.is_image(Some(SourceKind::Git)));
		assert!(s.is_image(None));
		assert!(
			signals(&json!({ "source": "git" })).is_image(Some(SourceKind::Image)),
			"an image app stays an image app"
		);
	}

	#[test]
	fn a_missing_source_or_port_is_a_plain_message() {
		let msg = |s: AppSignals, kind| match s.input(kind) {
			Err(ServerError::Validation(m)) => m,
			other => panic!("{:?}", other.map(|_| ())),
		};
		assert_eq!(
			msg(signals(&json!({ "repo_url": " " })), None),
			"Repository URL is required."
		);
		assert_eq!(
			msg(signals(&json!({ "source": "image" })), None),
			"Image is required."
		);
		assert_eq!(
			msg(signals(&json!({ "container_port": "" })), None),
			"Container port is required."
		);
		assert_eq!(
			msg(signals(&json!({ "container_port": "x" })), None),
			"Container port must be a number."
		);
	}
}
