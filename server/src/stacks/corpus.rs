//! Offline tests of the translation: a snapshot of a small stack, and real
//! Dokploy templates that `docs/spikes/compose-coverage.json` marks fully
//! supported.
//!
//! `fixtures/dokploy/<id>/` holds a docker-compose.yml and template.toml
//! copied from <https://github.com/Dokploy/templates> (Apache-2.0), enough of
//! them to exercise every key the subset supports. The ignored test
//! [`every_fully_supported_template_translates`] runs the whole corpus when
//! `PODKIT_COMPOSE_CORPUS` points at a checkout of it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use proto::{DeploySpec, Ingress};
use serde_json::{Value, json};

use super::compose::{self, Inputs};
use super::translate::{self, Domain, Settings};

fn ingress() -> Ingress {
	Ingress {
		http_port: 8088,
		https_port: 8443,
		acme_email: None,
	}
}

fn translate_yaml(
	yaml: &str,
	env: &[(&str, &str)],
	domains: &BTreeMap<String, Domain>,
) -> translate::Plan {
	let env: BTreeMap<String, String> = env
		.iter()
		.map(|(k, v)| ((*k).to_string(), (*v).to_string()))
		.collect();
	let files = BTreeMap::new();
	let compose = compose::parse(
		yaml,
		&Inputs {
			env: &env,
			files: &files,
		},
	)
	.unwrap_or_else(|issues| panic!("refused: {issues:#?}"));
	translate::plan(
		&compose,
		&Settings {
			stack: 42,
			ingress: ingress(),
			domains,
		},
	)
	.unwrap_or_else(|issues| panic!("not translatable: {issues:#?}"))
}

fn spec_json(spec: &DeploySpec) -> Value {
	serde_json::to_value(spec).unwrap()
}

const APP_AND_POSTGRES: &str = r#"
services:
  app:
    image: ghcr.io/acme/app:1.4
    environment:
      DATABASE_URL: postgres://app:${DB_PASSWORD}@db:5432/app
      LOG_LEVEL: ${LOG_LEVEL:-info}
    expose:
      - "8080"
    depends_on:
      db:
        condition: service_healthy
    restart: unless-stopped
    deploy:
      resources:
        limits:
          memory: 256m
          cpus: "0.5"
  db:
    image: postgres:17
    environment:
      POSTGRES_USER: app
      POSTGRES_PASSWORD: ${DB_PASSWORD}
      POSTGRES_DB: app
    volumes:
      - pgdata:/var/lib/postgresql/data
    healthcheck:
      test: ["CMD-SHELL", "pg_isready -U app"]
      interval: 5s
      retries: 5
    restart: always
volumes:
  pgdata:
"#;

#[test]
fn two_services_translate_to_one_network_and_two_specs() {
	let domains = BTreeMap::from([(
		"app".to_string(),
		Domain {
			host: "app.example.com".to_string(),
			port: None,
		},
	)]);
	let plan = translate_yaml(APP_AND_POSTGRES, &[("DB_PASSWORD", "hunter2")], &domains);

	assert_eq!(plan.network, "podkit-stack-42");
	assert_eq!(plan.volumes, ["podkit-stack-42-pgdata"]);
	let order: Vec<&str> = plan.services.iter().map(|s| s.name.as_str()).collect();
	assert_eq!(order, ["db", "app"], "the database starts first");

	let db = &plan.services[0];
	assert_eq!(
		spec_json(&db.spec),
		json!({
			"service": "stack-42-db",
			"instance_id": 0,
			"source": {"type": "image", "reference": "postgres:17", "pull": true},
			"env": [
				["POSTGRES_DB", "app"],
				["POSTGRES_PASSWORD", "hunter2"],
				["POSTGRES_USER", "app"],
			],
			"command": null,
			"container_port": null,
			"routing": null,
			"network_aliases": [],
			"volumes": [{"name": "podkit-stack-42-pgdata", "path": "/var/lib/postgresql/data"}],
			"published_ports": [],
			"memory_bytes": null,
			"cpu_cores": null,
			"strategy": "recreate",
			"ingress": {"http_port": 8088, "https_port": 8443, "acme_email": null},
			"networks": ["podkit-stack-42"],
			"options": {
				"isolated": true,
				"net_aliases": ["db"],
				"healthcheck": {
					"test": ["CMD-SHELL", "pg_isready -U app"],
					"interval_ms": 5000,
					"retries": 5,
				},
			},
		}),
		"an unrouted service lives on the stack network only"
	);

	let app = &plan.services[1];
	assert_eq!(
		spec_json(&app.spec),
		json!({
			"service": "stack-42-app",
			"instance_id": 0,
			"source": {"type": "image", "reference": "ghcr.io/acme/app:1.4", "pull": true},
			"env": [
				["DATABASE_URL", "postgres://app:hunter2@db:5432/app"],
				["LOG_LEVEL", "info"],
			],
			"command": null,
			"container_port": 8080,
			"routing": {"http_hosts": ["app.example.com"], "https_hosts": []},
			"network_aliases": ["podkit-stack-42-app"],
			"volumes": [],
			"published_ports": [],
			"memory_bytes": 268_435_456,
			"cpu_cores": 0.5,
			"strategy": "rolling",
			"ingress": {"http_port": 8088, "https_port": 8443, "acme_email": null},
			"networks": ["podkit-stack-42"],
			"options": {"net_aliases": ["app"]},
		}),
		"a routed service also joins the ingress network, under a name unique to the stack"
	);
	assert_eq!(
		app.depends_on,
		[("db".to_string(), compose::Condition::Healthy)]
	);
}

