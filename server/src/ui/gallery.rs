//! `/_ui`: every component in every state and variant, each in a dark and a light panel
//! side by side, for visual review. Not part of the product: it exists only when the
//! server is a debug build or `PODKIT_UI_GALLERY=1` is set, and only for signed-in users.
//!
//! Each component's section is one line of HTML with a `data-component` attribute, so
//! `curl -s /_ui | grep -c 'data-component='` counts them.

use axum::Router;
use axum::http::Method;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use maud::{Markup, html};

use super::assets::ICONS_SVG;
use super::components::Dismiss;
use super::components::{
	BUTTON, CHECKBOX, INPUT, LABEL, LogoSize, MENU_ITEM, MENU_ITEM_DANGER, SELECT, SWITCH, Size,
	Status, TabItem, Tone, Variant, avatar, badge, breadcrumb, button, button_class, card,
	card_content, card_description, card_footer, card_section, card_title, checkbox_field, dialog,
	dialog_cancel, dialog_open_button, dropdown_menu, empty_state, error_text, help_text, icon,
	icon_button, icon_with, kbd, kbd_combo, link_button, link_card, logo, menu_label, menu_link,
	menu_separator, select_field, skeleton, status_badge, status_dot, switch_field, table, tabs,
	text_field, textarea_field, theme_toggle, toast, tooltip,
};
use super::extract::UiUser;
use super::{error, layout};
use crate::error::ServerError;
use crate::state::AppState;

pub fn router() -> Router<AppState> {
	Router::new().route("/_ui", get(page))
}

/// Icons the components and this page name directly; a test checks each is in the sprite.
#[cfg(test)]
pub const ICON_NAMES: &[&str] = &[
	"check",
	"chevron-right",
	"circle-check",
	"circle-x",
	"ellipsis",
	"info",
	"loader-circle",
	"monitor",
	"moon",
	"plus",
	"rocket",
	"search",
	"sun",
	"trash-2",
	"triangle-alert",
	"x",
];

fn enabled_with(debug_build: bool, env: Option<&str>) -> bool {
	debug_build || env == Some("1")
}

fn enabled() -> bool {
	enabled_with(
		cfg!(debug_assertions),
		std::env::var("PODKIT_UI_GALLERY").ok().as_deref(),
	)
}

async fn page(user: UiUser) -> Response {
	if !enabled() {
		return error::respond(&Method::GET, Some(&user), &ServerError::NotFound);
	}
	layout::document("UI gallery", &body(&user.name)).into_response()
}

/// Every icon in the sprite, from the sprite itself.
fn sprite_icons() -> Vec<&'static str> {
	ICONS_SVG
		.split("<symbol id=\"")
		.skip(1)
		.filter_map(|s| s.split('"').next())
		.collect()
}

/// One component's section: `demo` is rendered twice, once per color scheme, with a prefix
/// that keeps element ids unique.
fn section(name: &str, title: &str, note: &str, demo: impl Fn(&str) -> Markup) -> Markup {
	html! {
		section data-component=(name) class="flex flex-col gap-4 border-t border-border pt-8" {
			div class="flex flex-col gap-1" {
				h2 class="text-lg font-semibold" { (title) }
				p class="max-w-3xl text-sm text-muted-foreground" { (note) }
			}
			div class="grid gap-4 lg:grid-cols-2" {
				div class="ui-scheme-dark flex min-w-0 flex-col gap-4 rounded-xl border border-border bg-background p-6 text-foreground" data-scheme="dark" {
					p class="text-xs font-medium text-muted-foreground" { "Dark" }
					(demo(&format!("{name}-d")))
				}
				div class="ui-scheme-light flex min-w-0 flex-col gap-4 rounded-xl border border-border bg-background p-6 text-foreground" data-scheme="light" {
					p class="text-xs font-medium text-muted-foreground" { "Light" }
					(demo(&format!("{name}-l")))
				}
			}
		}
		"\n"
	}
}

