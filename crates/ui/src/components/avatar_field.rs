//! Avatar field: choose a picture, see it before it is sent, upload or remove it.
//!
//! A plain multipart `<form>`: it works without script, and the browser does the file
//! picking. Datastar only adds the preview and the size check (the server checks again).
//! The picture goes in the `file` part; the server crops it square and resizes it.

use maud::{Markup, html};

use super::{BUTTON, BUTTON_DANGER};
use podkit_core::avatars::MAX_UPLOAD;

static ACCEPT: &str = "image/png,image/jpeg,image/webp,image/gif";

/// Runs when a file is chosen: refuses one over the limit through the browser's own
/// validation message, else shows it as the preview.
fn on_change() -> String {
	format!(
		"const f = el.files[0]; $_big = !!f && f.size > {MAX_UPLOAD}; \
		 el.setCustomValidity($_big ? 'The picture is over 2 MB.' : ''); \
		 $_preview = f && !$_big ? URL.createObjectURL(f) : ''"
	)
}

pub struct AvatarField<'a> {
	/// Unique on the page: the input's id.
	pub id: &'a str,
	/// What the picture is of, for the preview's name: `team`, `account`.
	pub subject: &'a str,
	/// The current avatar, drawn large.
	pub current: &'a Markup,
	/// Where the form posts an upload.
	pub action: &'a str,
	/// Where the Remove button posts; `None` when there is nothing to remove.
	pub remove_action: Option<&'a str>,
}

pub fn avatar_field(field: &AvatarField<'_>) -> Markup {
	let hint_id = format!("{}-hint", field.id);
	html! {
		form class="ui-avatar-field" method="post" action=(field.action) enctype="multipart/form-data"
			data-signals="{_preview: '', _big: false}" {
			div class="ui-avatar-field-pic" {
				(field.current)
				img class="ui-avatar-pic ui-avatar-preview" alt={ "Preview of the new " (field.subject) " picture" }
					"data-show"="$_preview" "data-attr:src"="$_preview" style="display: none";
			}
			div class="ui-avatar-field-body" {
				div class="ui-field-group" {
					label class="ui-label" for=(field.id) { "Image file" }
					input id=(field.id) class="ui-file" type="file" name="file" accept=(ACCEPT) required
						aria-describedby=(hint_id) "data-on:change"=(on_change());
					p id=(hint_id) class="ui-hint" { "PNG, JPEG, WebP or GIF, up to 2 MB. It is cropped to a square." }
				}
				div class="ui-form-actions" {
					button type="submit" class=(BUTTON) { "Upload picture" }
					@if let Some(remove) = field.remove_action {
						button type="submit" class=(BUTTON_DANGER) formaction=(remove) formnovalidate { "Remove" }
					}
				}
			}
		}
	}
}
