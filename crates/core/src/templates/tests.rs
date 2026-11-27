//! Offline tests of the template format, the variable helpers, the embedded
//! gallery, the API and the deploy; and the ignored live test that deploys
//! three real templates.
//!
//! `cargo test -p podkit-core --all-features -- templates::`
//! `cargo test -p podkit-core --all-features -- --ignored template_live`

use std::collections::BTreeMap;

use axum::Router;
use axum::http::StatusCode;
use base64::Engine;
use hmac::{Hmac, KeyInit, Mac};
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt;

use super::expr::{self, Context, Counter, Helper, Os};
use super::render::{Placement, render};
use super::{Template, Variable, find, load_all, search};
use crate::tests::{call, count, server_id, state, user_id, user_with_team};

fn place() -> Placement<'static> {
	Placement {
		slug: "demo",
		ip: "203-0-113-7",
		flat: false,
	}
}

fn template(extra: &str) -> Result<Template, super::Invalid> {
	Template::parse(&format!(
		"id = \"demo\"\nname = \"Demo\"\ndescription = \"A demo.\"\ncategory = \"tools\"\nlogo = \"@n8n\"\n{extra}"
	))
}

fn make(helper: &Helper) -> String {
	let vars = BTreeMap::new();
	expr::generate(
		helper,
		&mut Context {
			rng: &mut Os,
			variables: &vars,
			domain: "x.example.com",
		},
	)
}

// ------------------------------------------------------------ helpers

#[test]
fn passwords_have_the_length_and_charset_asked_for() {
	for n in [1, 8, 16, 32, 64, 1024] {
		let p = make(&Helper::Password(n));
		assert_eq!(p.len(), n);
		assert!(
			p.bytes()
				.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()),
			"{p}"
		);
	}
	let all: std::collections::BTreeSet<char> = (0..40)
		.flat_map(|_| make(&Helper::Password(64)).chars().collect::<Vec<_>>())
		.collect();
	assert!(all.len() > 30, "uses most of a-z0-9: {all:?}");
	assert_ne!(make(&Helper::Password(32)), make(&Helper::Password(32)));
}

#[test]
fn base64_is_n_random_bytes_with_padding() {
	for n in [1, 2, 3, 32, 96] {
		let b = make(&Helper::Base64(n));
		let decoded = base64::engine::general_purpose::STANDARD
			.decode(&b)
			.unwrap();
		assert_eq!(decoded.len(), n, "{b}");
		assert_eq!(b.len(), n.div_ceil(3) * 4);
	}
}