#[test]
fn a_routed_service_lives_on_the_stack_network_and_a_linked_one_joins_the_others() {
	use crate::networks::attach::Linked;
	let domains = BTreeMap::from([(
		"app".to_string(),
		Domain {
			host: "app.example.com".to_string(),
			port: None,
		},
	)]);
	let mut plan = translate_yaml(APP_AND_POSTGRES, &[("DB_PASSWORD", "x")], &domains);
	let before: Vec<String> = plan.services.iter().map(|s| s.hash.clone()).collect();
	let names = |plan: &translate::Plan, service: &str| -> Vec<String> {
		plan.services
			.iter()
			.find(|s| s.name == service)
			.unwrap()
			.spec
			.attach
			.iter()
			.map(|a| a.name.clone())
			.collect()
	};

	// Not attached to anything: only the routed service has a place of its own, the
	// stack network, where Traefik reaches it. Nothing is on `podkit`.
	translate::link_networks(&mut plan, &[]);
	assert_eq!(names(&plan, "app"), ["podkit-stack-42"]);
	assert!(names(&plan, "db").is_empty());

	// Attached to a user network: every service joins it, the routed one after its own.
	let linked = [Linked {
		name: "podkit-net-9".to_string(),
		internal: true,
	}];
	translate::link_networks(&mut plan, &linked);
	assert_eq!(names(&plan, "app"), ["podkit-stack-42", "podkit-net-9"]);
	assert_eq!(names(&plan, "db"), ["podkit-net-9"]);
	let db = plan.services.iter().find(|s| s.name == "db").unwrap();
	assert_eq!(db.spec.attach[0].aliases, ["podkit-stack-42-db"]);
	assert!(db.spec.attach[0].internal);
	let after: Vec<String> = plan.services.iter().map(|s| s.hash.clone()).collect();
	assert_ne!(before, after, "attaching a network redeploys the services");
}

#[test]
fn the_hash_follows_the_spec_and_nothing_else() {
	let domains = BTreeMap::new();
	let first = translate_yaml(APP_AND_POSTGRES, &[("DB_PASSWORD", "a")], &domains);
	let again = translate_yaml(APP_AND_POSTGRES, &[("DB_PASSWORD", "a")], &domains);
	let hashes = |plan: &translate::Plan| -> Vec<String> {
		plan.services.iter().map(|s| s.hash.clone()).collect()
	};
	assert_eq!(hashes(&first), hashes(&again));

	// A different password changes the two services that use it, no others.
	let other = translate_yaml(APP_AND_POSTGRES, &[("DB_PASSWORD", "b")], &domains);
	assert_ne!(first.services[0].hash, other.services[0].hash);
	assert_ne!(first.services[1].hash, other.services[1].hash);

	// A change to one service leaves the other's hash alone.
	let edited = APP_AND_POSTGRES.replace("LOG_LEVEL:-info", "LOG_LEVEL:-debug");
	let edited = translate_yaml(&edited, &[("DB_PASSWORD", "a")], &domains);
	assert_eq!(
		first.services[0].hash, edited.services[0].hash,
		"db is unchanged"
	);
	assert_ne!(
		first.services[1].hash, edited.services[1].hash,
		"app changed"
	);

	// The instance id is per deploy and not part of the identity.
	let mut spec = first.services[0].spec.clone();
	spec.instance_id = 999;
	assert_eq!(
		serde_json::to_value(&spec).unwrap()["service"],
		"stack-42-db"
	);
}

