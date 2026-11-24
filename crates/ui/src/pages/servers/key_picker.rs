//! The SSH key step of "Add a server": pick a key from the team's vault, import one by
//! pasting it, or have podkit make one. A made key is shown right away (its public half, to
//! copy to the server), because the bootstrap that follows needs it already installed.
//!
//! The private half is write-only: a pasted key travels in a signal once and is cleared
//! after the request; nothing here renders a stored one.

use maud::{Markup, html};

use crate::components::{BannerKind, LABEL, SELECT, TEXTAREA, banner, copy_field};
use crate::sse::js_string;
use podkit_core::ids::TeamId;
use podkit_core::ssh_keys::service::KeyView;

/// The id the generate action patches.
pub(super) const PICKER_ID: &str = "add-key-picker";

/// `/teams/{t}/servers/key`: makes a key for the dialog.
pub(super) fn generate_url(team: TeamId) -> String {
	format!("/teams/{team}/servers/key")
}

/// The picker. `keys` are the vault's, `can_create` whether the user may add to it, and
/// `made` the key just generated (selected, with its public half to copy).
pub(super) fn key_picker(
	team: TeamId,
	keys: &[KeyView],
	can_create: bool,
	made: Option<&KeyView>,
) -> Markup {
	let on_change = format!(
		"el.value === 'generate' && @post({})",
		js_string(&generate_url(team))
	);
	html! {
		div id=(PICKER_ID) class="flex flex-col gap-4" {
			label class=(LABEL) {
				"SSH key"
				select class=(SELECT) required "data-bind:ssh_key_id" "data-on:change"=(on_change) {
					button { selectedcontent {} }
					option value="" { "Choose a key" }
					@for key in keys {
						option value=(key.id.0) selected[made.is_some_and(|m| m.id == key.id)] {
							(key.name) " \u{b7} " (key.algorithm_label())
						}
					}
					@if can_create {
						option value="import" { "Import a key\u{2026}" }
						option value="generate" { "Generate a new key\u{2026}" }
					}
				}
				@if keys.is_empty() && !can_create {
					span class="text-xs font-normal text-destructive" {
						"The vault has no keys, and your role cannot add one."
					}
				} @else {
					span class="ui-hint" { "podkit logs in to the server with this key. Keys live on the Keys page; one key can serve many servers." }
				}
			}
			label class=(LABEL) "data-show"="$ssh_key_id === 'import'" style="display: none" {
				"Private key"
				textarea class=(TEXTAREA)
					rows="6" spellcheck="false" autocomplete="off" autocapitalize="off"
					"data-attr:required"="$ssh_key_id === 'import'"
					placeholder="Paste an OpenSSH or PEM private key" "data-bind:ssh_private_key" {}
				span class="text-xs font-normal text-muted-foreground" {
					"Stored encrypted in the vault, under this server's name. It is never shown again, here or in the API. A key with a passphrase is refused."
				}
			}
			@if let Some(key) = made {
				(banner(
					BannerKind::Info,
					Some("Key made."),
					"Add this public key to the server's ~/.ssh/authorized_keys for the SSH user, then continue.",
					None,
				))
				(copy_field("add-key-public", "Public key", &key.public_key))
			}
		}
	}
}
