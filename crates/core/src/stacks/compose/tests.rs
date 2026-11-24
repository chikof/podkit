use std::collections::{BTreeMap, BTreeSet};

use proto::{Protocol, Restart};

use super::value::{bytes, dotenv, duration_ms, port};
use super::{Compose, Condition, Inputs, Issue, Mount, interpolate, parse, slug};

fn env(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
	pairs
		.iter()
		.map(|(k, v)| ((*k).to_string(), (*v).to_string()))
		.collect()
}

fn ok(yaml: &str, vars: &[(&str, &str)]) -> Compose {
	let env = env(vars);
	let files = BTreeMap::new();
	parse(
		yaml,
		&Inputs {
			env: &env,
			files: &files,
		},
	)
	.unwrap_or_else(|issues| panic!("refused: {issues:#?}"))
}

fn refused(yaml: &str) -> Vec<Issue> {
	let env = BTreeMap::new();
	let files = BTreeMap::new();
	parse(
		yaml,
		&Inputs {
			env: &env,
			files: &files,
		},
	)
	.expect_err("should be refused")
}

fn paths(issues: &[Issue]) -> Vec<&str> {
	issues.iter().map(|i| i.path.as_str()).collect()
}

fn expand(input: &str, vars: &[(&str, &str)]) -> Result<(String, Vec<String>), String> {
	let mut missing = BTreeSet::new();
	let out = interpolate(input, &env(vars), &mut missing)?;
	Ok((out, missing.into_iter().collect()))
}

#[test]
fn interpolation_forms() {
	let vars = [("A", "1"), ("EMPTY", ""), ("PW", "s3cret")];
	let cases = [
		("$A and ${A}", "1 and 1"),
		("${A:-x}", "1"),
		("${EMPTY:-x}", "x"),
		("${EMPTY-x}", ""),
		("${NOPE:-x}", "x"),
		("${NOPE-x}", "x"),
		("${A:+yes}", "yes"),
		("${EMPTY:+yes}", ""),
		("${EMPTY+yes}", "yes"),
		("${NOPE+yes}", ""),
		("${NOPE:-${A}-z}", "1-z"),
		("price: $$5 and $${A}", "price: $5 and ${A}"),
		(
			"postgres://u:${PW}@db:5432/app",
			"postgres://u:s3cret@db:5432/app",
		),
		("cost $ 5", "cost $ 5"),
		("no variables", "no variables"),
		("héllo ${A}", "héllo 1"),
	];
	for (input, want) in cases {
		let (got, missing) = expand(input, &vars).unwrap();
		assert_eq!(got, want, "{input}");
		assert!(missing.is_empty(), "{input}: {missing:?}");
	}
}

#[test]
fn missing_variables_expand_to_nothing_and_are_reported() {
	let (out, missing) = expand("a=${GONE} b=$ALSO_GONE c=${GONE}", &[]).unwrap();
	assert_eq!(out, "a= b= c=");
	assert_eq!(missing, ["ALSO_GONE", "GONE"]);

	// A default is not a miss.
	let (_, missing) = expand("${GONE:-d}", &[]).unwrap();
	assert!(missing.is_empty());

	let compose = ok(
		"services:\n  web:\n    image: app:${TAG}\n    environment:\n      A: ${A}\n",
		&[("A", "x")],
	);
	assert_eq!(compose.services["web"].image, "app:");
	assert_eq!(compose.missing_vars.iter().collect::<Vec<_>>(), ["TAG"]);
}

#[test]
fn required_variables_and_malformed_expressions_are_errors() {
	let err = expand("${DB_PASSWORD:?set a password}", &[]).unwrap_err();
	assert!(
		err.contains("DB_PASSWORD") && err.contains("set a password"),
		"{err}"
	);
	assert!(expand("${DB_PASSWORD:?}", &[("DB_PASSWORD", "x")]).is_ok());
	assert!(expand("${DB_PASSWORD:?}", &[("DB_PASSWORD", "")]).is_err());
	assert!(expand("${DB_PASSWORD?}", &[("DB_PASSWORD", "")]).is_ok());
	assert!(expand("${unterminated", &[]).is_err());
	assert!(expand("${1BAD}", &[]).is_err());
	assert!(expand("${A%x}", &[("A", "1")]).is_err());

	// In a file the error lands on the key it was written in.
	let issues =
		refused("services:\n  web:\n    image: x\n    environment:\n      PW: ${PW:?needed}\n");
	assert_eq!(paths(&issues), ["services.web.environment.PW"]);
	assert_eq!(issues[0].service.as_deref(), Some("web"));
	assert!(issues[0].message.contains("needed"));
}

