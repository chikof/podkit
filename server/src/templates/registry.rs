//! The templates compiled into the binary, parsed and checked once.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::sync::OnceLock;

use proto::{ContainerOptions, Ingress};
use sha2::{Digest, Sha256};

use super::expr::Counter;
use super::render::{Placement, render};
use super::{Invalid, MAX_LOGO_BYTES, Template, embed, logo_type};
use crate::stacks::compose::{self, Inputs};
use crate::stacks::translate::{self, Domain, Settings};

/// A service of a template, as the gallery shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceInfo {
	/// Compose service name.
	pub name: String,
	/// Image reference.
	pub image: String,
	/// Container port of its domain, when it has one.
	pub domain_port: Option<u16>,
	/// Whether it keeps data in named volumes.
	pub volumes: bool,
	/// Services it waits for.
	pub depends_on: Vec<String>,
}

/// A template's logo.
#[derive(Debug, Clone)]
pub struct Logo {
	/// Media type.
	pub media: &'static str,
	/// File content.
	pub bytes: Cow<'static, [u8]>,
	/// First 8 hex digits of its SHA-256, the cache-busting part of its URL.
	pub hash: String,
}

/// A template that parsed, validated and translated.
#[derive(Debug, Clone)]
pub struct Loaded {
	/// `template.toml`.
	pub template: Template,
	/// `docker-compose.yml`, verbatim.
	pub compose: Cow<'static, str>,
	/// The logo.
	pub logo: Logo,
	/// What it runs.
	pub services: Vec<ServiceInfo>,
	/// Keys the translation ignores or approximates.
	pub warnings: Vec<String>,
	/// One service with one domain, an image, environment and named volumes,
	/// and nothing else an application cannot express: it is deployed as an
	/// image application instead of a one-service stack.
	pub plain_app: bool,
}

/// First 8 hex digits of the SHA-256 of `bytes`.
#[must_use]
pub fn short_hash(bytes: &[u8]) -> String {
	use std::fmt::Write;
	Sha256::digest(bytes)
		.iter()
		.take(4)
		.fold(String::new(), |mut s, b| {
			let _ = write!(s, "{b:02x}");
			s
		})
}

fn describe(issues: &[compose::Issue]) -> String {
	issues
		.iter()
		.map(|i| match &i.service {
			Some(s) => format!("{s}: {}: {}", i.path, i.message),
			None => format!("{}: {}", i.path, i.message),
		})
		.collect::<Vec<_>>()
		.join("; ")
}

/// Parses and checks one template given its files by name: that it parses,
/// that its logo is usable, and that filled in with placeholder values its
/// compose file translates.
///
/// # Errors
/// The first thing wrong, as text.
pub fn load(
	dir: &str,
	file: &dyn Fn(&str) -> Option<Cow<'static, [u8]>>,
) -> Result<Loaded, Invalid> {
	let text = file("template.toml").ok_or_else(|| Invalid("no template.toml".to_string()))?;
	let text =
		std::str::from_utf8(&text).map_err(|_| Invalid("template.toml is not UTF-8".into()))?;
	let template = Template::parse(text)?;
	if template.id != dir {
		return Err(Invalid(format!(
			"id {:?} does not match the folder {dir:?}",
			template.id
		)));
	}
	let compose_text = match file("docker-compose.yml")
		.ok_or_else(|| Invalid("no docker-compose.yml".to_string()))?
	{
		Cow::Borrowed(b) => Cow::Borrowed(
			std::str::from_utf8(b)
				.map_err(|_| Invalid("docker-compose.yml is not UTF-8".into()))?,
		),
		Cow::Owned(v) => Cow::Owned(
			String::from_utf8(v).map_err(|_| Invalid("docker-compose.yml is not UTF-8".into()))?,
		),
	};

	let registry_logo = template.logo.starts_with('@');
	let media = if registry_logo {
		"image/svg+xml"
	} else {
		logo_type(&template.logo).ok_or_else(|| {
			Invalid(format!(
				"logo {:?} must be an .svg or a .png",
				template.logo
			))
		})?
	};
	let bytes = file(&template.logo)
		.ok_or_else(|| Invalid(format!("logo file {:?} is missing", template.logo)))?;
	if bytes.is_empty() || bytes.len() > MAX_LOGO_BYTES {
		return Err(Invalid(format!(
			"logo {:?} must be 1 byte to {} KiB",
			template.logo,
			MAX_LOGO_BYTES / 1024
		)));
	}
	if media == "image/svg+xml" && !is_safe_svg(&bytes) {
		return Err(Invalid(format!(
			"logo {:?} is not a plain SVG",
			template.logo
		)));
	}
	let logo = Logo {
		media,
		hash: short_hash(&bytes),
		bytes,
	};

	let (services, warnings, plain_app) = translate_check(&template, &compose_text)?;
	Ok(Loaded {
		template,
		compose: compose_text,
		logo,
		services,
		warnings,
		plain_app,
	})
}