#[test]
fn hash_uuid_and_jwt_have_their_shapes() {
	for n in [1, 7, 8, 40] {
		let h = make(&Helper::Hash(n));
		assert_eq!(h.len(), n);
		assert!(
			h.bytes()
				.all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
		);
	}
	let u = make(&Helper::Uuid);
	let parts: Vec<&str> = u.split('-').collect();
	assert_eq!(
		parts.iter().map(|p| p.len()).collect::<Vec<_>>(),
		[8, 4, 4, 4, 12]
	);
	assert!(parts[2].starts_with('4'), "version 4: {u}");
	assert!("89ab".contains(&parts[3][..1]), "variant: {u}");

	assert_eq!(make(&Helper::JwtHex(16)).len(), 32);
	let token = make(&Helper::Jwt {
		secret: None,
		payload: None,
	});
	assert_eq!(token.split('.').count(), 3);
	let head = base64::engine::general_purpose::URL_SAFE_NO_PAD
		.decode(token.split('.').next().unwrap())
		.unwrap();
	assert_eq!(head, br#"{"alg":"HS256","typ":"JWT"}"#);
}

#[test]
fn a_jwt_is_signed_with_the_named_variable_and_carries_its_claims() {
	let vars = BTreeMap::from([
		("secret".to_string(), "s3cret".to_string()),
		("claims".to_string(), r#"{"role":"anon"}"#.to_string()),
	]);
	let make = || {
		expr::expand(
			"${jwt:secret:claims}",
			&mut Context {
				rng: &mut Os,
				variables: &vars,
				domain: "d",
			},
		)
		.unwrap()
	};
	let token = make();
	let parts: Vec<&str> = token.split('.').collect();
	let claims = base64::engine::general_purpose::URL_SAFE_NO_PAD
		.decode(parts[1])
		.unwrap();
	let claims: Value = serde_json::from_slice(&claims).unwrap();
	assert_eq!(claims["role"], "anon");
	assert_eq!(claims["exp"], 1_893_456_000);
	// Same secret and payload sign the same way (iat aside, the same second).
	let mut mac = Hmac::<sha2::Sha256>::new_from_slice(b"s3cret").unwrap();
	mac.update(format!("{}.{}", parts[0], parts[1]).as_bytes());
	let want = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes());
	assert_eq!(parts[2], want);
}

#[test]
fn unsupported_helpers_and_bad_lengths_are_refused() {
	for bad in [
		"${timestamp}",
		"${timestamps}",
		"${timestampms}",
		"${randomPort}",
		"${password:0}",
		"${password:abc}",
		"${password:99999}",
		"${base64:-1}",
		"${hash:1.5}",
	] {
		assert!(expr::scan(bad).is_err(), "{bad}");
	}
	for fine in [
		"${password}",
		"${password:16}",
		"${HOME}",
		"${1+x}",
		"${line%%=*}",
		"no expression",
	] {
		assert!(expr::scan(fine).is_ok(), "{fine}");
	}
	// An expression in a template is refused when it is loaded.
	let err = template("[variables]\nport = \"${randomPort}\"\n").unwrap_err();
	assert!(err.0.contains("randomPort"), "{err}");
}

#[test]
fn an_unknown_name_is_left_as_written() {
	let vars = BTreeMap::from([("known".to_string(), "yes".to_string())]);
	let out = expr::expand(
		"a ${known} b ${HOME} c ${x:-y}",
		&mut Context {
			rng: &mut Os,
			variables: &vars,
			domain: "d",
		},
	)
	.unwrap();
	assert_eq!(out, "a yes b ${HOME} c ${x:-y}");
}

// ------------------------------------------------------------- format

#[test]
fn unknown_variable_types_are_refused() {
	for kind in ["number", "password", "select", "boolean", ""] {
		let err = template(&format!(
			"[variables]\nwho = {{ type = \"{kind}\", default = \"x\" }}\n"
		))
		.unwrap_err();
		assert!(err.0.contains("unknown type"), "{kind}: {err}");
	}
	assert!(template("[variables]\nwho = { type = \"string\", default = \"x\" }\n").is_ok());
	// A table that is not a variable at all.
	assert!(template("[variables]\nwho = { colour = \"red\" }\n").is_err());
	// Unknown keys anywhere are refused rather than ignored.
	assert!(template("flavour = \"x\"\n").is_err());
	assert!(template("[config]\nvolumes = []\n").is_err());
	assert!(template("[requires]\ncpu = 4\n").is_err());
}

#[test]
fn names_categories_and_ids_are_checked() {
	for id in [
		"",
		"Upper",
		"a b",
		"-a",
		"a-",
		"a_b",
		"a.b",
		&"a".repeat(51),
	] {
		assert!(!super::valid_id(id), "{id:?}");
	}
	assert!(super::valid_id("uptime-kuma"));
	let parse = |category: &str, logo: &str| {
		Template::parse(&format!(
			"id = \"x\"\nname = \"X\"\ndescription = \"d\"\ncategory = \"{category}\"\nlogo = \"{logo}\"\n"
		))
	};
	assert!(parse("tools", "logo.svg").is_ok());
	assert!(parse("tools", "@n8n").is_ok());
	assert!(parse("pets", "logo.svg").is_err());
	assert!(parse("tools", "../x.svg").is_err());
	assert!(parse("tools", "@a/b").is_err());
	assert!(parse("tools", "").is_err());
}

#[test]
fn routing_by_path_and_two_domains_for_one_service_are_refused() {
	let domain = |extra: &str| {
		template(&format!(
			"[[config.domains]]\nserviceName = \"web\"\nport = 80\n{extra}"
		))
	};
	assert!(domain("").is_ok());
	assert!(domain("path = \"/\"\n").is_ok());
	let err = domain("path = \"/api\"\n").unwrap_err();
	assert!(err.0.contains("path"), "{err}");
	let err = template(
		"[[config.domains]]\nserviceName = \"web\"\nport = 80\n[[config.domains]]\nserviceName = \"web\"\nport = 81\n",
	)
	.unwrap_err();
	assert!(err.0.contains("more than one domain"), "{err}");
	assert!(template("[[config.domains]]\nserviceName = \"web\"\nport = 0\n").is_err());
}

#[test]
fn mounts_stay_inside_the_template_and_reveal_names_a_generated_variable() {
	let mount = |path: &str| {
		template(&format!(
			"[[config.mounts]]\nfilePath = \"{path}\"\ncontent = \"x\"\n"
		))
	};
	assert!(mount("./config/app.conf").is_ok());
	assert!(
		mount("/nginx/nginx.conf").is_ok(),
		"Dokploy writes them with a slash"
	);
	for bad in ["../x", "a/../../x", "", "./", "a//b"] {
		assert!(mount(bad).is_err(), "{bad:?}");
	}
	assert_eq!(
		super::mount_key("/nginx/nginx.conf").as_deref(),
		Some("nginx/nginx.conf")
	);
	assert_eq!(super::mount_key("../files/a/b").as_deref(), Some("a/b"));

	assert!(template("reveal = [\"pw\"]\n[variables]\npw = \"${password:20}\"\n").is_ok());
	for bad in [
		"reveal = [\"nope\"]\n",
		"reveal = [\"who\"]\n[variables]\nwho = { type = \"string\", default = \"x\" }\n",
	] {
		assert!(template(bad).is_err(), "{bad}");
	}
}

#[test]
fn env_may_be_a_table_or_dokploys_list_and_survives_a_round_trip() {
	let table = template("[config.env]\nA = \"1\"\nB = 2\nC = true\n").unwrap();
	let list = template("[config]\nenv = [\"A=1\", \"B=2\", { C = true }]\n").unwrap();
	assert_eq!(table.config.env, list.config.env);
	assert_eq!(table.config.env["B"], "2");
	assert_eq!(table.config.env["C"], "true");

	let t = template(
		"[variables]\nd = \"${domain}\"\nask = { type = \"string\", default = \"x\", secret = true }\n[config.env]\nA = \"${d}\"\n[[config.domains]]\nserviceName = \"web\"\nport = 80\nhost = \"${d}\"\n[[config.mounts]]\nfilePath = \"a.conf\"\ncontent = \"line1\\nline2\\n\"\n",
	)
	.unwrap();
	let again = Template::parse(&t.to_toml().unwrap()).unwrap();
	assert_eq!(t, again);
}

// ------------------------------------------------------------- render

#[test]
fn render_fills_variables_env_domains_and_files() {
	let t = template(
		"reveal = [\"admin_password\"]\n[variables]\nmain_domain = \"${domain}\"\nadmin_password = \"${password:24}\"\nsalt = \"${base64:9}\"\nsite = { type = \"string\", default = \"My site\" }\n[config.env]\nHOST = \"${main_domain}\"\nADMIN = \"${admin_password}\"\nSITE = \"${site}\"\n[[config.domains]]\nserviceName = \"web\"\nport = 8080\nhost = \"${main_domain}\"\n[[config.mounts]]\nfilePath = \"./conf/app.ini\"\ncontent = \"salt=${salt}\\nhost=${main_domain}\\n\"\n",
	)
	.unwrap();
	let r = render(&t, &BTreeMap::new(), &place(), &mut Os).unwrap();
	assert_eq!(r.domains.len(), 1);
	assert_eq!(r.domains[0].host, "web-demo.203-0-113-7.sslip.io");
	assert_eq!(r.domains[0].port, 8080);
	assert_eq!(r.env["HOST"], r.domains[0].host);
	assert_eq!(r.env["ADMIN"].len(), 24);
	assert_eq!(r.env["SITE"], "My site");
	let ini = &r.files["conf/app.ini"];
	assert!(
		ini.contains(&format!("host={}", r.domains[0].host)),
		"{ini}"
	);
	assert!(ini.contains(&format!("salt={}", {
		let line = ini.lines().next().unwrap();
		line.trim_start_matches("salt=").to_string()
	})));

	// Credentials are named, kept out of `values`, and revealed only on request.
	assert!(r.secrets.contains("admin_password"));
	assert!(r.secrets.contains("salt"));
	assert!(!r.values.contains_key("admin_password"));
	assert!(!r.values.contains_key("salt"));
	assert_eq!(r.values["main_domain"], r.domains[0].host);
	assert_eq!(r.values["site"], "My site");
	assert_eq!(r.reveal.keys().collect::<Vec<_>>(), ["admin_password"]);
	assert_eq!(r.reveal["admin_password"], r.env["ADMIN"]);

	// Two renders make two sets of secrets.
	let again = render(&t, &BTreeMap::new(), &place(), &mut Os).unwrap();
	assert_ne!(r.env["ADMIN"], again.env["ADMIN"]);
	// The counter is for checking templates: it repeats itself.
	let a = render(&t, &BTreeMap::new(), &place(), &mut Counter(1)).unwrap();
	let b = render(&t, &BTreeMap::new(), &place(), &mut Counter(1)).unwrap();
	assert_eq!(a.env, b.env);
}

#[test]
fn render_takes_answers_for_inputs_and_hostnames_and_nothing_else() {
	let t = template(
		"[variables]\nmain_domain = \"${domain}\"\nsite = { type = \"string\", default = \"d\" }\nname = { type = \"string\" }\npw = \"${password:8}\"\n[config.env]\nSITE = \"${site}-${name}\"\nHOST = \"${main_domain}\"\n[[config.domains]]\nserviceName = \"web\"\nport = 80\nhost = \"${main_domain}\"\n",
	)
	.unwrap();
	let ask = |pairs: &[(&str, &str)]| {
		let inputs: BTreeMap<String, String> = pairs
			.iter()
			.map(|(k, v)| ((*k).to_string(), (*v).to_string()))
			.collect();
		render(&t, &inputs, &place(), &mut Os)
	};
	// `name` has no default.
	assert!(ask(&[]).unwrap_err().0.contains("name is required"));
	let r = ask(&[("name", "bob"), ("site", "mine")]).unwrap();
	assert_eq!(r.env["SITE"], "mine-bob");
	// An empty answer falls back to the default.
	assert_eq!(
		ask(&[("name", "bob"), ("site", "  ")]).unwrap().env["SITE"],
		"d-bob"
	);
	// Your own hostname for a domain variable.
	let r = ask(&[("name", "x"), ("main_domain", "Apps.Example.com")]).unwrap();
	assert_eq!(r.domains[0].host, "apps.example.com");
	assert_eq!(r.env["HOST"], "Apps.Example.com".trim());
	assert!(ask(&[("name", "x"), ("main_domain", "not a host")]).is_err());
	// A generated variable is not yours to set; neither is a name nobody defined.
	for key in ["pw", "other"] {
		let e = ask(&[("name", "x"), (key, "v")]).unwrap_err();
		assert!(e.0.contains("not a variable you can set"), "{key}: {e}");
	}
	assert!(ask(&[("name", &"x".repeat(5000))]).is_err());
}

#[test]
fn variables_may_refer_to_each_other_and_cycles_are_refused() {
	let t = template(
		"[variables]\nb = \"${a}-b\"\na = \"${password:6}\"\nc = \"${b}-${a}\"\n[config.env]\nC = \"${c}\"\n",
	)
	.unwrap();
	let r = render(&t, &BTreeMap::new(), &place(), &mut Os).unwrap();
	let c = &r.env["C"];
	let a: String = c.chars().take(6).collect();
	assert_eq!(c, &format!("{a}-b-{a}"));
	assert!(
		r.secrets.contains("c"),
		"a reference to a secret is a secret"
	);

	let t = template("[variables]\na = \"${b}\"\nb = \"${a}\"\n").unwrap();
	assert!(render(&t, &BTreeMap::new(), &place(), &mut Os).is_err());
}

#[test]
fn two_domain_variables_get_one_host_each() {
	let t = template(
		"[variables]\napi_domain = \"${domain}\"\nweb_domain = \"${domain}\"\n[[config.domains]]\nserviceName = \"api\"\nport = 1\nhost = \"${api_domain}\"\n[[config.domains]]\nserviceName = \"web\"\nport = 2\nhost = \"${web_domain}\"\n",
	)
	.unwrap();
	let r = render(&t, &BTreeMap::new(), &place(), &mut Os).unwrap();
	assert_eq!(r.domains[0].host, "api-demo.203-0-113-7.sslip.io");
	assert_eq!(r.domains[1].host, "web-demo.203-0-113-7.sslip.io");
	let flat = Placement {
		flat: true,
		..place()
	};
	let r = render(&t, &BTreeMap::new(), &flat, &mut Os).unwrap();
	assert_eq!(r.domains[0].host, "demo.203-0-113-7.sslip.io");
}

// ---------------------------------------------------------- the gallery

#[test]
fn every_template_kept_for_the_tests_parses_validates_and_translates() {
	let registry = load_all();
	assert!(
		registry.errors.is_empty(),
		"templates that do not load: {:#?}",
		registry.errors
	);
	assert!(!registry.templates.is_empty());
	for (id, l) in &registry.templates {
		assert_eq!(&l.template.id, id);
		assert!(!l.services.is_empty(), "{id} has no services");
		// What the translation says it will run, and no ghosts.
		for domain in &l.template.config.domains {
			assert!(
				l.services.iter().any(|s| s.name == domain.service_name),
				"{id}: domain for {}",
				domain.service_name
			);
		}
	}
}

#[test]
fn the_hand_written_templates_are_there_with_podkit_models_in_mind() {
	for id in ["n8n", "openclaw", "hermes-agent"] {
		let l = find(id).unwrap_or_else(|| panic!("{id} is missing"));
		assert!(l.template.upstream.is_none(), "{id} is hand-written");
		assert!(l.template.requires.ram_mb.is_some(), "{id}");
	}
	// n8n and OpenClaw are plain applications, so a model can be linked.
	for id in ["n8n", "openclaw"] {
		let l = find(id).unwrap();
		assert!(l.template.gateway && l.plain_app, "{id}");
	}
	// Hermes needs `gateway run`, which only a stack can say: it takes the endpoint as inputs.
	let hermes = find("hermes-agent").unwrap();
	assert!(!hermes.plain_app && !hermes.template.gateway);
	assert!(matches!(
		hermes.template.variables.get("openai_base_url"),
		Some(Variable::Input(_))
	));
	assert!(
		hermes
			.template
			.reveal
			.contains(&"dashboard_password".to_string())
	);
}

#[test]
fn every_template_has_a_logo_that_resolves_to_a_file() {
	for (id, l) in &load_all().templates {
		let url = crate::logos::template_url(l);
		assert!(url.starts_with("/static/logos/"), "{id}: {url:?}");
		if let Some(name) = l.template.logo.strip_prefix('@') {
			assert!(load_all().logo_file(id).is_none(), "{id}");
			assert!(crate::logos::has(name), "{id}: registry logo {name}");
		} else {
			let ext = l.template.logo.rsplit('.').next().unwrap();
			assert!(["svg", "png"].contains(&ext), "{id}: {}", l.template.logo);
			let size = std::fs::metadata(load_all().logo_file(id).unwrap())
				.unwrap()
				.len();
			assert!((1..=super::MAX_LOGO_BYTES as u64).contains(&size), "{id}");
			assert!(
				url.contains(&l.logo.hash),
				"{id}: the URL carries the file's hash"
			);
		}
	}
	// Every file of the registry is a small SVG or PNG and a real image.
	for (name, bytes) in crate::templates::embed::LOGOS {
		assert!(
			bytes.len() <= super::MAX_LOGO_BYTES,
			"{name} is {} bytes",
			bytes.len()
		);
		if std::path::Path::new(name)
			.extension()
			.is_some_and(|e| e == "svg")
		{
			assert!(String::from_utf8_lossy(bytes).contains("<svg"), "{name}");
		} else {
			assert!(bytes.starts_with(b"\x89PNG"), "{name}");
		}
	}
}

#[test]
fn no_template_logo_can_run_script() {
	for (name, bytes) in crate::templates::embed::LOGOS {
		let text = String::from_utf8_lossy(bytes).to_lowercase();
		for bad in [
			"<script",
			"javascript:",
			"<foreignobject",
			" onload=",
			" onerror=",
		] {
			assert!(!text.contains(bad), "{name} contains {bad}");
		}
	}
}

#[test]
fn search_matches_names_tags_and_categories() {
	let ids = |q: Option<&str>, c: Option<&str>| -> Vec<String> {
		search(q, c).iter().map(|l| l.template.id.clone()).collect()
	};
	let everything = ids(None, None);
	assert_eq!(everything.len(), load_all().templates.len());
	assert_eq!(
		ids(Some("  "), None),
		everything,
		"a blank query is no query"
	);

	assert_eq!(
		ids(Some("n8n"), None).first().map(String::as_str),
		Some("n8n"),
		"exact id first"
	);
	assert!(
		ids(Some("N8N"), None).contains(&"n8n".to_string()),
		"case does not matter"
	);
	let ai = ids(None, Some("ai"));
	assert!(ai.contains(&"openclaw".to_string()) && ai.contains(&"hermes-agent".to_string()));
	assert!(!ai.contains(&"n8n".to_string()));
	assert!(
		ids(Some("workflow automation"), None).contains(&"n8n".to_string()),
		"every word must match"
	);
	assert!(
		!ids(Some("n8n"), Some("ai")).contains(&"n8n".to_string()),
		"n8n is automation"
	);
	assert!(ids(Some("zzzz-no-such-template"), None).is_empty());
	for l in search(None, Some("database")) {
		assert_eq!(l.template.category, "database");
	}
}

#[test]
fn logos_are_guessed_from_image_names() {
	let guess = |image: &str| {
		crate::logos::for_image(image).map(|u| {
			let file = u.rsplit('/').next().unwrap().to_string();
			file.split('.').next().unwrap().to_string()
		})
	};
	assert_eq!(guess("postgres:18").as_deref(), Some("postgres"));
	assert_eq!(
		guess("docker.io/library/postgres:18-alpine").as_deref(),
		Some("postgres")
	);
	assert_eq!(
		guess("pgvector/pgvector:pg17").as_deref(),
		Some("postgresql")
	);
	assert_eq!(guess("redis:7-alpine").as_deref(), Some("redis"));
	assert_eq!(guess("mongo:8").as_deref(), Some("mongodb"));
	assert_eq!(
		guess("ghcr.io/foo/mysql@sha256:abc").as_deref(),
		Some("mysql")
	);
	assert_eq!(guess("localhost:5000/nginx:1").as_deref(), Some("nginx"));
	assert_eq!(guess("traefik/whoami").as_deref(), Some("traefik"));
	assert_eq!(guess("node:22").as_deref(), Some("nodejs"));
	assert_eq!(guess("golang:1.25").as_deref(), Some("go"));
	assert_eq!(guess("docker.io/ollama/ollama").as_deref(), Some("ollama"));
	assert_eq!(guess("qwen/qwen3-coder").as_deref(), Some("qwen"));
	assert_eq!(
		guess("deepseek-ai/deepseek-r1").as_deref(),
		Some("deepseek")
	);
	assert_eq!(
		guess("ghcr.io/ggml-org/llama.cpp:server").as_deref(),
		Some("huggingface")
	);
	assert_eq!(
		guess("registry.example.com/team/some-unknown-thing:1"),
		None
	);
	assert_eq!(guess(""), None);
	assert!(
		crate::logos::for_image("postgres:18")
			.unwrap()
			.starts_with("/static/logos/postgres.")
	);
}

// ------------------------------------------------------------ the API

struct Setup {
	app: Router,
	state: crate::state::AppState,
	db: PgPool,
	token: String,
	team: i64,
	project: i64,
}

impl Setup {
	async fn new(db: PgPool) -> Self {
		Self::with_state(db.clone(), state(db)).await
	}

	async fn with_state(db: PgPool, state: crate::state::AppState) -> Self {
		let app = crate::routes::routes(state.clone()).await.unwrap();
		let (token, team) = user_with_team(&app, "alice").await;
		let (s, project) = call(
			&app,
			"POST",
			&format!("/api/v1/teams/{team}/projects"),
			Some(&token),
			Some(json!({ "name": "web", "slug": "web" })),
		)
		.await;
		assert!(s.is_success(), "create project: {s} {project}");
		Self {
			app,
			state,
			db,
			token,
			team,
			project: project["id"].as_i64().unwrap(),
		}
	}

	async fn get(&self, path: &str) -> (StatusCode, Value) {
		call(&self.app, "GET", path, Some(&self.token), None).await
	}

	async fn server(&self) -> i64 {
		server_id(&self.db, self.team).await
	}

	fn deploy_path(&self, id: &str) -> String {
		format!(
			"/api/v1/teams/{}/projects/{}/templates/{id}/deploy",
			self.team, self.project
		)
	}

	async fn deploy(&self, id: &str, extra: Value) -> (StatusCode, Value) {
		let mut body = json!({ "server_id": self.server().await });
		for (k, v) in extra.as_object().unwrap() {
			body[k] = v.clone();
		}
		call(
			&self.app,
			"POST",
			&self.deploy_path(id),
			Some(&self.token),
			Some(body),
		)
		.await
	}
}

#[sqlx::test]
async fn the_list_can_be_searched_and_filtered_and_needs_a_login(db: PgPool) {
	let s = Setup::new(db).await;
	let (status, _) = call(&s.app, "GET", "/api/v1/templates", None, None).await;
	assert!(status.is_client_error(), "{status}");

	let (status, all) = s.get("/api/v1/templates").await;
	assert_eq!(status, StatusCode::OK);
	assert_eq!(
		all["templates"].as_array().unwrap().len(),
		load_all().templates.len()
	);
	let sum: i64 = all["categories"]
		.as_array()
		.unwrap()
		.iter()
		.map(|c| c["count"].as_i64().unwrap())
		.sum();
	assert_eq!(
		usize::try_from(sum).unwrap(),
		load_all().templates.len(),
		"every template is in one category"
	);

	let (_, found) = s.get("/api/v1/templates?q=n8n").await;
	let first = &found["templates"][0];
	assert_eq!(first["id"], "n8n");
	assert_eq!(first["kind"], "application");
	assert_eq!(first["gateway"], true);
	assert!(
		first["logo_url"]
			.as_str()
			.unwrap()
			.starts_with("/static/logos/")
	);
	assert_eq!(first["category"], "automation");

	let (_, ai) = s.get("/api/v1/templates?category=ai").await;
	let ids: Vec<&str> = ai["templates"]
		.as_array()
		.unwrap()
		.iter()
		.map(|t| t["id"].as_str().unwrap())
		.collect();
	assert!(ids.contains(&"openclaw") && !ids.contains(&"n8n"));
	let (_, both) = s.get("/api/v1/templates?q=n8n&category=ai").await;
	assert!(
		both["templates"]
			.as_array()
			.unwrap()
			.iter()
			.all(|t| t["id"] != "n8n")
	);
	// The counts follow the query, not the category.
	assert!(
		both["categories"]
			.as_array()
			.unwrap()
			.iter()
			.any(|c| c["category"] == "automation")
	);

	let (status, body) = s.get("/api/v1/templates?category=pets").await;
	assert!(status.is_client_error(), "{status} {body}");
}

#[sqlx::test]
async fn a_template_the_catalog_flags_says_so_with_its_reason(db: PgPool) {
	let s = Setup::new(db).await;
	let (_, flagged) = s.get("/api/v1/templates/bytebase").await;
	assert_eq!(flagged["status"], "broken");
	assert_eq!(
		flagged["status_note"],
		"The image tag it names is no longer published."
	);
	let (_, fine) = s.get("/api/v1/templates/gotify").await;
	assert_eq!(fine["status"], "ok");
	assert_eq!(fine["status_note"], Value::Null);
}

#[sqlx::test]
async fn only_the_instance_administrator_refreshes_the_catalog(db: PgPool) {
	let s = Setup::new(db).await;
	let (bob, _) = user_with_team(&s.app, "bob").await;
	let (status, _) = call(
		&s.app,
		"POST",
		"/api/v1/templates/refresh",
		Some(&bob),
		None,
	)
	.await;
	assert_eq!(status, StatusCode::FORBIDDEN);
	let (status, _) = call(&s.app, "POST", "/api/v1/templates/refresh", None, None).await;
	assert!(status.is_client_error(), "{status}");
}

#[sqlx::test]
async fn a_refresh_that_cannot_reach_the_catalog_keeps_the_one_there(db: PgPool) {
	let s = Setup::new(db).await;
	let before = load_all().templates.len();
	// The test state points the catalog at a port nothing listens on.
	let (status, body) = call(
		&s.app,
		"POST",
		"/api/v1/templates/refresh",
		Some(&s.token),
		None,
	)
	.await;
	assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
	assert_eq!(load_all().templates.len(), before);
	assert!(!s.state.config.data_dir.join("catalog").exists());
}

#[sqlx::test]
async fn the_detail_lists_variables_and_services_and_never_a_generated_value(db: PgPool) {
	let s = Setup::new(db).await;
	let (status, body) = s.get("/api/v1/templates/openclaw").await;
	assert_eq!(status, StatusCode::OK, "{body}");
	assert_eq!(body["name"], "OpenClaw");
	assert_eq!(body["services"][0]["name"], "openclaw");
	assert_eq!(body["services"][0]["domain_port"], 8080);
	let vars = body["variables"].as_array().unwrap();
	let var = |name: &str| {
		vars.iter()
			.find(|v| v["name"] == name)
			.unwrap_or_else(|| panic!("{name}"))
	};
	assert_eq!(var("auth_username")["kind"], "input");
	assert_eq!(var("auth_username")["default"], "admin");
	assert_eq!(var("auth_password")["required"], true);
	assert_eq!(var("auth_password")["secret"], true);
	assert_eq!(var("main_domain")["kind"], "domain");
	assert_eq!(var("gateway_token")["kind"], "generated");
	assert_eq!(var("gateway_token")["secret"], true);

	// Nothing of a deploy is in it, and the same call twice is the same.
	let (_, again) = s.get("/api/v1/templates/openclaw").await;
	assert_eq!(body, again);
	for v in vars {
		assert!(v.get("value").is_none(), "{v}");
	}
	let (_, hermes) = s.get("/api/v1/templates/hermes-agent").await;
	let pw = hermes["variables"]
		.as_array()
		.unwrap()
		.iter()
		.find(|v| v["name"] == "dashboard_password")
		.unwrap();
	assert_eq!(pw["revealed"], true);

	let (status, _) = s.get("/api/v1/templates/no-such-template").await;
	assert_eq!(status, StatusCode::NOT_FOUND);
	let (status, _) = s.get("/api/v1/templates/..%2Fetc").await;
	assert_eq!(status, StatusCode::NOT_FOUND);
}

#[sqlx::test]
async fn generated_secrets_are_never_returned_by_get(db: PgPool) {
	let s = Setup::new(db).await;
	// Every template's detail, rendered as JSON text, holds none of the secrets
	// a render of it makes.
	for (id, l) in &load_all().templates {
		let r = render(
			&l.template,
			&placeholder_inputs(&l.template),
			&place(),
			&mut Os,
		)
		.unwrap();
		let (status, body) = s.get(&format!("/api/v1/templates/{id}")).await;
		assert_eq!(status, StatusCode::OK, "{id}");
		let text = body.to_string();
		for (name, value) in &r.env {
			let secret = l.template.variables.keys().any(|v| {
				r.secrets.contains(v)
					&& l.template
						.config
						.env
						.get(name)
						.is_some_and(|t| t.contains(&format!("${{{v}}}")))
			});
			if secret && value.len() >= 12 {
				assert!(
					!text.contains(value.as_str()),
					"{id}: {name} is in the detail"
				);
			}
		}
	}
}

fn placeholder_inputs(t: &Template) -> BTreeMap<String, String> {
	t.variables
		.iter()
		.filter_map(|(name, v)| match v {
			Variable::Input(i) if i.default.is_none() => {
				Some((name.clone(), "placeholder".to_string()))
			}
			_ => None,
		})
		.collect()
}

#[sqlx::test]
async fn logos_are_served_at_a_hashed_url_with_a_year_of_caching(db: PgPool) {
	let s = Setup::new(db).await;
	let fetch = |uri: String| {
		let app = s.app.clone();
		async move {
			app.oneshot(
				axum::http::Request::builder()
					.uri(uri)
					.body(axum::body::Body::empty())
					.unwrap(),
			)
			.await
			.unwrap()
		}
	};
	let url = crate::logos::url("postgresql").unwrap();
	let res = fetch(url.clone()).await;
	assert_eq!(res.status(), StatusCode::OK);
	let h = res.headers();
	assert_eq!(h["content-type"], "image/svg+xml");
	assert_eq!(h["cache-control"], "public, max-age=31536000, immutable");
	assert_eq!(h["x-content-type-options"], "nosniff");
	assert!(
		h["content-security-policy"]
			.to_str()
			.unwrap()
			.contains("default-src 'none'")
	);
	let body = axum::body::to_bytes(res.into_body(), usize::MAX)
		.await
		.unwrap();
	assert!(body.starts_with(b"<svg") || String::from_utf8_lossy(&body).contains("<svg"));

	// A stale hash is not the file.
	let stale = url.replace(
		&crate::logos::url("postgresql")
			.unwrap()
			.split('.')
			.nth(1)
			.unwrap()
			.to_string(),
		"00000000",
	);
	assert_eq!(fetch(stale).await.status(), StatusCode::NOT_FOUND);
	assert_eq!(
		fetch("/static/logos/postgresql.svg".to_string())
			.await
			.status(),
		StatusCode::NOT_FOUND
	);
	assert_eq!(
		fetch("/static/logos/nope.00000000.svg".to_string())
			.await
			.status(),
		StatusCode::NOT_FOUND
	);
	assert_eq!(
		fetch("/static/logos/..%2Fsecrets.00000000.svg".to_string())
			.await
			.status(),
		StatusCode::NOT_FOUND
	);

	// Every template's logo URL answers, own file or registry's.
	for (id, l) in &load_all().templates {
		let res = fetch(crate::logos::template_url(l)).await;
		assert_eq!(res.status(), StatusCode::OK, "{id}");
		let want = if l.logo.media == "image/svg+xml" {
			"image/svg+xml"
		} else {
			"image/png"
		};
		assert_eq!(res.headers()["content-type"], want, "{id}");
	}
}

/// What a deploy of `id` left in the database, as one line per fact.
async fn deploy_application_template(s: &Setup) -> Value {
	let (status, body) = s.deploy("gotify", json!({})).await;
	assert_eq!(status, StatusCode::ACCEPTED, "{body}");
	body
}

#[sqlx::test]
async fn a_single_service_template_becomes_an_image_application(db: PgPool) {
	let s = Setup::new(db).await;
	let body = deploy_application_template(&s).await;
	assert_eq!(body["kind"], "application");
	assert_eq!(body["name"], "Gotify");
	let urls = body["urls"].as_array().unwrap();
	assert_eq!(urls.len(), 1);
	// The test configuration serves ingress on its own port; the host is the app's.
	assert!(
		urls[0]
			.as_str()
			.unwrap()
			.contains("gotify.127-0-0-1.sslip.io"),
		"{urls:?}"
	);
	assert_eq!(body["credentials"], json!({}));

	let id = body["id"].as_i64().unwrap();
	let app = crate::applications::find_by_id(&s.db, crate::ids::ApplicationId(id))
		.await
		.unwrap()
		.unwrap();
	assert_eq!(app.source_kind, crate::applications::SourceKind::Image);
	assert_eq!(app.image.as_deref(), Some("gotify/server:2.9.1"));
	assert_eq!(app.container_port, 80);
	assert_eq!(app.slug, "gotify");
	let volumes: Vec<String> =
		sqlx::query_scalar("SELECT mount_path FROM volumes WHERE application_id = $1")
			.bind(id)
			.fetch_all(&s.db)
			.await
			.unwrap();
	assert_eq!(volumes, ["/app/data"]);
	assert_eq!(
		count(&s.db, "deployments").await,
		1,
		"the first deploy is queued"
	);
	assert_eq!(count(&s.db, "stacks").await, 0);

	// A second deploy of the same template does not collide.
	let (status, second) = s.deploy("gotify", json!({})).await;
	assert_eq!(status, StatusCode::ACCEPTED, "{second}");
	assert_eq!(second["name"], "Gotify 2");
	assert!(
		second["urls"][0]
			.as_str()
			.unwrap()
			.contains("gotify-2.127-0-0-1.sslip.io")
	);
	assert_eq!(count(&s.db, "applications").await, 2);
}

#[sqlx::test]
async fn an_application_template_stores_its_environment_encrypted_and_never_returns_it(db: PgPool) {
	let s = Setup::new(db).await;
	let (status, body) = s
		.deploy(
			"openclaw",
			json!({ "inputs": { "auth_password": "hunter2-do-not-leak", "auth_username": "carol" } }),
		)
		.await;
	assert_eq!(status, StatusCode::ACCEPTED, "{body}");
	assert_eq!(body["kind"], "application");
	let id = body["id"].as_i64().unwrap();

	let (status, vars) = s
		.get(&format!(
			"/api/v1/teams/{}/projects/{}/applications/{id}/env-vars",
			s.team, s.project
		))
		.await;
	assert_eq!(status, StatusCode::OK);
	let keys: Vec<&str> = vars
		.as_array()
		.unwrap()
		.iter()
		.map(|v| v["key"].as_str().unwrap())
		.collect();
	for key in [
		"AUTH_USERNAME",
		"AUTH_PASSWORD",
		"OPENCLAW_GATEWAY_TOKEN",
		"OPENCLAW_STATE_DIR",
	] {
		assert!(keys.contains(&key), "{key} in {keys:?}");
	}
	assert!(!vars.to_string().contains("hunter2"));
	assert!(
		!body.to_string().contains("hunter2"),
		"inputs are not echoed"
	);

	// In the database the values are ciphertext.
	let stored: Vec<Vec<u8>> =
		sqlx::query_scalar("SELECT value FROM env_vars WHERE application_id = $1")
			.bind(id)
			.fetch_all(&s.db)
			.await
			.unwrap();
	assert!(!stored.is_empty());
	for blob in &stored {
		assert!(!String::from_utf8_lossy(blob).contains("hunter2"));
	}
	let token = crate::env_vars::list(&s.db, crate::ids::ApplicationId(id))
		.await
		.unwrap()
		.into_iter()
		.find(|v| v.key == "OPENCLAW_GATEWAY_TOKEN")
		.unwrap();
	let plain = s.state.secrets.decrypt(&token.value).unwrap();
	assert_eq!(plain.len(), 32);
	assert!(
		plain
			.bytes()
			.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
	);
	assert!(
		!body.to_string().contains(&plain),
		"a generated secret is not in the response"
	);
	assert_eq!(
		body["credentials"],
		json!({}),
		"openclaw's password is the user's own"
	);
}

/// The two files of a fixture template, as somebody would hand them in.
fn files_of(id: &str) -> Value {
	let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
		.join("templates")
		.join(id);
	json!({
		"template": std::fs::read_to_string(dir.join("template.toml")).unwrap(),
		"compose": std::fs::read_to_string(dir.join("docker-compose.yml")).unwrap(),
	})
}

#[sqlx::test]
async fn an_imported_template_is_previewed_in_full_and_deploys_marked_as_an_import(db: PgPool) {
	let s = Setup::new(db).await;
	let base = format!(
		"/api/v1/teams/{}/projects/{}/templates/import",
		s.team, s.project
	);
	let files = files_of("umami");

	let (status, preview) = call(
		&s.app,
		"POST",
		&format!("{base}/preview"),
		Some(&s.token),
		Some(files.clone()),
	)
	.await;
	assert_eq!(status, StatusCode::OK, "{preview}");
	assert_eq!(preview["official"], false);
	assert_eq!(preview["compose"], files["compose"]);
	assert_eq!(preview["services"].as_array().unwrap().len(), 2);
	assert_eq!(count(&s.db, "stacks").await, 0, "a preview deploys nothing");

	let mut body = files.clone();
	body["server_id"] = json!(s.server().await);
	let (status, made) = call(&s.app, "POST", &base, Some(&s.token), Some(body)).await;
	assert_eq!(status, StatusCode::ACCEPTED, "{made}");
	assert_eq!(made["kind"], "stack");
	let (_, stack) = s
		.get(&format!(
			"/api/v1/teams/{}/projects/{}/stacks/{}",
			s.team, s.project, made["id"]
		))
		.await;
	// The file says it is umami, and the catalog has an umami: the result is not that one.
	let source = stack["source"].as_str().unwrap();
	assert_eq!(source, preview["origin"]);
	assert!(source.starts_with("import:umami@"), "{source}");
}

#[sqlx::test]
async fn a_catalog_deploy_is_recorded_with_its_template_and_an_import_is_not(db: PgPool) {
	let s = Setup::new(db).await;
	let (_, app) = s.deploy("gotify", json!({})).await;
	let (_, stack) = s.deploy("umami", json!({})).await;
	let recorded: Vec<(Option<i64>, Option<i64>, String)> = sqlx::query_as(
		"SELECT application_id, stack_id, template_id FROM template_deploys ORDER BY template_id",
	)
	.fetch_all(&s.db)
	.await
	.unwrap();
	assert_eq!(
		recorded,
		vec![
			(app["id"].as_i64(), None, "gotify".to_owned()),
			(None, stack["id"].as_i64(), "umami".to_owned()),
		]
	);

	let mut body = files_of("it-tools");
	body["server_id"] = json!(s.server().await);
	let import = format!(
		"/api/v1/teams/{}/projects/{}/templates/import",
		s.team, s.project
	);
	let (status, _) = call(&s.app, "POST", &import, Some(&s.token), Some(body)).await;
	assert_eq!(status, StatusCode::ACCEPTED);
	assert_eq!(count(&s.db, "template_deploys").await, 2);

	// The fixtures have no catalog revision, so nothing is called newer.
	let id = stack["id"].as_i64().unwrap();
	let origin = super::service::origin(&s.db, super::service::Kind::Stack, id)
		.await
		.unwrap()
		.expect("a catalog deploy has an origin");
	assert_eq!(origin.template.template.id, "umami");
	assert!(!origin.newer);
}

#[sqlx::test]
async fn an_imported_one_service_template_is_a_stack_so_that_it_carries_its_mark(db: PgPool) {
	let s = Setup::new(db).await;
	let (_, from_catalog) = s.deploy("gotify", json!({})).await;
	assert_eq!(from_catalog["kind"], "application");

	let mut body = files_of("gotify");
	body["server_id"] = json!(s.server().await);
	body["name"] = json!("Gotify by hand");
	let (status, made) = call(
		&s.app,
		"POST",
		&format!(
			"/api/v1/teams/{}/projects/{}/templates/import",
			s.team, s.project
		),
		Some(&s.token),
		Some(body),
	)
	.await;
	assert_eq!(status, StatusCode::ACCEPTED, "{made}");
	assert_eq!(made["kind"], "stack");
	let (_, stack) = s
		.get(&format!(
			"/api/v1/teams/{}/projects/{}/stacks/{}",
			s.team, s.project, made["id"]
		))
		.await;
	assert!(
		stack["source"]
			.as_str()
			.unwrap()
			.starts_with("import:gotify@"),
		"{stack}"
	);
}

#[sqlx::test]
async fn hostile_template_files_are_refused_and_not_worked_through(db: PgPool) {
	use std::fmt::Write as _;
	let s = Setup::new(db).await;
	let preview = format!(
		"/api/v1/teams/{}/projects/{}/templates/import/preview",
		s.team, s.project
	);
	let base = "id = \"x\"\nname = \"X\"\ndescription = \"d\"\ncategory = \"other\"\nlogo = \"logo.svg\"\n";
	let compose = "services:\n  x:\n    image: nginx\n";
	// Each variable is a thousand copies of the one before: a terabyte, four levels down.
	let mut grows = format!("{base}[variables]\na0 = \"{}\"\n", "y".repeat(1000));
	for level in 1..4 {
		let copies = format!("${{a{}}}", level - 1).repeat(1000);
		let _ = writeln!(grows, "a{level} = \"{copies}\"");
	}
	// Each variable names the one before twice: two to the sixtieth walks, unless counted once.
	let mut doubles = format!("{base}[variables]\nv0 = \"\"\n");
	for level in 1..=60 {
		let _ = writeln!(doubles, "v{level} = \"${{v{0}}}${{v{0}}}\"", level - 1);
	}
	// Sixty variables, each made from the one before and the first a secret: allowed.
	// Five thousand of them: refused, and at once.
	let chain_of = |length: usize| {
		let mut chain = format!("{base}[variables]\nc0 = \"${{password:8}}\"\n");
		for n in 1..length {
			let _ = writeln!(chain, "c{n} = \"${{c{}}}\"", n - 1);
		}
		chain
	};
	// The same against the order variables are taken in, so each is reached through the
	// one before: a call a link, and without a limit the end of the stack and the process.
	let mut deep = format!("{base}[variables]\nd9999 = \"x\"\n");
	for n in 0..9999 {
		let _ = writeln!(deep, "d{n:04} = \"${{d{:04}}}\"", n + 1);
	}
	let mut ring = format!("{base}[variables]\nr4999 = \"${{r0000}}\"\n");
	for n in 0..4999 {
		let _ = writeln!(ring, "r{n:04} = \"${{r{:04}}}\"", n + 1);
	}
	// One 60 KB value, copied into two thousand settings: 120 MB from a 90 KB file.
	let mut copies = format!(
		"{base}[variables]\nbig = \"{}\"\n[config.env]\n",
		"y".repeat(60_000)
	);
	for n in 0..2000 {
		let _ = writeln!(copies, "E{n} = \"${{big}}\"");
	}
	for (what, template, status) in [
		("a value copied everywhere", copies, StatusCode::BAD_REQUEST),
		("a chain too deep", deep, StatusCode::BAD_REQUEST),
		("a ring", ring, StatusCode::BAD_REQUEST),
		("a value that grows", grows, StatusCode::BAD_REQUEST),
		("a variable walked once", doubles, StatusCode::OK),
		("a long chain", chain_of(60), StatusCode::OK),
		(
			"a chain of thousands",
			chain_of(5000),
			StatusCode::BAD_REQUEST,
		),
	] {
		let started = std::time::Instant::now();
		let (got, body) = call(
			&s.app,
			"POST",
			&preview,
			Some(&s.token),
			Some(json!({ "template": template, "compose": compose })),
		)
		.await;
		assert_eq!(got, status, "{what}: {body}");
		assert!(started.elapsed().as_secs() < 10, "{what} took too long");
		if what == "a long chain" {
			// Secrecy follows the chain to its end, and stops at what is not on it.
			let secret = |name: &str| {
				let all = body["variables"].as_array().unwrap();
				all.iter().find(|v| v["name"] == name).unwrap()["secret"] == true
			};
			assert!(secret("c0") && secret("c59"));
		}
	}

	// Somebody who may not create a stack does not get their files read.
	let (bob, _) = user_with_team(&s.app, "bob").await;
	let (status, _) = call(
		&s.app,
		"POST",
		&preview,
		Some(&bob),
		Some(files_of("umami")),
	)
	.await;
	assert!(
		status == StatusCode::FORBIDDEN || status == StatusCode::NOT_FOUND,
		"{status}"
	);
}

#[test]
fn a_template_says_who_maintains_it_only_with_user_names_and_prints_cleanly() {
	let base = "id = \"x\"\ndescription = \"d\"\ncategory = \"other\"\nlogo = \"logo.svg\"\n";
	let parse = |extra: &str| Template::parse(&format!("{base}{extra}"));
	assert!(parse("name = \"X\"\nmaintainers = [\"octo-cat\"]").is_ok());
	assert!(parse("name = \"X\"\nmaintainers = [\"login/oauth?x=1\"]").is_err());
	assert!(parse("name = \"X\\u001b[2J\"").is_err());
	let long = "s".repeat(51);
	assert!(
		parse(&format!(
			"name = \"X\"\n[[config.domains]]\nserviceName = \"{long}\"\nport = 80\nhost = \"h\""
		))
		.is_err()
	);
}

#[sqlx::test]
async fn an_imported_template_gets_nothing_podkit_keeps_for_the_catalog(db: PgPool) {
	let s = Setup::new(db).await;
	let preview = format!(
		"/api/v1/teams/{}/projects/{}/templates/import/preview",
		s.team, s.project
	);
	for id in ["n8n", "mcp-fetch"] {
		let (status, body) =
			call(&s.app, "POST", &preview, Some(&s.token), Some(files_of(id))).await;
		assert_eq!(status, StatusCode::BAD_REQUEST, "{id}: {body}");
	}
	let (status, body) = call(
		&s.app,
		"POST",
		&preview,
		Some(&s.token),
		Some(json!({ "template": "id = \"x\"", "compose": "services: {}" })),
	)
	.await;
	assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
	let (status, _) = call(&s.app, "POST", &preview, None, Some(files_of("umami"))).await;
	assert!(status.is_client_error(), "{status}");
}

#[sqlx::test]
async fn a_multi_service_template_becomes_a_stack_with_domains_and_encrypted_variables(db: PgPool) {
	let s = Setup::new(db).await;
	let (status, body) = s.deploy("umami", json!({})).await;
	assert_eq!(status, StatusCode::ACCEPTED, "{body}");
	assert_eq!(body["kind"], "stack");
	assert_eq!(body["name"], "Umami");
	assert!(
		body["urls"][0]
			.as_str()
			.unwrap()
			.contains("umami-umami.127-0-0-1.sslip.io"),
		"{body}"
	);
	let id = body["id"].as_i64().unwrap();

	let (status, detail) = s
		.get(&format!(
			"/api/v1/teams/{}/projects/{}/stacks/{id}",
			s.team, s.project
		))
		.await;
	assert_eq!(status, StatusCode::OK, "{detail}");
	assert_eq!(detail["status"], "deploying");
	assert_eq!(detail["source"], "template:umami@v3.4.1");
	let services = detail["detail"]["services"].as_array().unwrap();
	let umami = services.iter().find(|x| x["name"] == "umami").unwrap();
	assert_eq!(umami["domain"], "umami-umami.127-0-0-1.sslip.io");
	let db_service = services.iter().find(|x| x["name"] == "db").unwrap();
	assert_eq!(db_service["domain"], Value::Null);
	assert_eq!(count(&s.db, "jobs").await, 1, "one deploy job");

	// The variables are in the stack, encrypted, with the right shapes.
	let stack = crate::stacks::find_by_id(&s.db, crate::ids::StackId(id))
		.await
		.unwrap()
		.unwrap();
	let env = crate::stacks::decrypt_env(&s.state.secrets, &stack).unwrap();
	assert_eq!(env["DB_PASSWORD"].len(), 32);
	assert!(
		env["DB_PASSWORD"]
			.bytes()
			.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
	);
	let secret = base64::engine::general_purpose::STANDARD
		.decode(&env["APP_SECRET"])
		.unwrap();
	assert_eq!(secret.len(), 64);
	for text in [body.to_string(), detail.to_string()] {
		assert!(!text.contains(&env["DB_PASSWORD"]) && !text.contains(&env["APP_SECRET"]));
	}
	assert!(!String::from_utf8_lossy(&stack.env).contains(&env["DB_PASSWORD"]));
}

#[sqlx::test]
async fn files_a_template_mounts_are_stored_encrypted_and_reach_the_translation(db: PgPool) {
	let s = Setup::new(db).await;
	let all = load_all();
	let (id, l) = all
		.templates
		.iter()
		.find(|(_, l)| {
			!l.template.config.mounts.is_empty()
				&& !l.plain_app
				&& placeholder_inputs(&l.template).is_empty()
		})
		.expect("a template with a mount");
	let (status, body) = s.deploy(id, json!({})).await;
	assert_eq!(status, StatusCode::ACCEPTED, "{id}: {body}");
	let stack = crate::stacks::find_by_id(&s.db, crate::ids::StackId(body["id"].as_i64().unwrap()))
		.await
		.unwrap()
		.unwrap();
	let files = crate::stacks::decrypt_files(&s.state.secrets, &stack).unwrap();
	assert_eq!(files.len(), l.template.config.mounts.len(), "{id}");
	for m in &l.template.config.mounts {
		let key = super::mount_key(&m.file_path).unwrap();
		assert!(files.contains_key(&key), "{id}: {key}");
	}
	assert!(!stack.files.is_empty());
	assert!(
		!String::from_utf8_lossy(&stack.files).contains("services"),
		"encrypted"
	);
}

#[sqlx::test]
async fn domains_the_user_chooses_are_used_and_must_be_free(db: PgPool) {
	let s = Setup::new(db).await;
	let (status, body) = s
		.deploy(
			"umami",
			json!({ "inputs": { "main_domain": "stats.example.com" } }),
		)
		.await;
	assert_eq!(status, StatusCode::ACCEPTED, "{body}");
	assert_eq!(
		body["urls"][0]
			.as_str()
			.unwrap()
			.split("//")
			.nth(1)
			.unwrap()
			.split(':')
			.next()
			.unwrap(),
		"stats.example.com"
	);
	// A second deploy cannot take the same name.
	let (status, body) = s
		.deploy(
			"umami",
			json!({ "name": "Umami two", "inputs": { "main_domain": "stats.example.com" } }),
		)
		.await;
	assert_eq!(status, StatusCode::CONFLICT, "{body}");
	assert_eq!(
		count(&s.db, "stacks").await,
		1,
		"the refused deploy left nothing behind"
	);
	// An application template takes a custom domain through the domains slice.
	let (status, body) = s
		.deploy(
			"gotify",
			json!({ "inputs": { "main_domain": "push.example.com" } }),
		)
		.await;
	assert_eq!(status, StatusCode::ACCEPTED, "{body}");
	let hosts: Vec<String> = sqlx::query_scalar("SELECT hostname FROM custom_domains")
		.fetch_all(&s.db)
		.await
		.unwrap();
	assert_eq!(hosts, ["push.example.com"]);
}

#[sqlx::test]
async fn refused_deploys_leave_nothing_behind(db: PgPool) {
	let s = Setup::new(db).await;
	let mut cases: Vec<(&str, Value, StatusCode)> = vec![
		("no-such-template", json!({}), StatusCode::NOT_FOUND),
		("openclaw", json!({}), StatusCode::UNPROCESSABLE_ENTITY), // auth_password is required
		(
			"openclaw",
			json!({ "inputs": { "auth_password": "x", "nope": "y" } }),
			StatusCode::UNPROCESSABLE_ENTITY,
		),
		(
			"openclaw",
			json!({ "inputs": { "auth_password": "x", "gateway_token": "y" } }),
			StatusCode::UNPROCESSABLE_ENTITY,
		),
		(
			"gotify",
			json!({ "model_id": 1 }),
			StatusCode::UNPROCESSABLE_ENTITY,
		), // not a gateway template
		(
			"umami",
			json!({ "name": "" }),
			StatusCode::UNPROCESSABLE_ENTITY,
		),
		(
			"umami",
			json!({ "inputs": { "main_domain": "no spaces allowed" } }),
			StatusCode::UNPROCESSABLE_ENTITY,
		),
	];
	for (id, extra, want) in cases.drain(..) {
		let (status, body) = s.deploy(id, extra.clone()).await;
		// Validation errors answer with the slice's own status.
		let ok = status == want
			|| (want == StatusCode::UNPROCESSABLE_ENTITY && status == StatusCode::BAD_REQUEST);
		assert!(ok, "{id} {extra}: {status} {body}");
	}
	for table in [
		"applications",
		"stacks",
		"jobs",
		"deployments",
		"env_vars",
		"volumes",
	] {
		assert_eq!(count(&s.db, table).await, 0, "{table}");
	}
	// A model that does not exist takes the half-made application with it.
	let (status, body) = s.deploy("n8n", json!({ "model_id": 4242 })).await;
	assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
	for table in ["applications", "env_vars", "volumes", "deployments"] {
		assert_eq!(
			count(&s.db, table).await,
			0,
			"{table} after the failed link"
		);
	}
}

#[sqlx::test]
async fn other_teams_get_404_and_viewers_and_strangers_are_refused(db: PgPool) {
	let s = Setup::new(db).await;
	let (bob, bob_team) = user_with_team(&s.app, "bob").await;
	let (status, bob_project) = call(
		&s.app,
		"POST",
		&format!("/api/v1/teams/{bob_team}/projects"),
		Some(&bob),
		Some(json!({ "name": "mine", "slug": "mine" })),
	)
	.await;
	assert!(status.is_success());
	let bob_project = bob_project["id"].as_i64().unwrap();
	let bob_server = server_id(&s.db, bob_team).await;
	let alice_server = s.server().await;

	let post = |team: i64, project: i64, server: i64| {
		let app = s.app.clone();
		let bob = bob.clone();
		async move {
			call(
				&app,
				"POST",
				&format!("/api/v1/teams/{team}/projects/{project}/templates/gotify/deploy"),
				Some(&bob),
				Some(json!({ "server_id": server })),
			)
			.await
			.0
		}
	};
	// Alice's project through Bob's team, a server of Alice's, Alice's team.
	assert_eq!(
		post(bob_team, s.project, bob_server).await,
		StatusCode::NOT_FOUND
	);
	assert_eq!(
		post(bob_team, bob_project, alice_server).await,
		StatusCode::NOT_FOUND
	);
	assert_eq!(
		post(s.team, s.project, alice_server).await,
		StatusCode::NOT_FOUND
	);
	assert_eq!(count(&s.db, "applications").await, 0);

	// A viewer reads the gallery and cannot deploy.
	let (carol, _) = user_with_team(&s.app, "carol").await;
	let carol_id = user_id(&s.db, "carol").await;
	sqlx::query("INSERT INTO team_members (id, team_id, user_id, role_id) VALUES ($1, $2, $3, 2)")
		.bind(crypto::generate_id())
		.bind(s.team)
		.bind(carol_id)
		.execute(&s.db)
		.await
		.unwrap();
	let (status, _) = call(&s.app, "GET", "/api/v1/templates", Some(&carol), None).await;
	assert_eq!(status, StatusCode::OK);
	for template in ["gotify", "umami"] {
		let (status, _) = call(
			&s.app,
			"POST",
			&format!(
				"/api/v1/teams/{}/projects/{}/templates/{template}/deploy",
				s.team, s.project
			),
			Some(&carol),
			Some(json!({ "server_id": alice_server })),
		)
		.await;
		assert_eq!(status, StatusCode::FORBIDDEN, "{template}");
	}
	for table in ["applications", "stacks", "jobs", "volumes", "env_vars"] {
		assert_eq!(count(&s.db, table).await, 0, "{table}");
	}
}

#[sqlx::test]
async fn a_revealed_credential_is_in_the_deploy_response_once_and_nowhere_else(db: PgPool) {
	let s = Setup::new(db).await;
	let (status, body) = s.deploy("hermes-agent", json!({})).await;
	assert_eq!(status, StatusCode::ACCEPTED, "{body}");
	let creds = body["credentials"].as_object().unwrap();
	assert_eq!(creds.keys().collect::<Vec<_>>(), ["dashboard_password"]);
	let password = creds["dashboard_password"].as_str().unwrap();
	assert_eq!(password.len(), 32);
	// The other generated secret stays hidden, and the password is not in any read.
	assert!(!body["values"].to_string().contains(password));
	let id = body["id"].as_i64().unwrap();
	let stack_url = format!("/api/v1/teams/{}/projects/{}/stacks", s.team, s.project);
	for path in [
		format!("{stack_url}/{id}"),
		stack_url.clone(),
		"/api/v1/templates/hermes-agent".to_string(),
	] {
		let (_, read) = s.get(&path).await;
		assert!(!read.to_string().contains(password), "{path}");
	}
	let stack = crate::stacks::find_by_id(&s.db, crate::ids::StackId(id))
		.await
		.unwrap()
		.unwrap();
	let env = crate::stacks::decrypt_env(&s.state.secrets, &stack).unwrap();
	assert_eq!(env["HERMES_DASHBOARD_BASIC_AUTH_PASSWORD"], password);
	assert_ne!(env["HERMES_DASHBOARD_BASIC_AUTH_SECRET"], password);
	assert!(
		!body
			.to_string()
			.contains(&env["HERMES_DASHBOARD_BASIC_AUTH_SECRET"])
	);
}

#[test]
fn every_template_can_be_filled_in_with_real_randomness() {
	// Not the counter the checks use: the real thing, for each template.
	for (id, l) in &load_all().templates {
		let r = render(
			&l.template,
			&placeholder_inputs(&l.template),
			&place(),
			&mut Os,
		)
		.unwrap_or_else(|e| panic!("{id}: {e}"));
		for d in &r.domains {
			assert!(crate::validate::hostname(&d.host), "{id}: {}", d.host);
		}
		for (name, secret) in r
			.secrets
			.iter()
			.filter_map(|n| Some((n, l.template.variables.get(n)?)))
		{
			let _ = (name, secret);
		}
	}
}

// ---------------------------------------------------------- live

const LIVE_INGRESS: u16 = 18_090;

/// Runs the queued deploy job of a template deploy through the real agent.
async fn run_deploy_job(s: &Setup, kind: &str, id: i64) -> Result<(), String> {
	use crate::jobs::{self, JobKind};
	use tokio_util::sync::CancellationToken;
	let (job_kind, target) = match kind {
		"application" => (JobKind::Deploy, id),
		_ => (JobKind::StackDeploy, id),
	};
	let job = jobs::latest_for(&s.db, job_kind, target)
		.await
		.map_err(|e| e.to_string())?
		.ok_or("no queued job")?;
	let mut log = s.state.logs.open(job.id).await.map_err(|e| e.to_string())?;
	let result = match job_kind {
		JobKind::Deploy => {
			crate::deployments::run::run(&s.state, &job, &CancellationToken::new(), &mut log).await
		}
		_ => crate::stacks::run::run(&s.state, &job, &CancellationToken::new(), &mut log).await,
	};
	log.finish().await;
	result.map_err(|e| {
		let text = std::fs::read_to_string(s.state.logs.path(job.id)).unwrap_or_default();
		format!("{e:?}\n{text}")
	})
}

async fn answers(host: &str, path: &str, contains: &str) -> Result<String, String> {
	let client = reqwest::Client::new();
	let mut last = String::new();
	for _ in 0..90 {
		tokio::time::sleep(std::time::Duration::from_secs(1)).await;
		match client
			.get(format!("http://127.0.0.1:{LIVE_INGRESS}{path}"))
			.header("Host", host)
			.send()
			.await
		{
			Ok(r) if r.status() == 200 => {
				let text = r.text().await.unwrap_or_default();
				if text.contains(contains) {
					return Ok(text);
				}
				last = format!("200 without {contains:?}: {}", &text[..text.len().min(120)]);
			}
			Ok(r) => last = format!("status {}", r.status()),
			Err(e) => last = e.to_string(),
		}
	}
	Err(format!("{host}{path} never answered: {last}"))
}

async fn podman(args: &[&str]) -> String {
	let program = ["/run/current-system/sw/bin/podman", "/usr/bin/podman"]
		.into_iter()
		.find(|p| std::path::Path::new(p).exists())
		.unwrap_or("podman");
	let out = tokio::process::Command::new(program)
		.args(args)
		.output()
		.await
		.unwrap();
	String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Deploys three real templates through the API, the jobs and the real agent:
/// an image application (gotify), a one-service stack (it-tools) and a stack
/// of two (umami with postgres). Each answers over HTTP through Traefik, then
/// everything is removed.
///
/// `cargo test -p podkit-core --all-features -- --ignored template_live`
#[sqlx::test]
#[ignore = "pulls gotify/server, corentinth/it-tools, umami and postgres:15-alpine (under 1 GB) with a real podman.sock and binds ingress ports 18090 and 18091"]
async fn template_live_three_templates_answer_through_ingress(db: PgPool) {
	let mut state = state(db.clone());
	{
		let config = std::sync::Arc::make_mut(&mut state.config);
		config.ingress_port = LIVE_INGRESS;
		config.https_port = LIVE_INGRESS + 1;
	}
	let podman_api = agent::podman::Podman::connect(&agent::podman::local_socket_path())
		.expect("connect to podman.sock");
	let data_dir = tempfile::tempdir().unwrap().keep();
	let ctx = std::sync::Arc::new(agent::Ctx {
		docker: podman_api.docker().clone(),
		supervisor: agent::supervisor::Supervisor::start(podman_api.clone(), &data_dir).await,
		data_dir,
		podman_socket: agent::podman::local_socket_path(),
		http: reqwest::Client::new(),
	});
	let listener = tokio::net::UnixListener::bind(&state.config.local_agent_socket).unwrap();
	tokio::spawn(async move {
		axum::serve(listener, agent::routes::router(ctx))
			.await
			.unwrap();
	});
	let s = Setup::with_state(db, state).await;

	let mut deployed: Vec<(String, i64)> = Vec::new();
	let outcome: Result<(), String> = async {
		for (template, kind, path, needle) in [
			("gotify", "application", "/", "Gotify"),
			("it-tools", "stack", "/", "IT Tools"),
			("umami", "stack", "/api/heartbeat", "ok"),
		] {
			let (status, body) = s.deploy(template, json!({})).await;
			if status != StatusCode::ACCEPTED {
				return Err(format!("{template}: {status} {body}"));
			}
			assert_eq!(body["kind"], kind, "{template}");
			let id = body["id"].as_i64().unwrap();
			deployed.push((kind.to_string(), id));
			let job_target = if kind == "application" {
				body["deploy_id"].as_i64().unwrap()
			} else {
				id
			};
			run_deploy_job(&s, kind, job_target)
				.await
				.map_err(|e| format!("{template} deploy: {e}"))?;
			let host = body["urls"][0]
				.as_str()
				.unwrap()
				.trim_start_matches("http://")
				.split(':')
				.next()
				.unwrap()
				.to_string();
			answers(&host, path, needle).await?;
			eprintln!("{template}: {host} answers");
		}
		// umami talks to its database by name, so the stack network works.
		let umami = deployed.last().unwrap().1;
		let db_container: String = sqlx::query_scalar(
			"SELECT container_id FROM stack_services WHERE stack_id = $1 AND name = 'db'",
		)
		.bind(umami)
		.fetch_one(&s.db)
		.await
		.unwrap();
		let health = podman(&["inspect", "-f", "{{.State.Health.Status}}", &db_container]).await;
		if health.trim() != "healthy" {
			return Err(format!("umami's database is {health:?}"));
		}
		Ok(())
	}
	.await;

	// Remove everything whatever happened above.
	for (kind, id) in &deployed {
		if kind == "stack" {
			let url = format!(
				"/api/v1/teams/{}/projects/{}/stacks/{id}?purge_volumes=true",
				s.team, s.project
			);
			let (status, _) = call(&s.app, "DELETE", &url, Some(&s.token), None).await;
			assert_eq!(status, StatusCode::ACCEPTED);
			let job = crate::jobs::latest_for(&s.db, crate::jobs::JobKind::StackPurge, *id)
				.await
				.unwrap()
				.unwrap();
			let mut log = s.state.logs.open(job.id).await.unwrap();
			let _ = crate::stacks::run::run(
				&s.state,
				&job,
				&tokio_util::sync::CancellationToken::new(),
				&mut log,
			)
			.await;
			log.finish().await;
		} else {
			let url = format!(
				"/api/v1/teams/{}/projects/{}/applications/{id}",
				s.team, s.project
			);
			let _ = call(&s.app, "DELETE", &url, Some(&s.token), None).await;
			for c in podman_api
				.service_containers(&format!("app-{id}"))
				.await
				.unwrap_or_default()
			{
				if let Some(name) = agent::podman::summary_name(&c) {
					let _ = podman_api.remove(name).await;
				}
			}
		}
	}
	let _ = podman_api
		.remove(agent::ingress::TRAEFIK_CONTAINER_NAME)
		.await;
	outcome.unwrap_or_else(|e| panic!("{e}"));
	let left = podman(&["ps", "-a", "--format", "{{.Names}}"]).await;
	for (kind, id) in &deployed {
		let mine = if kind == "stack" {
			format!("stack-{id}")
		} else {
			format!("app-{id}")
		};
		assert!(
			!left.lines().any(|n| n.contains(&mine)),
			"containers left of {mine}: {left}"
		);
	}
	assert!(!left.contains("podkit-traefik"), "ingress left: {left}");
}
