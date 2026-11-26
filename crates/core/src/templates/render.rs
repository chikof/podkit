//! Filling a template in: variables generated or asked for, then the
//! environment, domains and files that depend on them.

use std::collections::{BTreeMap, BTreeSet};

use super::expr::{self, Context, Expr, Random};
use super::{Invalid, Template, Variable, mount_key};
use crate::stacks::compose::slug;

/// Where the template is going, for default hostnames.
#[derive(Clone, Copy)]
pub struct Placement<'a> {
	/// Name the thing is deployed under, as a slug.
	pub slug: &'a str,
	/// The server's address with dots as dashes, `203-0-113-7`.
	pub ip: &'a str,
	/// Hostnames are `<slug>.<ip>.sslip.io`, an application's own address,
	/// instead of `<service>-<slug>.<ip>.sslip.io`.
	pub flat: bool,
}

/// `<service>-<slug>.<ip>.sslip.io`.
#[must_use]
pub fn default_host(service: &str, place: &Placement<'_>) -> String {
	if place.flat {
		return format!("{}.{}.sslip.io", place.slug, place.ip);
	}
	let service = slug(service);
	let mut label = format!("{service}-{}", place.slug);
	label.truncate(63);
	format!("{}.{}.sslip.io", label.trim_end_matches('-'), place.ip)
}

/// A service made reachable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedDomain {
	/// Compose service.
	pub service: String,
	/// Container port.
	pub port: u16,
	/// Hostname.
	pub host: String,
}

/// A template with every variable resolved.
#[derive(Debug, Clone)]
pub struct Rendered {
	/// The compose file's interpolation environment.
	pub env: BTreeMap<String, String>,
	/// Routed services.
	pub domains: Vec<RenderedDomain>,
	/// Mounted files by path relative to the compose file.
	pub files: BTreeMap<String, String>,
	/// Variables that are not credentials, with their values: the part of a
	/// deploy the user may be told.
	pub values: BTreeMap<String, String>,
	/// Names of the variables that are credentials. Their values live in
	/// `env` and `files` only.
	#[allow(
		dead_code,
		reason = "tests and the UI card check which variables are credentials"
	)]
	pub secrets: BTreeSet<String>,
	/// The values of the credentials the template says the user needs to log
	/// in with ([`Template::reveal`]): for the deploy response, once.
	pub reveal: BTreeMap<String, String>,
}

struct State<'a> {
	template: &'a Template,
	inputs: &'a BTreeMap<String, String>,
	rng: &'a mut dyn Random,
	values: BTreeMap<String, String>,
	secrets: BTreeSet<String>,
	visiting: Vec<String>,
	/// Hostname each `${domain}` variable stands for.
	hosts: BTreeMap<String, String>,
	/// What a bare `${domain}` outside a variable stands for.
	main_host: String,
}

fn is_domain_expr(text: &str) -> bool {
	text.trim() == "${domain}"
}

/// The service each `${domain}` variable routes to: the domain mappings
/// whose host is exactly that variable, then the unused ones in order.
fn domain_hosts(t: &Template, place: &Placement<'_>) -> (BTreeMap<String, String>, String) {
	let mut used: BTreeSet<String> = BTreeSet::new();
	let mut taken_services: BTreeSet<usize> = BTreeSet::new();
	let mut hosts = BTreeMap::new();
	let domain_vars: Vec<&String> = t
		.variables
		.iter()
		.filter_map(|(name, v)| match v {
			Variable::Expr(text) if is_domain_expr(text) => Some(name),
			_ => None,
		})
		.collect();
	let mut claim = |hosts: &mut BTreeMap<String, String>, name: &String, service: &str| {
		let mut host = default_host(service, place);
		let mut n = 2;
		while used.contains(&host) {
			host = if place.flat {
				let slug = format!("{}-{n}", place.slug);
				default_host(
					service,
					&Placement {
						slug: &slug,
						..*place
					},
				)
			} else {
				default_host(&format!("{service}-{n}"), place)
			};
			n += 1;
			// A name cut to a label's 63 characters stops changing with the number.
			if n > 99 {
				break;
			}
		}
		used.insert(host.clone());
		hosts.insert(name.clone(), host);
	};
	let mut unclaimed = Vec::new();
	for name in &domain_vars {
		let reference = format!("${{{name}}}");
		let found = t.config.domains.iter().enumerate().find(|(i, d)| {
			!taken_services.contains(i)
				&& d.host.as_deref().map(str::trim) == Some(reference.as_str())
		});
		match found {
			Some((i, d)) => {
				taken_services.insert(i);
				claim(&mut hosts, name, &d.service_name);
			}
			None => unclaimed.push(*name),
		}
	}
	for name in unclaimed {
		let spare = t
			.config
			.domains
			.iter()
			.enumerate()
			.find(|(i, _)| !taken_services.contains(i));
		match spare {
			Some((i, d)) => {
				taken_services.insert(i);
				claim(&mut hosts, name, &d.service_name);
			}
			None => claim(&mut hosts, name, &t.id),
		}
	}
	let main = t
		.config
		.domains
		.first()
		.map_or(t.id.as_str(), |d| d.service_name.as_str());
	let main = default_host(main, place);
	(hosts, main)
}