/// An SVG that is served from our own origin must not run anything.
fn is_safe_svg(bytes: &[u8]) -> bool {
	let Ok(text) = std::str::from_utf8(bytes) else {
		return false;
	};
	let lower = text.to_ascii_lowercase();
	lower.contains("<svg")
		&& !lower.contains("<script")
		&& !lower.contains("javascript:")
		&& !lower.contains("<foreignobject")
		&& !lower.contains(" onload")
		&& !lower.contains(" onerror")
		&& !lower.contains(" onclick")
}

fn placeholder_ingress() -> Ingress {
	Ingress {
		http_port: 80,
		https_port: 443,
		acme_email: None,
	}
}

/// Fills the template in with placeholder values and runs the compose
/// parser and the translation on the result.
fn translate_check(
	template: &Template,
	compose_text: &str,
) -> Result<(Vec<ServiceInfo>, Vec<String>, bool), Invalid> {
	let mut inputs = BTreeMap::new();
	for (name, var) in &template.variables {
		if let super::Variable::Input(input) = var
			&& input.default.is_none()
		{
			inputs.insert(name.clone(), "placeholder".to_string());
		}
	}
	let place = Placement {
		slug: &template.id,
		ip: "127-0-0-1",
		flat: false,
	};
	let rendered = render(template, &inputs, &place, &mut Counter(1))?;
	for d in &rendered.domains {
		if !crate::validate::hostname(&d.host) {
			return Err(Invalid(format!(
				"domain of {} renders as {:?}, which is not a hostname",
				d.service, d.host
			)));
		}
	}
	let parsed = compose::parse(
		compose_text,
		&Inputs {
			env: &rendered.env,
			files: &rendered.files,
		},
	)
	.map_err(|issues| Invalid(format!("compose: {}", describe(&issues))))?;

	let mut domains = BTreeMap::new();
	for d in &rendered.domains {
		if !parsed.services.contains_key(&d.service) {
			return Err(Invalid(format!(
				"domain names {:?}, which is not a service",
				d.service
			)));
		}
		domains.insert(
			d.service.clone(),
			Domain {
				host: d.host.clone(),
				port: Some(d.port),
			},
		);
	}
	let plan = translate::plan(
		&parsed,
		&Settings {
			stack: 1,
			ingress: placeholder_ingress(),
			domains: &domains,
		},
	)
	.map_err(|issues| Invalid(format!("translate: {}", describe(&issues))))?;

	let services = parsed
		.services
		.values()
		.map(|s| ServiceInfo {
			name: s.name.clone(),
			image: s.image.clone(),
			domain_port: domains.get(&s.name).and_then(|d| d.port),
			volumes: s
				.mounts
				.iter()
				.any(|m| matches!(m, compose::Mount::Volume { .. })),
			depends_on: s.depends_on.keys().cloned().collect(),
		})
		.collect::<Vec<_>>();
	let warnings = parsed
		.warnings
		.iter()
		.map(|w| match &w.service {
			Some(s) => format!("{s}: {}: {}", w.path, w.message),
			None => format!("{}: {}", w.path, w.message),
		})
		.collect();
	let plain_app = plan.services.len() == 1 && {
		let spec = &plan.services[0].spec;
		let mut options = spec.options.clone();
		options.isolated = false;
		options.net_aliases.clear();
		options.restart = ContainerOptions::default().restart;
		spec.command.is_none()
			&& spec.gpus.is_none()
			&& spec.published_ports.is_empty()
			&& spec.routing.is_some()
			&& spec.container_port.is_some()
			&& spec.memory_bytes.is_none()
			&& spec.cpu_cores.is_none()
			&& plan.services[0].depends_on.is_empty()
			&& options == ContainerOptions::default()
			&& rendered.files.is_empty()
			&& spec.env.iter().all(|(k, _)| valid_env_key(k))
			&& spec.volumes.iter().all(|v| v.path.starts_with('/'))
	};
	Ok((services, warnings, plain_app))
}