fn row(content: &Markup) -> Markup {
	html! { div class="flex flex-wrap items-center gap-3" { (content) } }
}

#[allow(
	clippy::too_many_lines,
	reason = "one page that lists every component; splitting it would only hide the list"
)]
fn body(user: &str) -> Markup {
	html! {
		header class="flex flex-wrap items-center justify-between gap-3 border-b border-border bg-card px-6 py-3" {
			div class="flex items-center gap-3" {
				a href="/" class="font-semibold" { "podkit" }
				span class="text-sm text-muted-foreground" { "UI gallery" }
			}
			div class="flex items-center gap-3" {
				span class="hidden text-sm text-muted-foreground md:inline" { (user) }
				(theme_toggle())
			}
		}
		"\n"
		main class="mx-auto flex max-w-7xl flex-col gap-10 px-4 py-8 md:px-8" {
			div class="flex flex-col gap-2" {
				h1 class="text-2xl font-semibold" { "Design system" }
				p class="max-w-3xl text-sm text-muted-foreground" {
					"Every component in every state, in both color schemes. The spec is docs/design/README.md; this page is the visual check."
				}
			}
			"\n"
			(section("button", "Button", "Variants by meaning, sizes, disabled, and as a link.", button_demo))
			(section("icon-button", "Icon button", "Always has an accessible name; the icon is decoration.", |_| {
				row(&html! {
					(icon_button(Variant::Outline, "Add", "plus"))
					(icon_button(Variant::Ghost, "Search", "search"))
					(icon_button(Variant::Destructive, "Delete", "trash-2"))
					button type="button" class=(button_class(Variant::Outline, Size::Icon)) aria-label="Disabled" disabled { (icon("x")) }
				})
			}))
			(section("input", "Input", "Default, filled, disabled, with help, with a server error. The red border shows only after the user touched an invalid field.", input_demo))
			(section("textarea", "Textarea", "For pasted text such as a key.", |p| {
				html! {
					(textarea_field(&format!("{p}-key"), "key", "Private key", None, Some("Paste the key; it is never shown again."), None))
					(textarea_field(&format!("{p}-bad"), "bad", "Notes", Some("too long"), None, Some("Keep it under 10 characters.")))
				}
			}))
			(section("select", "Select", "A native select; where the browser supports it the picker is styled too.", |p| {
				html! {
					(select_field(&format!("{p}-s"), "region", "Region", &[("eu", "Europe"), ("us", "United States"), ("ap", "Asia Pacific")], Some("eu"), Some("Where the app runs."), None))
					(select_field(&format!("{p}-t"), "size", "Size", &[("s", "Small"), ("m", "Medium")], None, None, Some("Pick a size.")))
					label class=(LABEL) {
						"Bare class constant (for Datastar binding)"
						select class=(SELECT) { button { selectedcontent {} } option value="a" { "Option A" } option value="b" selected { "Option B" } }
					}
				}
			}))
			(section("checkbox", "Checkbox", "Unchecked, checked, with help, disabled.", |p| {
				html! {
					(checkbox_field(&format!("{p}-a"), "a", "Remember me", false, None))
					(checkbox_field(&format!("{p}-b"), "b", "Deploy on push", true, Some("Builds every commit to main.")))
					div class="flex items-center gap-3" {
						input id=(format!("{p}-c")) type="checkbox" class=(CHECKBOX) disabled;
						label for=(format!("{p}-c")) class="text-sm font-medium" { "Disabled" }
					}
				}
			}))
			(section("switch", "Switch", "A setting that applies at once.", |p| {
				html! {
					(switch_field(&format!("{p}-a"), "a", "Auto deploy", true, Some("Redeploys when the branch changes.")))
					(switch_field(&format!("{p}-b"), "b", "Maintenance mode", false, None))
					div class="flex items-center gap-3" {
						input id=(format!("{p}-c")) type="checkbox" role="switch" class=(SWITCH) disabled checked;
						label for=(format!("{p}-c")) class="text-sm font-medium" { "Disabled" }
					}
				}
			}))
			(section("label", "Label, help and error text", "A label wraps or points at its control; help is muted, an error is announced.", |p| {
				html! {
					label class=(LABEL) { "Wrapping label" input class=(INPUT) placeholder="Placeholder"; }
					(help_text(&format!("{p}-h"), "Help text sits under the control."))
					(error_text(&format!("{p}-e"), "Error text says what to change."))
				}
			}))
			(section("card", "Card", "Header, content, footer. A card lays itself out from its own width.", |_| {
				html! {
					(card(&html! {
						(card_section("Environment", Some("Variables are encrypted at rest."), Some(&button(Variant::Outline, Size::Sm, "button", &html! { "Add variable" }))))
						(card_content(&html! { p class="text-sm" { "3 variables set for production." } }))
						(card_footer(&html! {
							(button(Variant::Ghost, Size::Md, "button", &html! { "Cancel" }))
							(button(Variant::Default, Size::Md, "button", &html! { "Save" }))
						}))
					}))
					(card(&html! {
						div class="flex flex-col gap-1.5 px-6" {
							(card_title("Composed header"))
							(card_description("From card_title and card_description, for a header with its own layout."))
						}
						(card_content(&html! { p class="text-sm text-muted-foreground" { "Content." } }))
					}))
				}
			}))
			(section("link-card", "Logo card", "One service or resource; the whole card is the link. Two widths show the container query.", link_card_demo))
			(section("logo", "Logo", "A service mark in a tile, or a monogram when there is none. Three sizes.", |_| {
				html! {
					(row(&html! {
						@for name in ["postgres", "redis", "mysql", "mongodb", "nginx", "minio", "docker", "node", "python", "rust", "go", "github"] {
							(logo(name, LogoSize::Md))
						}
					}))
					(row(&html! {
						(logo("postgres", LogoSize::Sm)) (logo("postgres", LogoSize::Md)) (logo("postgres", LogoSize::Lg))
						(logo("my-service", LogoSize::Sm)) (logo("my-service", LogoSize::Md)) (logo("my-service", LogoSize::Lg))
					}))
				}
			}))
			(section("badge", "Badge", "A fact, in one of five tones.", |_| {
				row(&html! {
					(badge(Tone::Neutral, "Neutral"))
					(badge(Tone::Progress, "In progress"))
					(badge(Tone::Success, "Success"))
					(badge(Tone::Warning, "Warning"))
					(badge(Tone::Danger, "Danger"))
				})
			}))
			(section("status-dot", "Status dot and status badge", "State of something that runs. Always carries its word.", |_| {
				html! {
					(row(&html! { @for s in ALL_STATUSES { (status_dot(s)) } }))
					(row(&html! { @for s in ALL_STATUSES { (status_badge(s)) } }))
				}
			}))
			(section("tabs", "Tabs", "Links with aria-current, server-rendered. Scrolls sideways when narrow.", |_| {
				tabs("Application sections", &[
					TabItem { label: "Overview", href: "#overview", current: false },
					TabItem { label: "Deployments", href: "#deployments", current: true },
					TabItem { label: "Environment", href: "#environment", current: false },
					TabItem { label: "Domains", href: "#domains", current: false },
					TabItem { label: "Settings", href: "#settings", current: false },
				])
			}))
			(section("table", "Table", "Sticky header, row header, keyboard-scrollable. Stacks below 36rem of its own width.", table_demo))
			(section("dialog", "Dialog", "A native modal. Esc closes it; focus is trapped and returned.", dialog_demo))
			(section("dropdown-menu", "Dropdown menu", "A popover on an invoker button; arrow keys, Esc and outside click close it.", menu_demo))
			(section("tooltip", "Tooltip", "On hover and keyboard focus; Esc hides it.", |p| {
				let tip = format!("{p}-tip");
				row(&html! {
					(tooltip(&tip, "Redeploy the latest commit", &html! {
						button type="button" class=(button_class(Variant::Outline, Size::Icon)) aria-label="Redeploy" aria-describedby=(tip) { (icon("rocket")) }
					}))
					(tooltip(&format!("{p}-tip2"), "Needs the owner role", &html! {
						button type="button" class=(button_class(Variant::Outline, Size::Md)) aria-describedby=(format!("{p}-tip2")) { "Delete team" }
					}))
				})
			}))
			(section("toast", "Toast", "Appended to the live region at the bottom right. Each tone shown here in place.", |_| {
				html! {
					@for (tone, title, description) in [
						(Tone::Success, "Deployment started", Some("api is building.")),
						(Tone::Progress, "Restarting", None),
						(Tone::Warning, "Disk almost full", Some("92% of 100 GB used.")),
						(Tone::Danger, "Build failed", Some("Exit code 1 in the install step.")),
						(Tone::Neutral, "Copied", None),
					] {
						(toast(tone, title, description))
					}
				}
			}))
			(section("skeleton", "Skeleton", "The shape of content that is loading.", |_| {
				html! {
					div class="flex items-center gap-3" aria-busy="true" {
						(skeleton("h-10 w-10 rounded-full"))
						div class="flex flex-col gap-2" { (skeleton("h-4 w-40")) (skeleton("h-3 w-24")) }
					}
					(skeleton("h-24 w-full"))
				}
			}))
			(section("empty-state", "Empty state", "What a list shows when it has nothing, and the one next step.", |_| {
				empty_state("rocket", "No applications yet", Some("Deploy a repository or a template to see it here."), Some(&link_button(Variant::Default, Size::Md, "#new", &html! { "Create application" })))
			}))
			(section("breadcrumb", "Breadcrumb", "Where this page sits. The last crumb is the current page.", |_| {
				breadcrumb(&[
					("Projects", Some("#p".to_owned())),
					("billing", Some("#b".to_owned())),
					("Settings", None),
				])
			}))
			(section("avatar", "Avatar", "Initials, named for assistive technology.", |_| {
				row(&html! { (avatar("Ada Lovelace")) (avatar("grace")) (avatar("Linus Torvalds")) (avatar("   ")) })
			}))
			(section("kbd", "Kbd", "A key or a shortcut, as a hint.", |_| {
				row(&html! { (kbd("Esc")) (kbd_combo(&["Ctrl", "K"])) (kbd_combo(&["Shift", "Enter"])) })
			}))
			(section("icon", "Icon", "The vendored Lucide subset, from /static/icons.svg.", |_| html! {
				div class="grid grid-cols-3 gap-3 sm:grid-cols-4 md:grid-cols-6" {
					@for name in sprite_icons() {
						div class="flex flex-col items-center gap-1.5 rounded-md border border-border p-2 text-center" {
							(icon_with(name, "h-5 w-5"))
							span class="w-full truncate text-xs text-muted-foreground" { (name) }
						}
					}
				}
			}))
			(section("theme-toggle", "Theme toggle", "System, light or dark; stored in a cookie and rendered on <html> by the server.", |_| {
				theme_toggle()
			}))
			(section("loading-error", "Loading and error patterns", "A busy button, an inline error and a banner.", |_| {
				html! {
					(row(&html! {
						button type="button" class=(BUTTON) disabled { (icon_with("loader-circle", "ui-icon-spin")) "Deploying" }
						button type="button" class=(button_class(Variant::Outline, Size::Md)) { "Retry" }
					}))
					div class="flex items-start gap-3 rounded-lg border border-destructive/30 bg-destructive/10 p-4 text-sm" role="alert" {
						(icon_with("circle-x", "mt-0.5 text-destructive"))
						div class="flex flex-col gap-1" {
							p class="font-medium" { "Could not reach the server" }
							p class="text-muted-foreground" { "podkit could not open an SSH connection to 10.0.0.5. Check the address and try again." }
						}
					}
				}
			}))
		}
	}
}