/// How long a chain of variables, each made from the next, may be. The catalog's longest is 2.
const MAX_DEPTH: usize = 64;

impl State<'_> {
	fn eval(&mut self, name: &str) -> Result<(), Invalid> {
		if self.values.contains_key(name) {
			return Ok(());
		}
		let Some(variable) = self.template.variables.get(name) else {
			return Ok(());
		};
		if self.visiting.iter().any(|v| v == name) {
			return Err(Invalid(format!("variable {name} refers to itself")));
		}
		// One call a link: a chain of thousands runs the thread out of stack, which ends
		// the process and not the request.
		if self.visiting.len() >= MAX_DEPTH {
			return Err(Invalid(format!(
				"variables refer to each other more than {MAX_DEPTH} deep, at {name}"
			)));
		}
		self.visiting.push(name.to_string());
		let (value, secret) = match variable {
			Variable::Input(input) => {
				let given = self
					.inputs
					.get(name)
					.map(|v| v.trim())
					.filter(|v| !v.is_empty());
				let value = given
					.map(str::to_string)
					.or_else(|| input.default.clone())
					.ok_or_else(|| Invalid(format!("{name} is required")))?;
				(value, input.secret)
			}
			Variable::Expr(text) => {
				if let (true, Some(own)) = (is_domain_expr(text), self.inputs.get(name)) {
					(own.trim().to_string(), false)
				} else {
					for reference in expr::references(text) {
						self.eval(&reference)?;
					}
					let secret = expr::scan(text).map_err(Invalid)?.iter().any(|e| match e {
						Expr::Helper(h) => h.is_secret(),
						Expr::Name(n) => self.secrets.contains(n),
					});
					let domain = self
						.hosts
						.get(name)
						.cloned()
						.unwrap_or_else(|| self.main_host.clone());
					let mut ctx = Context {
						rng: &mut *self.rng,
						variables: &self.values,
						domain: &domain,
					};
					(expr::expand(text, &mut ctx).map_err(Invalid)?, secret)
				}
			}
		};
		self.visiting.pop();
		if secret {
			self.secrets.insert(name.to_string());
		}
		self.values.insert(name.to_string(), value);
		Ok(())
	}

	fn expand(&mut self, text: &str) -> Result<String, Invalid> {
		let mut ctx = Context {
			rng: &mut *self.rng,
			variables: &self.values,
			domain: &self.main_host,
		};
		expr::expand(text, &mut ctx).map_err(Invalid)
	}
}

/// Resolves every variable of `t` and fills in its environment, domains and
/// files.
///
/// `inputs` may answer a `string` variable, or replace the hostname of a
/// `${domain}` variable; nothing else.
///
/// # Errors
/// An input that names no variable, a missing answer, a hostname that is not
/// one, a reference cycle.
pub fn render(
	t: &Template,
	inputs: &BTreeMap<String, String>,
	place: &Placement<'_>,
	rng: &mut dyn Random,
) -> Result<Rendered, Invalid> {
	for (name, value) in inputs {
		let askable = match t.variables.get(name) {
			Some(Variable::Input(_)) => true,
			Some(Variable::Expr(text)) => is_domain_expr(text),
			None => false,
		};
		if !askable {
			return Err(Invalid(format!("{name} is not a variable you can set")));
		}
		if value.len() > 4096 {
			return Err(Invalid(format!("{name} is too long")));
		}
		if matches!(t.variables.get(name), Some(Variable::Expr(_)))
			&& !crate::validate::hostname(&value.trim().to_ascii_lowercase())
		{
			return Err(Invalid(format!("{name} must be a hostname")));
		}
	}

	let (hosts, main_host) = domain_hosts(t, place);
	let mut state = State {
		template: t,
		inputs,
		rng,
		values: BTreeMap::new(),
		secrets: BTreeSet::new(),
		visiting: Vec::new(),
		hosts,
		main_host,
	};
	for name in t.variables.keys() {
		state.eval(name)?;
	}

	let mut env = BTreeMap::new();
	for (key, text) in &t.config.env {
		env.insert(key.clone(), state.expand(text)?);
	}
	let mut domains = Vec::new();
	for d in &t.config.domains {
		let host = match &d.host {
			Some(text) if !text.trim().is_empty() => state.expand(text)?,
			_ => default_host(&d.service_name, place),
		};
		domains.push(RenderedDomain {
			service: d.service_name.clone(),
			port: d.port,
			host: host.trim().to_ascii_lowercase(),
		});
	}
	let mut files = BTreeMap::new();
	for m in &t.config.mounts {
		let key = mount_key(&m.file_path)
			.ok_or_else(|| Invalid(format!("mount path {:?} is not valid", m.file_path)))?;
		files.insert(key, state.expand(&m.content)?);
	}
	let secrets = state.secrets;
	let reveal = t
		.reveal
		.iter()
		.filter_map(|name| Some((name.clone(), state.values.get(name)?.clone())))
		.collect();
	let values = state
		.values
		.into_iter()
		.filter(|(name, _)| !secrets.contains(name))
		.collect();
	Ok(Rendered {
		env,
		domains,
		files,
		values,
		secrets,
		reveal,
	})
}
