use super::*;
use crate::iac::doc::{parse, render};

/// A shop as it is exported from the team that runs it.
const SHOP: &str = r#"version = 1

[projects.shop]
name = "Shop"

[projects.shop.environments.production]
name = "Production"

[projects.shop.environments.production.apps.web]
server = "fra-1"
slug = "shop-web"
repo = "git@github.com:acme/shop.git"
port = 3000
deploy_key = "ci"
domains = ["shop.example.com"]

[projects.shop.environments.production.apps.web.env]
DATABASE_URL = "${secret:production/web/DATABASE_URL}"
SESSION_KEY = "${generate:session-key}"
PUBLIC = "plain"

[projects.shop.environments.production.apps.worker]
server = "fra-1"
slug = "shop-worker"
image = "registry.acme.dev/shop/worker:3"
port = 8080
registry = "acme"

[projects.shop.environments.production.databases.main]
server = "fra-2"
engine = "postgres"

[projects.shop.environments.production.stacks.search]
server = "fra-1"
compose = "services:\n  search:\n    image: getmeili/meilisearch:v1\n"

[projects.shop.environments.production.stacks.search.env]
MEILI_MASTER_KEY = "${generate:search-key}"

[projects.shop.environments.production.cron.report]
kind = "exec"
app = "web"
command = "bin/report"
schedule = "0 6 * * *"

[projects.shop.environments.production.cron.prune]
kind = "container"
server = "fra-2"
image = "alpine:3"
command = "true"
schedule = "0 3 * * 0"
"#;

fn shop() -> Doc {
	parse(SHOP).expect("the fixture is a valid project file")
}

fn at(slots: &[Slot]) -> Vec<(&str, Option<&str>)> {
	slots
		.iter()
		.map(|s| (s.at.as_str(), s.suggested.as_deref()))
		.collect()
}

#[test]
fn a_preset_asks_for_every_server_key_registry_and_secret_it_names() {
	let needs = needs(&shop()).unwrap();
	assert_eq!(
		at(&needs.servers),
		[
			("production/app/web", Some("fra-1")),
			("production/app/worker", Some("fra-1")),
			("production/database/main", Some("fra-2")),
			("production/stack/search", Some("fra-1")),
			// `report` runs inside the web application: it has no server of its own.
			("production/cron/prune", Some("fra-2")),
		]
	);
	assert_eq!(at(&needs.deploy_keys), [("production/app/web", Some("ci"))]);
	assert_eq!(
		at(&needs.registries),
		[("production/app/worker", Some("acme"))]
	);
	assert_eq!(needs.secrets, ["production/web/DATABASE_URL"]);
	assert_eq!(needs.generated, ["search-key", "session-key"]);
}

#[test]
fn saving_a_project_as_a_preset_takes_the_team_s_names_out_and_no_more() {
	let saved = strip(shop());
	let text = render(&saved).unwrap();
	for name in [
		"fra-1",
		"fra-2",
		"\"ci\"",
		"\"acme\"",
		"shop.example.com",
		"\"shop-web\"",
	] {
		assert!(!text.contains(name), "{name} is still in:\n{text}");
	}
	// What the project is stays: the repository, the image, the placeholders.
	assert!(text.contains("git@github.com:acme/shop.git"));
	assert!(text.contains("registry.acme.dev/shop/worker:3"));
	assert!(text.contains("${secret:production/web/DATABASE_URL}"));
	assert!(text.contains("slug = \"${project}-web\""), "{text}");

	let needs = needs(&saved).unwrap();
	assert_eq!(needs.servers.len(), 5);
	assert!(needs.servers.iter().all(|s| s.suggested.is_none()));
	assert_eq!(at(&needs.deploy_keys), [("production/app/web", None)]);
	// It is still a project file: it reads back as what was written.
	assert_eq!(parse(&text).unwrap(), saved);
}

fn answers() -> Bindings {
	let pick = |pairs: &[(&str, &str)]| {
		pairs
			.iter()
			.map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
			.collect()
	};
	Bindings {
		servers: pick(&[
			("production/app/web", "node-a"),
			("production/app/worker", "node-b"),
			("production/database/main", "node-a"),
			("production/stack/search", "node-b"),
			("production/cron/prune", "node-a"),
		]),
		deploy_keys: pick(&[("production/app/web", "deploy")]),
		registries: pick(&[("production/app/worker", "ghcr")]),
		secrets: pick(&[("production/web/DATABASE_URL", "postgres://u:p@db/shop")]),
	}
}

#[test]
fn binding_writes_each_choice_where_it_belongs_and_makes_up_what_it_was_asked_to() {
	let mut made = 0;
	let (doc, secrets) = bind(strip(shop()), &answers(), "my-shop", || {
		made += 1;
		format!("made-{made}")
	})
	.unwrap();
	let env = &doc.projects["shop"].environments["production"];
	assert_eq!(env.apps["web"].server, "node-a");
	assert_eq!(env.apps["web"].slug, "my-shop-web");
	assert_eq!(env.apps["worker"].slug, "my-shop-worker");
	assert_eq!(env.apps["web"].deploy_key.as_deref(), Some("deploy"));
	assert_eq!(env.apps["worker"].server, "node-b");
	assert_eq!(env.apps["worker"].registry.as_deref(), Some("ghcr"));
	assert_eq!(env.databases["main"].server, "node-a");
	assert_eq!(env.stacks["search"].server, "node-b");
	assert_eq!(env.cron["prune"].server.as_deref(), Some("node-a"));
	assert_eq!(env.cron["report"].server, None);

	// A made-up secret is an ordinary one afterwards, with its value beside the typed one.
	assert_eq!(env.apps["web"].env["SESSION_KEY"], "${secret:session-key}");
	assert_eq!(
		env.stacks["search"].env["MEILI_MASTER_KEY"],
		"${secret:search-key}"
	);
	assert_eq!(env.apps["web"].env["PUBLIC"], "plain");
	assert_eq!(
		secrets.0["production/web/DATABASE_URL"],
		"postgres://u:p@db/shop"
	);
	assert_eq!(secrets.0["search-key"], "made-1");
	assert_eq!(secrets.0["session-key"], "made-2");
	assert!(!render(&doc).unwrap().contains("${choose}"));
}

#[test]
fn binding_names_everything_left_unanswered_at_once_and_changes_nothing() {
	let mut some = answers();
	some.servers.remove("production/database/main");
	some.deploy_keys.clear();
	some.secrets
		.insert("production/web/DATABASE_URL".to_owned(), String::new());
	let missing = bind(strip(shop()), &some, "x", String::new).unwrap_err();
	assert_eq!(
		missing,
		[
			"production/app/web needs an SSH key",
			"production/database/main needs a server",
			"the secret production/web/DATABASE_URL needs a value",
		]
	);
}

#[test]
fn a_preset_is_one_project() {
	let mut two = shop();
	let copy = two.projects["shop"].clone();
	two.projects.insert("other".to_owned(), copy);
	assert!(needs(&two).unwrap_err().contains("exactly one project"));
	assert!(needs(&Doc::new()).is_err());
}