/// Names an application's env var may have.
fn valid_env_key(key: &str) -> bool {
	key.chars()
		.next()
		.is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
		&& key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// What loading the embedded templates produced.
pub struct Registry {
	/// Templates that loaded, by id.
	pub templates: BTreeMap<String, Loaded>,
	/// Folders that did not, with why. Empty in a healthy build; a test
	/// fails otherwise.
	#[allow(dead_code, reason = "read by the tests that keep the gallery healthy")]
	pub errors: Vec<(String, String)>,
}

static REGISTRY: OnceLock<Registry> = OnceLock::new();

/// Loads (once) every embedded template.
pub fn load_all() -> &'static Registry {
	REGISTRY.get_or_init(|| {
		let mut templates = BTreeMap::new();
		let mut errors = Vec::new();
		for (id, files) in embed::TEMPLATES {
			let file = |name: &str| {
				if let Some(logo) = name.strip_prefix('@') {
					let want = format!("{logo}.svg");
					return embed::LOGOS
						.iter()
						.find(|(n, _)| *n == want)
						.map(|(_, bytes)| Cow::Borrowed(*bytes));
				}
				files
					.iter()
					.find(|(n, _)| *n == name)
					.map(|(_, bytes)| Cow::Borrowed(*bytes))
			};
			match load(id, &file) {
				Ok(loaded) => {
					templates.insert((*id).to_string(), loaded);
				}
				Err(e) => errors.push(((*id).to_string(), e.0)),
			}
		}
		Registry { templates, errors }
	})
}

/// One template by id.
#[must_use]
pub fn find(id: &str) -> Option<&'static Loaded> {
	load_all().templates.get(id)
}

/// Templates matching `q` (every word in the name, id, description, tags or
/// category) and, when given, in `category`, best matches first, then by name.
#[must_use]
pub fn search(q: Option<&str>, category: Option<&str>) -> Vec<&'static Loaded> {
	let words: Vec<String> = q
		.unwrap_or_default()
		.split_whitespace()
		.map(str::to_lowercase)
		.collect();
	let mut hits: Vec<(u8, &Loaded)> = load_all()
		.templates
		.values()
		.filter(|l| category.is_none_or(|c| l.template.category == c))
		.filter_map(|l| {
			let t = &l.template;
			let name = t.name.to_lowercase();
			let id = t.id.to_lowercase();
			let rest = format!(
				"{} {} {}",
				t.description.to_lowercase(),
				t.tags.join(" ").to_lowercase(),
				t.category
			);
			let mut score = 0u8;
			for w in &words {
				score = score.max(if name == *w || id == *w {
					0
				} else if name.starts_with(w.as_str()) || id.starts_with(w.as_str()) {
					1
				} else if name.contains(w.as_str()) || id.contains(w.as_str()) {
					2
				} else if rest.contains(w.as_str()) {
					3
				} else {
					return None;
				});
			}
			Some((score, l))
		})
		.collect();
	hits.sort_by(|a, b| {
		a.0.cmp(&b.0).then_with(|| {
			a.1.template
				.name
				.to_lowercase()
				.cmp(&b.1.template.name.to_lowercase())
		})
	});
	hits.into_iter().map(|(_, l)| l).collect()
}