const ALL_STATUSES: [Status; 5] = [
	Status::Running,
	Status::Building,
	Status::Failed,
	Status::Stopped,
	Status::Pending,
];

fn button_demo(_: &str) -> Markup {
	html! {
		(row(&html! {
			(button(Variant::Default, Size::Md, "button", &html! { "Default" }))
			(button(Variant::Secondary, Size::Md, "button", &html! { "Secondary" }))
			(button(Variant::Outline, Size::Md, "button", &html! { "Outline" }))
			(button(Variant::Ghost, Size::Md, "button", &html! { "Ghost" }))
			(button(Variant::Destructive, Size::Md, "button", &html! { "Destructive" }))
			(button(Variant::Link, Size::Md, "button", &html! { "Link" }))
		}))
		(row(&html! {
			(button(Variant::Default, Size::Sm, "button", &html! { "Small" }))
			(button(Variant::Default, Size::Md, "button", &html! { "Medium" }))
			(button(Variant::Default, Size::Lg, "button", &html! { "Large" }))
			button type="button" class=(button_class(Variant::Default, Size::Md)) { (icon("plus")) "With icon" }
			button type="button" class=(button_class(Variant::Default, Size::Md)) disabled { "Disabled" }
			(link_button(Variant::Outline, Size::Md, "#link", &html! { "As link" }))
		}))
	}
}

