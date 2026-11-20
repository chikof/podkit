//! The dialogs of the Keys page: generate, import, rotate, delete, and one read-only public
//! key per key. Rotate and delete are one dialog each, filled from the signals the row menu
//! sets (`_k_id`, `_k_name`, `_k_uses`); nothing in them is a stored private key.

use maud::{Markup, html};

use super::keys_url;
use crate::ids::TeamId;
use crate::ssh_keys::service::KeyView;
pub(super) use crate::ui::components::dialog_open_button as open_button;
use crate::ui::components::{
	BUTTON, BUTTON_OUTLINE, INPUT, LABEL, SELECT, Size, TEXTAREA, Variant, button_class,
	copy_field, dialog, dialog_cancel,
};
use crate::ui::layout::form_error;
use crate::ui::sse::js_string;

pub(super) const GENERATE: &str = "generate-key";
pub(super) const IMPORT: &str = "import-key";
pub(super) const ROTATE: &str = "rotate-key";
pub(super) const DELETE: &str = "delete-key";

/// The id of a key's public-key dialog.
pub(super) fn public_id(key: &KeyView) -> String {
	format!("public-key-{}", key.id.0)
}

const FOOT: &str = "flex flex-col-reverse gap-2 sm:flex-row sm:justify-end";

/// The public half of one key, to copy.
pub(super) fn public_dialog(key: &KeyView) -> Markup {
	let id = public_id(key);
	dialog(
		&id,
		&format!("Public key of {}", key.name),
		Some(
			"Add it to ~/.ssh/authorized_keys on a server, or as a deploy key at your git provider. It is safe to share.",
		),
		&html! {
			div class="flex flex-col gap-3" {
				@if key.public_key.is_empty() {
					p class="text-sm text-muted-foreground" { "Its public key has not been derived yet. Reload in a moment." }
				} @else {
					(copy_field(&format!("{id}-value"), "Public key", &key.public_key))
					p class="text-xs text-muted-foreground" {
						(key.algorithm_label()) " \u{b7} " span class="ui-key-fp" { (key.fingerprint) }
					}
				}
			}
		},
		&html! { (dialog_cancel(BUTTON_OUTLINE, &html! { "Close" })) },
	)
}

pub(super) fn generate_dialog(team: TeamId) -> Markup {
	let post = format!(
		"evt.preventDefault(); @post({})",
		js_string(&format!("{}/generate", keys_url(team)))
	);
	let body = html! {
		form id="generate-form" class="flex flex-col gap-4" "data-show"="!$_gen_done" "data-on:submit"=(post) "data-indicator:_busy" {
			label class=(LABEL) {
				"Name"
				input class=(INPUT) type="text" required maxlength="60" autocomplete="off" placeholder="deploy-box" autofocus "data-bind:gen_name";
			}
			label class=(LABEL) {
				"Type"
				select class=(SELECT) "data-bind:gen_algorithm" {
					button { selectedcontent {} }
					option value="ed25519" { "Ed25519 (recommended)" }
					option value="rsa" { "RSA 4096" }
				}
				span class="ui-hint" { "Use RSA only for a server or git host that does not accept Ed25519. Making one takes a few seconds." }
			}
			(form_error())
		}
		div id="generate-made" class="flex flex-col gap-3" "data-show"="$_gen_done" style="display: none" {}
	};
	let footer = html! {
		div class=(FOOT) "data-show"="!$_gen_done" {
			(dialog_cancel(BUTTON_OUTLINE, &html! { "Cancel" }))
			button type="submit" form="generate-form" class=(BUTTON) "data-attr:disabled"="$_busy" { "Generate key" }
		}
		div class=(FOOT) "data-show"="$_gen_done" style="display: none" {
			(dialog_cancel(BUTTON, &html! { "Done" }))
		}
	};
	dialog(
		GENERATE,
		"Generate a key",
		Some(
			"podkit makes the key pair here and keeps the private half, encrypted. You only ever see the public half.",
		),
		&body,
		&footer,
	)
}