#[test]
fn domains_need_a_known_service_and_a_port() {
	let env = BTreeMap::new();
	let files = BTreeMap::new();
	let compose = compose::parse(
		"services:\n  web:\n    image: x\n    expose: [80, 8080]\n  db:\n    image: y\n",
		&Inputs {
			env: &env,
			files: &files,
		},
	)
	.unwrap();
	let plan = |domains: BTreeMap<String, Domain>| {
		translate::plan(
			&compose,
			&Settings {
				stack: 1,
				ingress: ingress(),
				domains: &domains,
			},
		)
	};
	let host = |port| Domain {
		host: "x.example.com".into(),
		port,
	};
	assert!(plan(BTreeMap::from([("ghost".to_string(), host(Some(80)))])).is_err());
	let ambiguous = plan(BTreeMap::from([("web".to_string(), host(None))])).unwrap_err();
	assert_eq!(ambiguous[0].path, "services.web.domain");
	assert!(
		ambiguous[0].message.contains("80, 8080"),
		"{}",
		ambiguous[0].message
	);
	assert!(plan(BTreeMap::from([("web".to_string(), host(Some(8080)))])).is_ok());

	// Published ports cannot take the ingress's.
	let greedy = compose::parse(
		"services:\n  web:\n    image: x\n    ports: ['8088:80']\n",
		&Inputs {
			env: &env,
			files: &files,
		},
	)
	.unwrap();
	let err = translate::plan(
		&greedy,
		&Settings {
			stack: 1,
			ingress: ingress(),
			domains: &BTreeMap::new(),
		},
	)
	.unwrap_err();
	assert!(err[0].message.contains("ingress"), "{err:?}");
}

#[test]
fn https_hosts_follow_acme() {
	let env = BTreeMap::new();
	let files = BTreeMap::new();
	let compose = compose::parse(
		"services:\n  web:\n    image: x\n    expose: [80]\n",
		&Inputs {
			env: &env,
			files: &files,
		},
	)
	.unwrap();
	let domains = BTreeMap::from([(
		"web".to_string(),
		Domain {
			host: "x.example.com".into(),
			port: None,
		},
	)]);
	let mut with_acme = ingress();
	with_acme.acme_email = Some("ops@example.com".into());
	let plan = translate::plan(
		&compose,
		&Settings {
			stack: 1,
			ingress: with_acme,
			domains: &domains,
		},
	)
	.unwrap();
	let routing = plan.services[0].spec.routing.as_ref().unwrap();
	assert_eq!(routing.http_hosts, ["x.example.com"]);
	assert_eq!(routing.https_hosts, ["x.example.com"]);
}

// ---- Dokploy corpus ----

/// Tier of every Dokploy template, from the spike.
fn tiers() -> BTreeMap<String, String> {
	let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../docs/spikes/compose-coverage.json");
	let json: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
	json["templates"]
		.as_array()
		.unwrap()
		.iter()
		.map(|t| {
			(
				t["id"].as_str().unwrap().to_string(),
				t["tier"].as_str().unwrap().to_string(),
			)
		})
		.collect()
}

/// A `${helper}` or `${helper:n}` of template.toml, as a fixed value. It is a
/// number because templates put these in ports and limits as well as names.
fn flatten_helpers(text: &str) -> String {
	let mut out = String::new();
	let mut rest = text;
	while let Some(start) = rest.find("${") {
		out.push_str(&rest[..start]);
		rest = &rest[start + 2..];
		match rest.find('}') {
			Some(end) => {
				out.push_str("8080");
				rest = &rest[end + 1..];
			}
			None => break,
		}
	}
	out.push_str(rest);
	out
}