fn input_demo(p: &str) -> Markup {
	html! {
		(text_field(&format!("{p}-name"), "name", "Name", "text", None, Some("Lowercase letters and dashes."), None))
		(text_field(&format!("{p}-email"), "email", "Email", "email", Some("ada@example.com"), None, None))
		(text_field(&format!("{p}-pw"), "pw", "Password", "password", None, None, Some("Use at least 12 characters.")))
		div class="flex flex-col gap-2" {
			label class=(LABEL) { "Required, try typing then clearing" input class=(INPUT) required; }
			label class=(LABEL) { "Disabled" input class=(INPUT) value="read only" disabled; }
		}
	}
}

fn link_card_demo(_: &str) -> Markup {
	let meta = html! { (status_dot(Status::Running)) (badge(Tone::Neutral, "Postgres 18")) };
	html! {
		div class="ui-card-grid" {
			(link_card("#a", &logo("postgres", LogoSize::Md), "billing-db", Some("Managed Postgres with daily backups."), Some(&meta)))
			(link_card("#b", &logo("redis", LogoSize::Md), "cache", Some("Redis 8, 256 MB."), Some(&html! { (status_dot(Status::Stopped)) })))
			(link_card("#c", &logo("custom-app", LogoSize::Md), "custom-app", None, Some(&html! { (status_dot(Status::Building)) })))
		}
		div class="max-w-56" {
			(link_card("#d", &logo("nginx", LogoSize::Md), "edge-proxy", Some("Narrow: stacked."), None))
		}
	}
}

