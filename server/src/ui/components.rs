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
mod banner;
mod breadcrumb;
mod button;
mod card;
mod chip;
mod dialog;
mod empty_state;
mod field;
mod icon;
mod image_logo;
mod kbd;
mod log_viewer;
mod logo;
mod menu;
mod metrics;
mod progress;
mod registry_select;
mod skeleton;
mod stepper;
mod table;
mod tabs;
mod theme_toggle;
mod toast;
mod tooltip;

pub use avatar::avatar;
pub use badge::{Status, Tone, badge, status_badge, status_dot};
pub use banner::{BannerKind, banner};
pub use breadcrumb::breadcrumb;
pub use button::{
	BUTTON, BUTTON_DANGER, BUTTON_OUTLINE, Size, Variant, button, button_class, icon_button,
	link_button,
};
pub use card::{
	card, card_content, card_description, card_footer, card_header, card_heading, card_section,
	card_title, link_card,
};
pub use chip::{chip, chip_group};
pub use dialog::{Dismiss, dialog, dialog_cancel, dialog_open_button};
pub use empty_state::empty_state;
pub use field::{
	CHECKBOX, INPUT, LABEL, SELECT, SWITCH, TEXTAREA, checkbox_field, copy_field, error_text,
	help_text, invalid_text, password_field, reveal_button, select_field, switch_field, text_field,
	textarea_field,
};
pub use icon::{icon, icon_with};
pub use image_logo::{Priority, image_logo};
pub use kbd::{kbd, kbd_combo};
pub use log_viewer::{log_viewer, log_viewer_titled};
pub use logo::{LogoSize, logo};
pub use menu::{MENU_ITEM, MENU_ITEM_DANGER, dropdown_menu, menu_label, menu_link, menu_separator};
// `REFRESH_SECONDS` and `resource_charts` are for the application, database, model and stack
// pages (cards F4, F5, F7, F8); until they embed the section only the tests call them.
#[allow(unused_imports)]
pub use metrics::{
	MetricsSection, REFRESH_SECONDS, metrics_body, metrics_section, range_phrase, range_value,
	resource_charts,
};
pub use progress::progress;
pub use registry_select::{registry_choice, registry_select};
pub use skeleton::{skeleton, skeleton_row};
pub use stepper::{Current, stepper};
pub use table::table;
pub use tabs::{RadioTab, TabItem, radio_tabs, tabs};
pub use theme_toggle::theme_toggle;
pub use toast::{toast, toast_region, toast_region_with};
pub use tooltip::tooltip;