/// What a template supplies besides the compose file: the variables of
/// `config.env`, and `config.mounts` as files by path.
fn template_inputs(toml_text: &str) -> (BTreeMap<String, String>, BTreeMap<String, String>) {
	let parsed: toml::Table = toml_text.parse().unwrap_or_default();
	let config = parsed.get("config").and_then(toml::Value::as_table);
	let mut env = BTreeMap::new();
	match config.and_then(|c| c.get("env")) {
		Some(toml::Value::Array(entries)) => {
			for entry in entries.iter().filter_map(toml::Value::as_str) {
				if let Some((k, v)) = entry.split_once('=') {
					env.insert(k.trim().to_string(), flatten_helpers(v));
				}
			}
		}
		Some(toml::Value::Table(table)) => {
			for (k, v) in table {
				if let Some(v) = v.as_str() {
					env.insert(k.clone(), flatten_helpers(v));
				}
			}
		}
		_ => {}
	}
	let mut files = BTreeMap::new();
	if let Some(mounts) = config
		.and_then(|c| c.get("mounts"))
		.and_then(toml::Value::as_array)
	{
		for mount in mounts.iter().filter_map(toml::Value::as_table) {
			let (Some(path), Some(content)) = (
				mount.get("filePath").and_then(toml::Value::as_str),
				mount.get("content").and_then(toml::Value::as_str),
			) else {
				continue;
			};
			let path = path
				.strip_prefix("./")
				.unwrap_or(path)
				.trim_start_matches('/');
			files.insert(path.to_string(), flatten_helpers(content));
		}
	}
	(env, files)
}

/// Translates one template; the error says what failed.
fn translate_template(dir: &Path) -> Result<(), String> {
	let yaml = std::fs::read_to_string(dir.join("docker-compose.yml"))
		.map_err(|e| format!("cannot read docker-compose.yml: {e}"))?;
	let toml_text = std::fs::read_to_string(dir.join("template.toml")).unwrap_or_default();
	let (env, files) = template_inputs(&toml_text);
	let compose = compose::parse(
		&yaml,
		&Inputs {
			env: &env,
			files: &files,
		},
	)
	.map_err(|issues| format!("refused: {issues:?}"))?;

	// Route every service that exposes exactly one port, the common case.
	let domains: BTreeMap<String, Domain> = compose
		.services
		.iter()
		.filter(|(_, s)| s.expose.len() == 1)
		.map(|(name, _)| {
			(
				name.clone(),
				Domain {
					host: format!("{name}.example.com"),
					port: None,
				},
			)
		})
		.collect();
	let plan = translate::plan(
		&compose,
		&Settings {
			stack: 7,
			ingress: ingress(),
			domains: &domains,
		},
	)
	.map_err(|issues| format!("not translatable: {issues:?}"))?;
	if plan.services.len() != compose.services.len() {
		return Err("a service went missing".to_string());
	}
	// Every spec must be one the agent will accept.
	for planned in &plan.services {
		let json = serde_json::to_string(&planned.spec).map_err(|e| e.to_string())?;
		serde_json::from_str::<DeploySpec>(&json).map_err(|e| e.to_string())?;
	}
	Ok(())
}

fn fixtures_dir() -> PathBuf {
	Path::new(env!("CARGO_MANIFEST_DIR")).join("src/stacks/fixtures/dokploy")
}

#[test]
fn fixture_templates_are_all_fully_supported_and_translate() {
	let tiers = tiers();
	let mut seen = 0;
	let mut failures = Vec::new();
	for entry in std::fs::read_dir(fixtures_dir()).unwrap() {
		let dir = entry.unwrap().path();
		if !dir.is_dir() {
			continue;
		}
		let id = dir.file_name().unwrap().to_string_lossy().to_string();
		assert_eq!(
			tiers.get(&id).map(String::as_str),
			Some("full"),
			"{id} is not fully supported in the spike's corpus"
		);
		seen += 1;
		if let Err(e) = translate_template(&dir) {
			failures.push(format!("{id}: {e}"));
		}
	}
	assert!(seen >= 30, "only {seen} fixtures");
	assert!(
		failures.is_empty(),
		"{} failed:\n{}",
		failures.len(),
		failures.join("\n")
	);
}

/// `PODKIT_COMPOSE_CORPUS=/path/to/Dokploy/templates cargo test -p server
/// --all-features -- --ignored every_fully_supported`
#[test]
#[ignore = "needs a checkout of github.com/Dokploy/templates in PODKIT_COMPOSE_CORPUS"]
fn every_fully_supported_template_translates() {
	let root = PathBuf::from(
		std::env::var("PODKIT_COMPOSE_CORPUS").expect("set PODKIT_COMPOSE_CORPUS to the checkout"),
	)
	.join("blueprints");
	let mut tried = 0;
	let mut failures = Vec::new();
	for (id, tier) in tiers() {
		if tier != "full" {
			continue;
		}
		tried += 1;
		if let Err(e) = translate_template(&root.join(&id)) {
			failures.push(format!("{id}: {e}"));
		}
	}
	assert!(tried > 400, "only {tried} templates");
	assert!(
		failures.is_empty(),
		"{} of {tried} failed:\n{}",
		failures.len(),
		failures.join("\n")
	);
}