fn table_demo(p: &str) -> Markup {
	table(
		&format!("{p}-t"),
		"Applications",
		&["Name", "Status", "Branch", "Updated"],
		&[
			vec![
				html! { "api" },
				status_dot(Status::Running),
				html! { span class="font-mono text-xs" { "main" } },
				html! { "2 min ago" },
			],
			vec![
				html! { "web" },
				status_dot(Status::Building),
				html! { span class="font-mono text-xs" { "feature/pricing" } },
				html! { "just now" },
			],
			vec![
				html! { "worker" },
				status_dot(Status::Failed),
				html! { span class="font-mono text-xs" { "main" } },
				html! { "1 h ago" },
			],
		],
	)
}

fn dialog_demo(p: &str) -> Markup {
	let confirm = format!("{p}-confirm");
	let form = format!("{p}-form");
	html! {
		(row(&html! {
			(dialog_open_button(&confirm, &button_class(Variant::Destructive, Size::Md), &html! { "Delete application" }))
			(dialog_open_button(&form, &button_class(Variant::Outline, Size::Md), &html! { "Rename" }))
		}))
		(dialog(
			&confirm,
			"Delete api?",
			Some("This removes its deployments and environment variables. It cannot be undone."),
			Dismiss::Esc,
			&html! { p { "Type the name to confirm." } input class=(INPUT) aria-label="Application name" placeholder="api"; },
			&html! {
				form method="dialog" { button class=(button_class(Variant::Outline, Size::Md)) autofocus { "Keep it" } }
				form method="dialog" { button class=(button_class(Variant::Destructive, Size::Md)) { "Delete" } }
			},
		))
		(dialog(
			&form,
			"Rename application",
			None,
			Dismiss::EscOrOutside,
			&html! { label class=(LABEL) { "Name" input class=(INPUT) value="api" autofocus; } },
			&html! {
				(dialog_cancel(&button_class(Variant::Outline, Size::Md), &html! { "Cancel" }))
				form method="dialog" { button class=(BUTTON) { "Save" } }
			},
		))
	}
}