pub(super) fn import_dialog(team: TeamId) -> Markup {
	let post = format!(
		"evt.preventDefault(); @post({})",
		js_string(&format!("{}/import", keys_url(team)))
	);
	let body = html! {
		form id="import-form" class="flex flex-col gap-4" "data-show"="!$_imp_done" "data-on:submit"=(post) "data-indicator:_busy" {
			label class=(LABEL) {
				"Name"
				input class=(INPUT) type="text" required maxlength="60" autocomplete="off" placeholder="ci-runner" "data-bind:imp_name";
			}
			label class=(LABEL) {
				"Private key"
				textarea class=(TEXTAREA) required rows="7" spellcheck="false" autocomplete="off" autocapitalize="off"
					placeholder="Paste an OpenSSH or PEM private key" "data-bind:imp_key" {}
				span class="ui-hint" { "Stored encrypted and never shown again, here or in the API. A key with a passphrase is refused: remove the passphrase first." }
			}
			(form_error())
		}
		div id="import-made" class="flex flex-col gap-3" "data-show"="$_imp_done" style="display: none" {}
	};
	let footer = html! {
		div class=(FOOT) "data-show"="!$_imp_done" {
			(dialog_cancel(BUTTON_OUTLINE, &html! { "Cancel" }))
			button type="submit" form="import-form" class=(BUTTON) "data-attr:disabled"="$_busy" { "Import key" }
		}
		div class=(FOOT) "data-show"="$_imp_done" style="display: none" {
			(dialog_cancel(BUTTON, &html! { "Done" }))
		}
	};
	dialog(
		IMPORT,
		"Import a key",
		Some("Bring a key you already use. podkit derives the public half from it."),
		&body,
		&footer,
	)
}

pub(super) fn rotate_dialog(team: TeamId) -> Markup {
	let post = format!(
		"@post({} + $_k_id + '/rotate')",
		js_string(&format!("{}/", keys_url(team)))
	);
	let body = html! {
		div class="flex flex-col gap-3" {
			div "data-show"="!$_rotated" class="flex flex-col gap-3" {
				p { "Rotate " strong class="break-all" "data-text"="$_k_name" {} "?" }
				p class="text-sm text-muted-foreground" "data-show"="$_k_uses !== ''" style="display: none" {
					"In use by " span "data-text"="$_k_uses" {} "."
				}
				p class="text-sm text-muted-foreground" "data-show"="$_k_uses === ''" style="display: none" {
					"Nothing uses this key, so there is nothing to rotate."
				}
				p class="text-sm text-muted-foreground" {
					"podkit makes a new Ed25519 key. On each server that uses this one it adds the new public key to authorized_keys, checks that it logs in, switches the server over, then removes the old one. A server that fails keeps its old key."
				}
				p class="text-sm text-muted-foreground" {
					"A git deploy key can't be rotated for you: you will get the new public key to add at your git provider."
				}
			}
			div id="rotate-result" class="flex flex-col gap-3" {}
			p class="text-sm text-destructive" role="alert" "data-show"="$_rotate_error" "data-text"="$_rotate_error" style="display: none" {}
		}
	};
	let footer = html! {
		div class=(FOOT) "data-show"="!$_rotated" {
			(dialog_cancel(BUTTON_OUTLINE, &html! { "Cancel" }))
			button type="button" class=(BUTTON) "data-on:click"=(post) "data-indicator:_rotating"
				"data-attr:disabled"="$_rotating || $_k_uses === ''" { "Rotate key" }
		}
		div class=(FOOT) "data-show"="$_rotated" style="display: none" {
			(dialog_cancel(BUTTON, &html! { "Done" }))
		}
	};
	dialog(
		ROTATE,
		"Rotate this key?",
		Some("Replaces the key on every server that uses it."),
		&body,
		&footer,
	)
}

pub(super) fn delete_dialog(team: TeamId) -> Markup {
	let post = format!(
		"@post({} + $_k_id + '/delete')",
		js_string(&format!("{}/", keys_url(team)))
	);
	dialog(
		DELETE,
		"Delete this key?",
		Some(
			"The private key is deleted for good. Anything that still needs it must be given another.",
		),
		&html! {
			div class="flex flex-col gap-3" {
				p { "Delete " strong class="break-all" "data-text"="$_k_name" {} "?" }
				p class="text-sm text-destructive" "data-show"="$_k_uses !== ''" style="display: none" {
					"It is in use by " span "data-text"="$_k_uses" {} ". Switch them to another key first."
				}
				p class="text-sm text-destructive" role="alert" "data-show"="$_delete_error" "data-text"="$_delete_error" style="display: none" {}
			}
		},
		&html! {
			form method="dialog" {
				button class=(button_class(Variant::Outline, Size::Md)) autofocus { "Keep it" }
			}
			button type="button" class=(button_class(Variant::Destructive, Size::Md)) "data-on:click"=(post)
				"data-indicator:_deleting" "data-attr:disabled"="$_deleting || $_k_uses !== ''" { "Delete key" }
		},
	)
}
