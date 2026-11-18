//! The design system's components: maud functions, plus class constants for the
//! elements that carry Datastar attributes (maud cannot forward arbitrary attributes
//! through a function). One file per component; the spec and the do's and don'ts are in
//! `docs/design/README.md`, and `/_ui` shows every one of them in every state.
//!
//! Rules for the files below, enforced by `build.rs`:
//!
//! - Class lists are whole string literals, in a `class` attribute or in a
//!   `const NAME: &str = ...;` item. Every class must be a utility encre-css knows or a
//!   `.ui-*` rule in `components.css`, or the build fails.
//! - So a `&str` const in this module tree is always a class list. Anything else (an
//!   asset path, an id prefix) is a `static`, or lives outside `components/`.
//! - No user data reaches `PreEscaped`; maud escapes every text and attribute value.
//!
//! Later cards add NEW files under `components/` instead of editing these.

mod avatar;
mod badge;
mod breadcrumb;
mod button;
mod card;
mod dialog;
mod empty_state;
mod field;
mod icon;
mod kbd;
mod logo;
mod menu;
mod skeleton;
mod table;
mod tabs;
mod theme_toggle;
mod toast;
mod tooltip;

pub use avatar::avatar;
pub use badge::{Status, Tone, badge, status_badge, status_dot};
pub use breadcrumb::breadcrumb;
pub use button::{
	BUTTON, BUTTON_DANGER, BUTTON_OUTLINE, Size, Variant, button, button_class, icon_button,
	link_button,
};
pub use card::{
	card, card_content, card_description, card_footer, card_header, card_section, card_title,
	link_card,
};
pub use dialog::{Dismiss, dialog, dialog_cancel, dialog_open_button};
pub use empty_state::empty_state;
pub use field::{
	CHECKBOX, INPUT, LABEL, SELECT, SWITCH, TEXTAREA, checkbox_field, error_text, help_text,
	select_field, switch_field, text_field, textarea_field,
};
pub use icon::{icon, icon_with};
pub use kbd::{kbd, kbd_combo};
pub use logo::{LogoSize, logo};
pub use menu::{MENU_ITEM, MENU_ITEM_DANGER, dropdown_menu, menu_label, menu_link, menu_separator};
pub use skeleton::skeleton;
pub use table::table;
pub use tabs::{TAB, TAB_ACTIVE, TabItem, tabs};
pub use theme_toggle::theme_toggle;
pub use toast::{toast, toast_region};
pub use tooltip::tooltip;