fn menu_demo(p: &str) -> Markup {
	let items = html! {
		(menu_label("api"))
		(menu_link("#open", &html! { (icon("rocket")) "Open" }))
		button type="button" class=(MENU_ITEM) { (icon("plus")) "Redeploy" }
		button type="button" class=(MENU_ITEM) disabled { "Disabled" }
		(menu_separator())
		button type="button" class=(MENU_ITEM_DANGER) { (icon("trash-2")) "Delete" }
	};
	row(&html! {
		(dropdown_menu(
			&format!("{p}-menu"),
			&button_class(Variant::Outline, Size::Icon),
			"Actions for api",
			&icon("ellipsis"),
			&items,
		))
		(dropdown_menu(
			&format!("{p}-menu2"),
			&button_class(Variant::Outline, Size::Md),
			"Team menu",
			&html! { "Team" (icon("chevron-right")) },
			&items,
		))
	})
}

#[cfg(test)]
mod tests {
	use axum::http::StatusCode;

	use super::*;
	use crate::tests::{PgPool, app, db_test};
	use crate::ui::testing::{assert_contains, assert_not_contains, get_html, send, ui_session};

	#[test]
	fn the_gallery_needs_a_debug_build_or_the_env_switch() {
		assert!(enabled_with(true, None));
		assert!(enabled_with(false, Some("1")));
		assert!(!enabled_with(false, None));
		assert!(!enabled_with(false, Some("0")));
		assert!(!enabled_with(false, Some("true")));
	}

	#[test]
	fn the_sprite_lists_its_icons_and_the_named_ones_exist() {
		let icons = sprite_icons();
		assert!(icons.len() >= 80, "{}", icons.len());
		for name in ICON_NAMES {
			assert!(icons.contains(name), "{name}");
		}
	}

	#[db_test]
	async fn it_needs_a_session_and_lists_every_component_on_its_own_line(db: PgPool) {
		let app = app(db).await;
		let (cookie, _) = ui_session(&app, "alice").await;

		let (s, h, _) = get_html(&app, "/_ui", None).await;
		assert_eq!(s, StatusCode::SEE_OTHER);
		assert!(h["location"].to_str().unwrap().starts_with("/login"));

		let (s, _, page) = get_html(&app, "/_ui", Some(&cookie)).await;
		assert_eq!(s, StatusCode::OK);
		let lines = page
			.lines()
			.filter(|l| l.contains("data-component="))
			.count();
		assert!(lines >= 25, "{lines} component lines");
		for name in [
			"button",
			"dialog",
			"dropdown-menu",
			"tooltip",
			"logo",
			"table",
		] {
			assert_contains(&page, &format!("data-component=\"{name}\""));
		}
		assert_contains(&page, "data-scheme=\"dark\"");
		assert_contains(&page, "data-scheme=\"light\"");
	}