#[test]
fn unsupported_keys_are_listed_with_service_and_path() {
	let issues = refused(
		"
services:
  web:
    image: nginx
    build: .
    privileged: true
    network_mode: host
    devices: [\"/dev/dri:/dev/dri\"]
    surprise: 1
    volumes:
      - /var/run/docker.sock:/var/run/docker.sock
      - /srv/data:/data
    depends_on:
      db:
        condition: service_completed_successfully
    deploy:
      replicas: 3
      resources:
        reservations:
          devices:
            - driver: nvidia
  db:
    image: postgres
    cap_add: [SYS_ADMIN, NET_ADMIN]
secrets:
  token:
    file: ./token
",
	);
	let mut got: Vec<(Option<&str>, &str)> = issues
		.iter()
		.map(|i| (i.service.as_deref(), i.path.as_str()))
		.collect();
	got.sort_unstable();
	let mut want = vec![
		(None, "secrets.token.file"),
		(Some("db"), "services.db.cap_add"),
		(Some("web"), "services.web.build"),
		(Some("web"), "services.web.depends_on.db.condition"),
		(Some("web"), "services.web.deploy.replicas"),
		(
			Some("web"),
			"services.web.deploy.resources.reservations.devices",
		),
		(Some("web"), "services.web.devices"),
		(Some("web"), "services.web.network_mode"),
		(Some("web"), "services.web.privileged"),
		(Some("web"), "services.web.surprise"),
		(Some("web"), "services.web.volumes.0"),
		(Some("web"), "services.web.volumes.1"),
	];
	want.sort_unstable();
	assert_eq!(got, want);

	let by_path = |path: &str| issues.iter().find(|i| i.path == path).unwrap();
	assert_eq!(by_path("services.web.build").key, "build");
	assert!(by_path("services.web.volumes.0").message.contains("socket"));
	assert!(
		by_path("services.web.surprise")
			.message
			.contains("unsupported")
	);
	assert!(by_path("services.db.cap_add").message.contains("SYS_ADMIN"));
}

#[test]
fn yaml_that_is_not_a_compose_file_is_refused_once() {
	for text in [
		"services: [",
		"- a\n- b\n",
		"just text",
		"services: {}\n",
		"services:\n  web: 1\n",
	] {
		let issues = refused(text);
		assert!(!issues.is_empty(), "{text:?}");
	}
	let issues = refused("version: '3'\nservicess:\n  web:\n    image: x\n");
	assert_eq!(paths(&issues), ["servicess", "services"]);
}

#[test]
fn ignored_keys_warn_instead_of_refusing() {
	let compose = ok(
		"
x-common: &common
  restart: unless-stopped
services:
  web:
    <<: *common
    image: nginx
    logging: {driver: json-file}
    tty: true
    pull_policy: always
    security_opt: [no-new-privileges]
    labels:
      - traefik.enable=true
      - com.example.team=core
",
		&[],
	);
	let web = &compose.services["web"];
	assert_eq!(
		web.options.restart,
		Restart::Always,
		"merge keys are resolved"
	);
	assert_eq!(
		web.options.labels,
		[("com.example.team".to_string(), "core".to_string())],
		"traefik.* labels are dropped"
	);
	let warned: Vec<&str> = compose.warnings.iter().map(|w| w.path.as_str()).collect();
	for path in [
		"services.web.logging",
		"services.web.tty",
		"services.web.pull_policy",
		"services.web.security_opt",
		"services.web.labels",
	] {
		assert!(warned.contains(&path), "{path} not in {warned:?}");
	}
}

#[test]
fn start_order_follows_depends_on_and_breaks_ties_by_name() {
	let compose = ok(
		"
services:
  web:
    image: web
    depends_on: [api]
  api:
    image: api
    depends_on:
      db: {condition: service_healthy}
      cache: {condition: service_started}
  cache:
    image: redis
  db:
    image: postgres
  admin:
    image: admin
",
		&[],
	);
	assert_eq!(compose.order(), ["admin", "cache", "db", "api", "web"]);
	assert_eq!(
		compose.services["api"].depends_on,
		BTreeMap::from([
			("cache".to_string(), Condition::Started),
			("db".to_string(), Condition::Healthy),
		])
	);
}

#[test]
fn dependency_cycles_are_refused_with_the_cycle() {
	let issues = refused(
		"
services:
  a:
    image: x
    depends_on: [b]
  b:
    image: x
    depends_on: [c]
  c:
    image: x
    depends_on: [a]
  fine:
    image: x
",
	);
	assert_eq!(issues.len(), 1, "{issues:#?}");
	assert!(
		issues[0].path.ends_with(".depends_on"),
		"{}",
		issues[0].path
	);
	for name in ["a", "b", "c"] {
		assert!(
			issues[0].message.contains(&format!("{name} ->"))
				|| issues[0].message.ends_with(&format!("-> {name}")),
			"{}",
			issues[0].message
		);
	}
	assert!(!issues[0].message.contains("fine"));

	let selfish = refused("services:\n  a:\n    image: x\n    depends_on: [a]\n");
	assert!(
		selfish[0].message.contains("a -> a"),
		"{}",
		selfish[0].message
	);

	let unknown = refused("services:\n  a:\n    image: x\n    depends_on: [ghost]\n");
	assert_eq!(paths(&unknown), ["services.a.depends_on.ghost"]);
}

#[test]
#[allow(clippy::too_many_lines, reason = "one table of cases")]
fn services_resolve_to_what_the_translation_needs() {
	let compose = ok(
		r#"
services:
  app:
    image: ghcr.io/acme/app:1.2
    command: sh -c "echo 'hi there' && run"
    entrypoint: ["/entry.sh"]
    environment:
      - MODE=prod
      - FROM_STACK
      - DROPPED
    env_file: .env
    expose: ["8080", 9090]
    ports:
      - "3000"
      - "127.0.0.1:8081:80"
      - "5353:53/udp"
      - target: 443
        published: 8443
    restart: on-failure:3
    user: "1000:1000"
    working_dir: /srv
    hostname: appy
    container_name: the-app
    read_only: true
    init: true
    mem_limit: 512m
    cpus: "1.5"
    shm_size: 64mb
    stop_grace_period: 1m30s
    tmpfs: ["/tmp"]
    extra_hosts: ["metrics:10.0.0.9"]
    ulimits:
      nofile: {soft: 1024, hard: 2048}
      nproc: 65535
    cap_add: [NET_BIND_SERVICE]
    cap_drop: [ALL]
    sysctls: {net.core.somaxconn: 1024}
    dns: 1.1.1.1
    networks:
      back:
        aliases: [api.internal]
    healthcheck:
      test: ["CMD", "curl", "-f", "http://localhost/"]
      interval: 10s
      timeout: 2s
      retries: 3
      start_period: 5s
    volumes:
      - data:/var/lib/app
      - cache:/cache:ro
      - ./uploads:/uploads
      - /scratch
volumes:
  data:
  cache:
"#,
		&[("FROM_STACK", "yes")],
	);
	let app = &compose.services["app"];
	assert_eq!(app.image, "ghcr.io/acme/app:1.2");
	assert_eq!(
		app.command.as_deref(),
		Some(
			&[
				"sh".to_string(),
				"-c".into(),
				"echo 'hi there' && run".into()
			][..]
		)
	);
	assert_eq!(
		app.entrypoint.as_deref(),
		Some(&["/entry.sh".to_string()][..])
	);
	assert_eq!(
		app.env,
		env(&[("MODE", "prod"), ("FROM_STACK", "yes")]),
		"a bare KEY takes the stack's value, or is left out"
	);
	assert_eq!(
		app.expose.iter().copied().collect::<Vec<_>>(),
		[53, 80, 443, 3000, 8080, 9090]
	);
	let ports: Vec<_> = app
		.ports
		.iter()
		.map(|p| (p.host, p.container, p.protocol, p.host_ip.as_deref()))
		.collect();
	assert_eq!(
		ports,
		[
			(8081, 80, Protocol::Tcp, Some("127.0.0.1")),
			(5353, 53, Protocol::Udp, None),
			(8443, 443, Protocol::Tcp, None),
		],
		"a container port alone publishes nothing"
	);
	assert_eq!(app.options.restart, Restart::Always);
	assert_eq!(app.options.user.as_deref(), Some("1000:1000"));
	assert_eq!(app.options.working_dir.as_deref(), Some("/srv"));
	assert!(app.options.read_only && app.options.init);
	assert_eq!(app.memory_bytes, Some(512 * 1024 * 1024));
	assert_eq!(app.cpu_cores, Some(1.5));
	assert_eq!(app.shm_bytes, Some(64 * 1024 * 1024));
	assert_eq!(app.stop_timeout_s, Some(90));
	assert_eq!(app.options.tmpfs, ["/tmp"]);
	assert_eq!(app.options.extra_hosts, ["metrics:10.0.0.9"]);
	assert_eq!(app.options.ulimits.len(), 2);
	assert_eq!(app.options.cap_add, ["NET_BIND_SERVICE"]);
	assert_eq!(app.options.cap_drop, ["ALL"]);
	assert_eq!(
		app.options.sysctls,
		[("net.core.somaxconn".to_string(), "1024".to_string())]
	);
	assert_eq!(app.options.dns, ["1.1.1.1"]);
	assert_eq!(
		app.aliases.iter().map(String::as_str).collect::<Vec<_>>(),
		["api.internal", "appy", "the-app"]
	);
	let check = app.options.healthcheck.as_ref().unwrap();
	assert_eq!(check.test, ["CMD", "curl", "-f", "http://localhost/"]);
	assert_eq!(
		(
			check.interval_ms,
			check.timeout_ms,
			check.retries,
			check.start_period_ms
		),
		(Some(10_000), Some(2_000), Some(3), Some(5_000))
	);

	let mounts: Vec<_> = app
		.mounts
		.iter()
		.map(|m| match m {
			Mount::Volume {
				name,
				target,
				read_only,
			} => (name.as_str(), target.as_str(), *read_only),
			Mount::File { .. } => unreachable!(),
		})
		.collect();
	assert_eq!(
		mounts,
		[
			("data", "/var/lib/app", false),
			("cache", "/cache", true),
			("bind-uploads", "/uploads", false),
			("app-anon-scratch", "/scratch", false),
		]
	);
	assert!(
		compose
			.warnings
			.iter()
			.any(|w| w.path == "services.app.volumes.2"),
		"relative binds warn"
	);
	assert_eq!(
		compose
			.volumes
			.iter()
			.map(String::as_str)
			.collect::<Vec<_>>(),
		["app-anon-scratch", "bind-uploads", "cache", "data"]
	);
}

#[test]
fn template_files_replace_binds_and_env_files() {
	let vars = BTreeMap::new();
	let files = BTreeMap::from([
		("config/app.conf".to_string(), "listen 80;".to_string()),
		("conf.d/a.conf".to_string(), "a".to_string()),
		("conf.d/b.conf".to_string(), "b".to_string()),
		(
			"app.env".to_string(),
			"# c\nA=1\nexport B='two'\n".to_string(),
		),
	]);
	let compose = parse(
		"
services:
  web:
    image: nginx
    env_file: ../files/app.env
    volumes:
      - ../files/config/app.conf:/etc/app.conf:ro
      - ./conf.d:/etc/conf.d
",
		&Inputs {
			env: &vars,
			files: &files,
		},
	)
	.unwrap();
	let web = &compose.services["web"];
	assert_eq!(web.env, env(&[("A", "1"), ("B", "two")]));
	let mut targets: Vec<_> = web
		.mounts
		.iter()
		.map(|m| match m {
			Mount::File { target, content } => (target.clone(), content.clone()),
			Mount::Volume { .. } => unreachable!(),
		})
		.collect();
	targets.sort();
	assert_eq!(
		targets,
		[
			("/etc/app.conf".to_string(), "listen 80;".to_string()),
			("/etc/conf.d/a.conf".to_string(), "a".to_string()),
			("/etc/conf.d/b.conf".to_string(), "b".to_string()),
		]
	);

	// Without the files, a single-file bind has nothing to mount.
	let none = BTreeMap::new();
	let issues = parse(
		"services:\n  web:\n    image: x\n    volumes: ['./nginx.conf:/etc/nginx.conf']\n",
		&Inputs {
			env: &vars,
			files: &none,
		},
	)
	.unwrap_err();
	assert_eq!(paths(&issues), ["services.web.volumes.0"]);
}

#[test]
fn healthchecks_in_all_their_spellings() {
	let compose = ok(
		"
services:
  a:
    image: x
    healthcheck:
      test: pg_isready -U postgres
  b:
    image: x
    healthcheck:
      test: [CMD-SHELL, 'true']
  c:
    image: x
    healthcheck:
      disable: true
  d:
    image: x
    healthcheck:
      test: [NONE]
  e:
    image: x
    healthcheck:
      interval: 5s
",
		&[],
	);
	let test = |name: &str| {
		compose.services[name]
			.options
			.healthcheck
			.as_ref()
			.map(|c| c.test.clone())
	};
	assert_eq!(test("a").unwrap(), ["CMD-SHELL", "pg_isready -U postgres"]);
	assert_eq!(test("b").unwrap(), ["CMD-SHELL", "true"]);
	assert_eq!(test("c").unwrap(), ["NONE"]);
	assert_eq!(test("d").unwrap(), ["NONE"]);
	assert_eq!(test("e"), None, "keeps the image's own check");
}

#[test]
fn scalar_parsers() {
	use serde_json::json;
	assert_eq!(duration_ms(&json!("1m30s")).unwrap(), 90_000);
	assert_eq!(duration_ms(&json!("500ms")).unwrap(), 500);
	assert_eq!(duration_ms(&json!("1h")).unwrap(), 3_600_000);
	assert_eq!(duration_ms(&json!("1.5s")).unwrap(), 1_500);
	assert_eq!(duration_ms(&json!(30)).unwrap(), 30_000);
	assert!(duration_ms(&json!("soon")).is_err());
	assert!(duration_ms(&json!("")).is_err());

	assert_eq!(bytes(&json!("512m")).unwrap(), 512 << 20);
	assert_eq!(bytes(&json!("1.5g")).unwrap(), 3 << 29);
	assert_eq!(bytes(&json!("64MB")).unwrap(), 64 << 20);
	assert_eq!(bytes(&json!(1024)).unwrap(), 1024);
	assert!(bytes(&json!("lots")).is_err());

	let p = |text: &str| {
		port(&json!(text)).map(|p| (p.mapping.host, p.mapping.container, p.unpublished))
	};
	assert_eq!(p("80").unwrap(), (80, 80, true));
	assert_eq!(p("8080:80").unwrap(), (8080, 80, false));
	assert!(p("8000-8010:8000-8010").is_err());
	assert!(p("0:80").is_err());
	assert!(p("http").is_err());

	assert_eq!(
		dotenv("A=1\n\n# x\nB='q'\nnoequals\n"),
		[("A".into(), "1".into()), ("B".into(), "q".into())]
	);
	assert_eq!(slug("My_Service.1"), "my-service-1");
}

#[test]
fn names_that_would_collide_or_break_are_refused() {
	let issues = refused("services:\n  a_b:\n    image: x\n  a-b:\n    image: x\n");
	assert_eq!(issues.len(), 1);
	assert!(issues[0].message.contains("collide"));
	let issues = refused("services:\n  '-bad':\n    image: x\n");
	assert_eq!(paths(&issues), ["services.-bad"]);
}