	#[db_test]
	async fn the_pages_ids_are_unique(db: PgPool) {
		let app = app(db).await;
		let (cookie, _) = ui_session(&app, "alice").await;
		let (_, _, page) = get_html(&app, "/_ui", Some(&cookie)).await;
		let mut ids: Vec<&str> = page
			.split(" id=\"")
			.skip(1)
			.filter_map(|s| s.split('"').next())
			.collect();
		let total = ids.len();
		ids.sort_unstable();
		ids.dedup();
		assert_eq!(ids.len(), total, "duplicate element ids on /_ui");
	}

	#[db_test]
	async fn the_theme_cookie_sets_the_class_on_html_and_the_toggle_sets_the_cookie(db: PgPool) {
		let app = app(db).await;
		let (cookie, _) = ui_session(&app, "alice").await;

		let (_, _, page) = get_html(&app, "/_ui", Some(&cookie)).await;
		assert_contains(&page, "<html lang=\"en\">");
		assert_contains(&page, "content=\"dark light\"");

		for (theme, class) in [("dark", "dark"), ("light", "light")] {
			let with_theme = format!("{cookie}; theme={theme}");
			let (_, _, page) = get_html(&app, "/_ui", Some(&with_theme)).await;
			assert_contains(&page, &format!("<html lang=\"en\" class=\"{class}\">"));
			assert_contains(&page, &format!("content=\"{class}\""));
			assert_contains(&page, "aria-pressed=\"true\"");
		}
		let (_, _, page) = get_html(&app, "/_ui", Some(&format!("{cookie}; theme=pink"))).await;
		assert_contains(&page, "<html lang=\"en\">");

		let origin = format!("http://{}", crate::ui::testing::HOST);
		let (s, h, _) = send(
			&app,
			"POST",
			"/theme",
			None,
			Some("theme=dark"),
			Some(&origin),
		)
		.await;
		assert_eq!(s, StatusCode::SEE_OTHER);
		assert_eq!(h["location"], "/");
		let set_cookie = h["set-cookie"].to_str().unwrap();
		assert!(set_cookie.starts_with("theme=dark"), "{set_cookie}");
		assert!(set_cookie.contains("Max-Age"), "{set_cookie}");

		let (s, _, _) = send(
			&app,
			"POST",
			"/theme",
			None,
			Some("theme=dark"),
			Some("http://evil.example"),
		)
		.await;
		assert_eq!(
			s,
			StatusCode::FORBIDDEN,
			"the origin guard covers the toggle"
		);
	}

	#[db_test]
	async fn every_page_gets_the_class_and_loads_only_same_origin_assets(db: PgPool) {
		let app = app(db).await;
		let (_, _, login) = get_html(&app, "/login", Some("theme=dark")).await;
		assert_contains(&login, "<html lang=\"en\" class=\"dark\">");
		for external in ["https://", "//cdn", "fonts.googleapis", "unpkg"] {
			assert_not_contains(&login, external);
		}
		assert_contains(&login, "rel=\"preload\"");
		assert_contains(&login, "/static/fonts/Geist-Variable.woff2");
		assert_contains(&login, "id=\"toasts\"");
	}

	#[db_test]
	async fn the_static_assets_are_served_immutable(db: PgPool) {
		let app = app(db).await;
		for (path, kind) in [
			("/static/icons.svg", "image/svg+xml"),
			("/static/logos.svg", "image/svg+xml"),
			("/static/podkit.js", "text/javascript"),
			("/static/fonts/Geist-Variable.woff2", "font/woff2"),
			("/static/fonts/GeistMono-Variable.woff2", "font/woff2"),
		] {
			let resp = tower::ServiceExt::oneshot(
				app.clone(),
				axum::http::Request::get(path)
					.body(axum::body::Body::empty())
					.unwrap(),
			)
			.await
			.unwrap();
			let (s, h) = (resp.status(), resp.headers().clone());
			assert_eq!(s, StatusCode::OK, "{path}");
			assert!(
				h["content-type"].to_str().unwrap().starts_with(kind),
				"{path}"
			);
			assert!(
				h["cache-control"].to_str().unwrap().contains("immutable"),
				"{path}"
			);
		}
	}
}
