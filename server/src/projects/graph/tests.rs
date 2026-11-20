//! Graph tests: nodes, edges and groups from real data; the layout (deterministic, no
//! overlaps, on fixtures and on random graphs); the wiring rules; and every wire through the
//! services (permissions, other team's ids, redeploys, secrets never shown).

use axum::Router;
use serde_json::{Value, json};
use time::OffsetDateTime;

use super::layout::{Layout, Rect, layout};
use super::rules::{self, Intent, Step};
use super::{Edge, EdgeKind, Graph, NetRef, Node, NodeKey, Scan, Severity, build, wire};
use crate::auth::permission::{Action, Resource};
use crate::deployments::DeploymentStatus;
use crate::environments::Environment;
use crate::error::ServerError;
use crate::ids::{ApplicationId, EnvironmentId, NetworkId, ProjectId, ServerId, TeamId, UserId};
use crate::projects::Project;
use crate::projects::overview::{ResourceKind, ResourceStatus};
use crate::state::AppState;
use crate::tests::{
	PgPool, call, count, db_test, member_role, revoke_permission, seed_deployment, seed_model,
	state, user_id, user_with_team,
};

// --- fixtures without a database -----------------------------------------------------------

fn env(id: i64, name: &str) -> Environment {
	Environment {
		id: EnvironmentId(id),
		project_id: ProjectId(1),
		name: name.to_owned(),
		slug: name.to_lowercase(),
		position: i32::try_from(id).unwrap(),
		created_at: OffsetDateTime::UNIX_EPOCH,
	}
}

fn net(id: i64, label: &str, auto: bool) -> NetRef {
	NetRef {
		id: NetworkId(id),
		label: label.to_owned(),
		auto,
	}
}

fn node(kind: ResourceKind, id: i64, environment: i64, name: &str) -> Node {
	Node {
		key: NodeKey::new(kind, id),
		name: name.to_owned(),
		detail: String::new(),
		mark: None,
		logo_url: None,
		status: ResourceStatus::Deployment(DeploymentStatus::Running),
		environment: EnvironmentId(environment),
		server: ServerId(1),
		nets: Vec::new(),
		var: (kind == ResourceKind::Database).then(|| "DATABASE_URL".to_owned()),
		env_keys: Vec::new(),
		scan: None,
	}
}

fn app(id: i64, environment: i64, name: &str) -> Node {
	node(ResourceKind::Application, id, environment, name)
}

fn dbn(id: i64, environment: i64, name: &str) -> Node {
	node(ResourceKind::Database, id, environment, name)
}

fn edge(from: &Node, to: &Node, kind: EdgeKind, label: &str) -> Edge {
	Edge {
		from: from.key,
		to: to.key,
		kind,
		label: label.to_owned(),
		network: None,
	}
}

fn graph(envs: &[(i64, &str)], nodes: Vec<Node>, mut edges: Vec<Edge>) -> Graph {
	edges.sort_by(|a, b| (a.from, a.to, a.kind, &a.label).cmp(&(b.from, b.to, b.kind, &b.label)));
	Graph {
		project: Project {
			id: ProjectId(1),
			team_id: TeamId(1),
			name: "shop".to_owned(),
			slug: "shop".to_owned(),
		},
		environments: envs.iter().map(|(id, name)| env(*id, name)).collect(),
		nodes,
		edges,
	}
}

fn a(id: i64) -> NodeKey {
	NodeKey::new(ResourceKind::Application, id)
}
fn d(id: i64) -> NodeKey {
	NodeKey::new(ResourceKind::Database, id)
}
fn m(id: i64) -> NodeKey {
	NodeKey::new(ResourceKind::Model, id)
}
fn s(id: i64) -> NodeKey {
	NodeKey::new(ResourceKind::Stack, id)
}

/// The D1 mockup's project: two environments, a stack, a model, links and two networks.
/// Production: apps 1 (storefront-api) and 2 (checkout-worker), databases 1 and 2, model 1,
/// stack 1. Staging: app 3, database 3.
fn storefront() -> Graph {
	let mut api = app(1, 1, "storefront-api");
	api.scan = Some(Scan {
		level: Severity::Critical,
		count: 1,
	});
	let mut worker = app(2, 1, "checkout-worker");
	worker.nets = vec![net(10, "podkit-env-1", true)];
	let mut pg = dbn(1, 1, "postgres-16");
	pg.nets = vec![net(10, "podkit-env-1", true)];
	let mut redis = dbn(2, 1, "redis-cache");
	redis.var = Some("REDIS_URL".to_owned());
	redis.nets = vec![net(10, "podkit-env-1", true)];
	let qwen = node(ResourceKind::Model, 1, 1, "qwen3-4b");
	let obs = node(ResourceKind::Stack, 1, 1, "observability");
	let mut sapi = app(3, 2, "storefront-api");
	sapi.nets = vec![net(20, "podkit-env-2", true)];
	let mut spg = dbn(3, 2, "postgres-16");
	spg.nets = vec![net(20, "podkit-env-2", true)];
	let edges = vec![
		edge(&api, &qwen, EdgeKind::Gateway, "gateway"),
		edge(&worker, &pg, EdgeKind::Variable, "DATABASE_URL"),
		edge(&worker, &redis, EdgeKind::Variable, "REDIS_URL"),
		edge(&sapi, &spg, EdgeKind::Variable, "DATABASE_URL"),
	];
	graph(
		&[(1, "Production"), (2, "Staging")],
		vec![api, obs, qwen, worker, pg, redis, sapi, spg],
		edges,
	)
}

/// A chain, a diamond and a cycle in one environment.
fn shapes() -> Graph {
	let n: Vec<Node> = (1..=9).map(|i| app(i, 1, &format!("n{i}"))).collect();
	let e = |x: usize, y: usize| edge(&n[x], &n[y], EdgeKind::Variable, "X");
	let edges = vec![
		// chain 1 -> 2 -> 3
		e(0, 1),
		e(1, 2),
		// diamond 4 -> 5, 4 -> 6, 5 -> 7, 6 -> 7
		e(3, 4),
		e(3, 5),
		e(4, 6),
		e(5, 6),
		// cycle 8 -> 9 -> 8
		e(7, 8),
		e(8, 7),
	];
	graph(&[(1, "Production")], n, edges)
}

fn rects(g: &Graph, l: &Layout) -> Vec<(NodeKey, Rect)> {
	g.nodes
		.iter()
		.map(|n| (n.key, l.rect_of(n.key).expect("every node is placed")))
		.collect()
}

/// The invariants of a layout: nodes never overlap, a frame holds exactly its members, an
/// environment frame holds its nodes and no other environment's, and every link is routed.
fn assert_clean(g: &Graph, l: &Layout) {
	let nodes = rects(g, l);
	for (i, (x, rx)) in nodes.iter().enumerate() {
		for (y, ry) in &nodes[i + 1..] {
			assert!(!rx.overlaps(ry), "{x} overlaps {y}: {rx:?} {ry:?}");
		}
	}
	for frame in &l.envs {
		for (key, r) in &nodes {
			let node = g.node(*key).unwrap();
			if node.environment == frame.env {
				assert!(frame.rect.contains(r), "{key} is outside its environment");
			} else {
				assert!(!frame.rect.overlaps(r), "{key} sits in another environment");
			}
		}
	}
	for (i, x) in l.envs.iter().enumerate() {
		for y in &l.envs[i + 1..] {
			assert!(!x.rect.overlaps(&y.rect), "environments overlap");
		}
	}
	for frame in &l.nets {
		for (key, r) in &nodes {
			let node = g.node(*key).unwrap();
			let member = node.environment == frame.env
				&& node.frame_net().map(|n| n.id) == Some(frame.net.id);
			if member {
				assert!(
					frame.rect.contains(r),
					"{key} is outside {}",
					frame.net.label
				);
			} else {
				assert!(
					!frame.rect.overlaps(r),
					"{key} overlaps {}, which it is not in",
					frame.net.label
				);
			}
		}
		for other in &l.nets {
			if other.env == frame.env && other.net.id != frame.net.id {
				assert!(
					!frame.rect.overlaps(&other.rect),
					"two network frames overlap"
				);
			}
		}
	}
	assert_eq!(l.edges.len(), g.edges.len());
	assert!(l.edges.iter().all(|e| e.d.starts_with('M')));
}

// --- keys ---------------------------------------------------------------------------------

#[test]
fn graph_node_keys_round_trip_and_garbage_is_refused() {
	for kind in [
		ResourceKind::Application,
		ResourceKind::Database,
		ResourceKind::Model,
		ResourceKind::Stack,
	] {
		let key = NodeKey::new(kind, 42);
		assert_eq!(NodeKey::parse(&key.to_string()), Some(key));
	}
	assert_eq!(d(7).to_string(), "d-7");
	for bad in [
		"", "a", "a-", "a-0", "a--1", "x-1", "ab-1", "a-1 ", "a-1;b-2", "a-<b>", "-1",
	] {
		assert_eq!(NodeKey::parse(bad), None, "{bad:?}");
	}
}

// --- layout ---------------------------------------------------------------------------------

#[test]
fn graph_layout_puts_a_consumer_left_of_its_provider() {
	let g = storefront();
	let l = layout(&g);
	let x = |key: NodeKey| l.pos[&key].0;
	assert!(x(a(2)) < x(d(1)));
	assert!(x(a(2)) < x(d(2)));
	assert!(x(a(1)) < x(m(1)));
	assert!(x(a(3)) < x(d(3)));
	// Environments sit side by side, the first on the left.
	assert!(l.envs[0].rect.x < l.envs[1].rect.x);
}

#[test]
fn graph_layout_layers_chains_diamonds_and_cycles() {
	let g = shapes();
	let l = layout(&g);
	let col = |i: i64| l.pos[&a(i)].0;
	// A chain is one column per step.
	assert!(col(1) < col(2) && col(2) < col(3));
	// A diamond's tip is right of both of its sides, and they share a column.
	assert_eq!(col(5), col(6));
	assert!(col(4) < col(5) && col(5) < col(7));
	// A cycle is broken by edge order: it still lays out, with one end right of the other.
	assert_ne!(col(8), col(9));
	assert_clean(&g, &l);
}

#[test]
fn graph_layout_is_deterministic_and_a_status_change_moves_nothing() {
	let g = storefront();
	let first = layout(&g);
	assert_eq!(first, layout(&g));
	let mut changed = g;
	changed.nodes[0].status = ResourceStatus::Deployment(DeploymentStatus::Building);
	changed.nodes[4].status = ResourceStatus::Deployment(DeploymentStatus::Failed);
	assert_eq!(
		first,
		layout(&changed),
		"a status change must not move a node"
	);
}

#[test]
fn graph_layout_has_no_overlaps_on_the_fixtures() {
	for g in [storefront(), shapes()] {
		assert_clean(&g, &layout(&g));
	}
	// Networks: two frames in one environment, resources on none in a band of their own.
	let mut g = shapes();
	for (i, n) in g.nodes.iter_mut().enumerate() {
		match i % 3 {
			0 => n.nets = vec![net(1, "front", false)],
			1 => n.nets = vec![net(2, "back", false)],
			_ => {}
		}
	}
	let l = layout(&g);
	assert_eq!(l.nets.len(), 2);
	assert_clean(&g, &l);
}

#[test]
fn graph_layout_stacks_environments_when_side_by_side_would_be_too_wide() {
	// Six columns per environment: far wider than two of them side by side may be.
	let mut nodes = Vec::new();
	let mut edges = Vec::new();
	for env in 1..=2 {
		let first = nodes.len();
		for i in 0..6 {
			nodes.push(app(env * 100 + i, env, &format!("e{env}-{i}")));
		}
		for i in 0..5 {
			edges.push(edge(
				&nodes[first + i],
				&nodes[first + i + 1],
				EdgeKind::Variable,
				"X",
			));
		}
	}
	let g = graph(&[(1, "Production"), (2, "Staging")], nodes, edges);
	let l = layout(&g);
	assert_eq!(
		l.envs[0].rect.x, l.envs[1].rect.x,
		"stacked, not side by side"
	);
	assert!(l.envs[0].rect.y < l.envs[1].rect.y);
	assert_clean(&g, &l);
}

/// A small deterministic generator, so the property test needs no dependency.
struct Lcg(u64);
impl Lcg {
	fn next(&mut self, n: u64) -> u64 {
		self.0 = self
			.0
			.wrapping_mul(6_364_136_223_846_793_005)
			.wrapping_add(1_442_695_040_888_963_407);
		(self.0 >> 33) % n
	}
}

#[test]
fn graph_layout_random_graphs_never_overlap_and_are_stable() {
	let mut rng = Lcg(7);
	for round in 0..40 {
		let envs = 1 + i64::try_from(rng.next(3)).unwrap();
		let total = 1 + i64::try_from(rng.next(100)).unwrap();
		let mut nodes = Vec::new();
		for i in 1..=total {
			let kind = [
				ResourceKind::Application,
				ResourceKind::Database,
				ResourceKind::Model,
				ResourceKind::Stack,
			][usize::try_from(rng.next(4)).unwrap()];
			let environment = 1 + i64::try_from(rng.next(u64::try_from(envs).unwrap())).unwrap();
			let mut n = node(kind, i, environment, &format!("n{i}"));
			match rng.next(4) {
				0 => n.nets = vec![net(1, "one", false)],
				1 => n.nets = vec![net(2, "two", false)],
				2 => n.nets = vec![net(50 + environment, "auto", true)],
				_ => {}
			}
			nodes.push(n);
		}
		let mut edges = Vec::new();
		for _ in 0..rng.next(150) {
			let x = usize::try_from(rng.next(u64::try_from(total).unwrap())).unwrap();
			let y = usize::try_from(rng.next(u64::try_from(total).unwrap())).unwrap();
			if x != y && nodes[x].environment == nodes[y].environment {
				edges.push(edge(&nodes[x], &nodes[y], EdgeKind::Variable, "X"));
			}
		}
		let names: Vec<(i64, String)> = (1..=envs).map(|e| (e, format!("env{e}"))).collect();
		let names: Vec<(i64, &str)> = names.iter().map(|(i, n)| (*i, n.as_str())).collect();
		let g = graph(&names, nodes, edges);
		let l = layout(&g);
		assert_clean(&g, &l);
		assert_eq!(l, layout(&g), "round {round}");
	}
}

// --- rules ----------------------------------------------------------------------------------

fn why(g: &Graph, from: NodeKey, to: NodeKey) -> String {
	rules::check(g, g.node(from).unwrap(), g.node(to).unwrap())
		.expect_err("should be refused")
		.0
}

#[test]
fn graph_rules_say_why_a_wire_is_refused() {
	let g = storefront();
	assert_eq!(why(&g, a(1), a(1)), "Pick a different resource");
	assert_eq!(
		why(&g, m(1), d(1)),
		"A model can only be linked from an app"
	);
	assert_eq!(why(&g, d(1), m(1)), "Only apps can use a model");
	assert_eq!(why(&g, s(1), m(1)), "Only apps can use a model");
	assert_eq!(
		why(&g, a(2), d(1)),
		"Already connected through DATABASE_URL"
	);
	assert_eq!(
		why(&g, a(1), m(1)),
		"Already linked through the model gateway"
	);
	assert_eq!(why(&g, d(1), d(2)), "Already on network podkit-env-1");
	assert_eq!(why(&g, a(1), a(2)), "Already on network podkit-env-1");
	// Another environment, by name.
	assert_eq!(
		why(&g, a(1), d(3)),
		"postgres-16 is in Staging. Wiring stays inside one environment"
	);
}

#[test]
fn graph_rules_refuse_another_server() {
	let mut g = storefront();
	let at = g.nodes.iter().position(|n| n.key == d(2)).unwrap();
	g.nodes[at].server = ServerId(2);
	assert!(why(&g, a(1), d(2)).contains("another server"));
	// The check runs for a model too: the gateway only reaches models on the app's server.
	let at = g.nodes.iter().position(|n| n.key == m(1)).unwrap();
	g.nodes[at].server = ServerId(2);
	assert!(why(&g, a(2), m(1)).contains("another server"));
}

#[test]
fn graph_rules_a_reverse_drop_is_the_same_wire_flipped() {
	let g = storefront();
	let (api, redis) = (g.node(a(1)).unwrap(), g.node(d(2)).unwrap());
	let forward = rules::check(&g, api, redis).unwrap();
	assert_eq!(forward, rules::check(&g, redis, api).unwrap());
	assert_eq!(
		forward,
		Intent::Variable {
			consumer: a(1),
			provider: d(2)
		}
	);
	let (model, worker) = (g.node(m(1)).unwrap(), g.node(a(2)).unwrap());
	assert_eq!(
		rules::check(&g, model, worker).unwrap(),
		Intent::Gateway {
			app: a(2),
			model: m(1)
		}
	);
}

#[test]
fn graph_rules_a_stack_joins_the_environment_network() {
	let mut g = storefront();
	let (stack, pg) = (g.node(s(1)).unwrap(), g.node(d(1)).unwrap());
	let Intent::Network { join, network, .. } = rules::check(&g, stack, pg).unwrap() else {
		panic!("a stack and a database are wired by network");
	};
	assert_eq!(join, vec![s(1)]);
	assert_eq!(network, "podkit-env-1");
	// Once it is on the environment's network there is nothing left to do.
	let at = g.nodes.iter().position(|n| n.key == s(1)).unwrap();
	g.nodes[at].nets = vec![net(10, "podkit-env-1", true)];
	assert_eq!(why(&g, s(1), d(1)), "Already on network podkit-env-1");
}

#[test]
fn graph_rules_targets_are_exactly_what_check_allows() {
	let g = storefront();
	let targets = rules::targets(&g, g.node(s(1)).unwrap());
	assert!(targets.contains(&d(1)) && targets.contains(&a(1)));
	assert!(!targets.contains(&d(3)), "another environment");
	assert!(!targets.contains(&m(1)), "a stack cannot use a model");
	assert!(!targets.contains(&s(1)), "not itself");
	for n in &g.nodes {
		for t in rules::targets(&g, n) {
			assert!(
				rules::check(&g, n, g.node(t).unwrap()).is_ok(),
				"{} -> {t}",
				n.key
			);
		}
	}
}

#[test]
fn graph_variable_name_falls_back_when_the_usual_one_is_taken() {
	let g = storefront();
	let mut consumer = g.node(a(1)).unwrap().clone();
	let pg = g.node(d(1)).unwrap();
	assert_eq!(rules::variable_key(&consumer, pg), "DATABASE_URL");
	consumer.env_keys = vec!["DATABASE_URL".to_owned()];
	assert_eq!(rules::variable_key(&consumer, pg), "POSTGRES_16_URL");
}

#[test]
fn graph_plans_skip_the_redeploy_of_a_resource_that_never_ran() {
	let mut g = storefront();
	let plan = rules::plan(&g, a(1), d(1)).unwrap();
	assert_eq!(
		plan.steps,
		vec![
			Step::SetVariable {
				node: a(1),
				key: "DATABASE_URL".to_owned()
			},
			Step::Redeploy { node: a(1) },
		]
	);
	let at = g.nodes.iter().position(|n| n.key == a(1)).unwrap();
	g.nodes[at].status = ResourceStatus::NotDeployed;
	let plan = rules::plan(&g, a(1), d(1)).unwrap();
	assert_eq!(plan.steps.len(), 1, "applies on its first deploy");
}

#[test]
fn graph_a_plan_names_every_permission_before_it_runs() {
	let g = storefront();
	let needs = rules::plan(&g, a(1), d(1)).unwrap().needs();
	for need in [
		(Action::Read, Resource::Project),
		(Action::Create, Resource::EnvVar),
		(Action::Update, Resource::Database),
		(Action::Create, Resource::Deployment),
	] {
		assert!(needs.contains(&need), "{need:?}");
	}
	// Undoing a link is planned from the link itself, and only if it exists.
	let existing = g
		.edges
		.iter()
		.find(|e| e.kind == EdgeKind::Variable)
		.unwrap();
	assert!(!rules::unplan(&g, existing).unwrap().steps.is_empty());
	let invented = Edge {
		label: "NOPE".to_owned(),
		..existing.clone()
	};
	assert!(rules::unplan(&g, &invented).is_err());
}

// --- from the database ------------------------------------------------------------------------

struct F {
	state: AppState,
	app: Router,
	token: String,
	db: PgPool,
	team: i64,
	project: i64,
	server: i64,
	user: UserId,
}

impl F {
	async fn new(db: PgPool) -> Self {
		let state = state(db.clone());
		let app = crate::routes::routes(state.clone()).await.unwrap();
		let (token, team) = user_with_team(&app, "alice").await;
		let (_, project) = call(
			&app,
			"POST",
			&format!("/api/v1/teams/{team}/projects"),
			Some(&token),
			Some(json!({ "name": "shop", "slug": "shop" })),
		)
		.await;
		let (_, servers) = call(
			&app,
			"GET",
			&format!("/api/v1/teams/{team}/servers"),
			Some(&token),
			None,
		)
		.await;
		let user = UserId(user_id(&db, "alice").await);
		Self {
			state,
			app,
			token,
			team,
			project: project["id"].as_i64().unwrap(),
			server: servers[0]["id"].as_i64().unwrap(),
			user,
			db,
		}
	}

	fn t(&self) -> TeamId {
		TeamId(self.team)
	}

	fn p(&self) -> ProjectId {
		ProjectId(self.project)
	}

	async fn post(&self, rest: &str, body: Value) -> Value {
		let (status, v) = call(
			&self.app,
			"POST",
			&format!(
				"/api/v1/teams/{}/projects/{}/{rest}",
				self.team, self.project
			),
			Some(&self.token),
			Some(body),
		)
		.await;
		assert!(status.is_success(), "{rest}: {status} {v}");
		v
	}

	async fn application(&self, name: &str, environment: Option<i64>) -> i64 {
		let mut body = json!({
			"name": name, "slug": name, "server_id": self.server,
			"repo_url": "https://github.com/acme/x", "git_ref": "main", "container_port": 3000,
		});
		if let Some(e) = environment {
			body["environment_id"] = json!(e);
		}
		self.post("applications", body).await["id"]
			.as_i64()
			.unwrap()
	}

	async fn database(&self, name: &str, environment: Option<i64>) -> i64 {
		let mut body = json!({ "name": name, "engine": "postgres", "server_id": self.server });
		if let Some(e) = environment {
			body["environment_id"] = json!(e);
		}
		self.post("databases", body).await["id"].as_i64().unwrap()
	}

	async fn environment(&self, name: &str) -> i64 {
		self.post(
			"environments",
			json!({ "name": name, "slug": name.to_lowercase() }),
		)
		.await["id"]
			.as_i64()
			.unwrap()
	}

	/// Makes the application look deployed, so wiring it redeploys.
	async fn deployed(&self, application: i64) {
		seed_deployment(&self.db, application, "running").await;
	}

	async fn graph(&self) -> Graph {
		build(&self.state, self.user, self.t(), self.p())
			.await
			.unwrap()
	}

	async fn wire(&self, from: NodeKey, to: NodeKey) -> Result<wire::Applied, ServerError> {
		wire::wire(&self.state, self.user, self.t(), self.p(), from, to).await
	}
}

#[db_test]
async fn graph_is_built_from_the_projects_resources(db: PgPool) {
	let f = F::new(db).await;
	let staging = f.environment("Staging").await;
	let web = f.application("web", None).await;
	let pg = f.database("main", None).await;
	let staged = f.application("web-staging", Some(staging)).await;
	let (_, model) = seed_model(&f.db, f.team, f.project, "qwen", "running", None).await;

	let g = f.graph().await;
	assert_eq!(g.environments.len(), 2, "production and staging");
	let keys: Vec<NodeKey> = g.nodes.iter().map(|n| n.key).collect();
	assert_eq!(keys.len(), 4);
	for key in [a(web), d(pg), a(staged), m(model)] {
		assert!(keys.contains(&key), "{key}");
	}
	let staged_node = g.node(a(staged)).unwrap();
	assert_eq!(
		g.environment(staged_node.environment).unwrap().name,
		"Staging"
	);
	assert_eq!(g.node(d(pg)).unwrap().var.as_deref(), Some("DATABASE_URL"));
	assert!(g.edges.is_empty(), "nothing is linked yet");

	// Wiring draws the links the rules describe.
	f.wire(a(web), d(pg)).await.unwrap();
	f.wire(a(web), m(model)).await.unwrap();
	let g = f.graph().await;
	let links: Vec<(NodeKey, NodeKey, EdgeKind, &str)> = g
		.edges
		.iter()
		.map(|e| (e.from, e.to, e.kind, e.label.as_str()))
		.collect();
	assert_eq!(
		links,
		vec![
			(a(web), d(pg), EdgeKind::Variable, "DATABASE_URL"),
			(a(web), m(model), EdgeKind::Gateway, "gateway"),
		]
	);
	assert_eq!(g.node(a(web)).unwrap().env_keys, vec!["DATABASE_URL"]);
}

#[db_test]
async fn graph_wiring_a_database_writes_the_variable_encrypted_and_redeploys(db: PgPool) {
	let f = F::new(db).await;
	let web = f.application("web", None).await;
	let pg = f.database("main", None).await;
	f.deployed(web).await;
	let before = count(&f.db, "deployments").await;

	let done = f.wire(a(web), d(pg)).await.unwrap();
	assert_eq!(done.redeployed, vec!["web"]);
	assert_eq!(
		count(&f.db, "deployments").await,
		before + 1,
		"a deploy was queued"
	);
	assert_eq!(count(&f.db, "env_vars").await, 1);

	// The stored value is the database's internal URL, sealed with the server's secrets.
	let stored: Vec<u8> = sqlx::query_scalar("SELECT value FROM env_vars")
		.fetch_one(&f.db)
		.await
		.unwrap();
	assert!(!String::from_utf8_lossy(&stored).contains("postgres://"));
	let plain = f.state.secrets.decrypt(&stored).unwrap();
	assert!(plain.starts_with("postgres://"), "{plain}");
	assert!(plain.contains(&super::db_host_marker(pg)), "{plain}");

	// The same wire twice, and the wire the other way round, are refused.
	for (from, to) in [(a(web), d(pg)), (d(pg), a(web))] {
		let err = f.wire(from, to).await.unwrap_err();
		assert!(
			matches!(&err, ServerError::Validation(m) if m == "Already connected through DATABASE_URL"),
			"{err:?}"
		);
	}
	assert_eq!(count(&f.db, "env_vars").await, 1);
}

#[db_test]
async fn graph_wiring_an_app_that_never_ran_only_sets_the_variable(db: PgPool) {
	let f = F::new(db).await;
	let web = f.application("web", None).await;
	let pg = f.database("main", None).await;
	let done = f.wire(a(web), d(pg)).await.unwrap();
	assert!(done.redeployed.is_empty());
	assert_eq!(
		count(&f.db, "deployments").await,
		0,
		"no first deploy as a side effect"
	);
	assert_eq!(count(&f.db, "env_vars").await, 1);
}

#[db_test]
async fn graph_wiring_across_environments_is_refused(db: PgPool) {
	let f = F::new(db).await;
	let staging = f.environment("Staging").await;
	let web = f.application("web", None).await;
	let pg = f.database("staging-db", Some(staging)).await;
	let err = f.wire(a(web), d(pg)).await.unwrap_err();
	assert!(
		matches!(&err, ServerError::Validation(m) if m == "staging-db is in Staging. Wiring stays inside one environment"),
		"{err:?}"
	);
	assert_eq!(count(&f.db, "env_vars").await, 0);
}

#[db_test]
async fn graph_wiring_a_model_links_it_through_the_gateway(db: PgPool) {
	let f = F::new(db).await;
	let web = f.application("web", None).await;
	let (_, model) = seed_model(&f.db, f.team, f.project, "qwen", "running", None).await;
	f.deployed(web).await;
	// Dropped from the model onto the app: the same wire, flipped.
	let done = f.wire(m(model), a(web)).await.unwrap();
	assert_eq!(done.redeployed, vec!["web"]);
	let links = crate::links::list(&f.db, ApplicationId(web)).await.unwrap();
	assert_eq!(links.len(), 1);
	assert_eq!(links[0].model_id.0, model);
	let err = f.wire(a(web), m(model)).await.unwrap_err();
	assert!(
		matches!(&err, ServerError::Validation(text) if text.contains("Already linked")),
		"{err:?}"
	);
}

#[db_test]
async fn graph_unwiring_removes_what_wiring_added(db: PgPool) {
	let f = F::new(db).await;
	let web = f.application("web", None).await;
	let pg = f.database("main", None).await;
	let (_, model) = seed_model(&f.db, f.team, f.project, "qwen", "running", None).await;
	f.wire(a(web), d(pg)).await.unwrap();
	f.wire(a(web), m(model)).await.unwrap();
	let edges = f.graph().await.edges;
	assert_eq!(edges.len(), 2);

	for e in &edges {
		wire::unwire(&f.state, f.user, f.t(), f.p(), e)
			.await
			.unwrap();
	}
	assert!(f.graph().await.edges.is_empty());
	assert_eq!(count(&f.db, "env_vars").await, 0);
	assert!(
		crate::links::list(&f.db, ApplicationId(web))
			.await
			.unwrap()
			.is_empty()
	);
	// A link that is already gone is refused, not silently ignored.
	assert!(
		wire::unwire(&f.state, f.user, f.t(), f.p(), &edges[0])
			.await
			.is_err()
	);
}

#[db_test]
async fn graph_a_missing_permission_stops_the_wire_before_anything_changes(db: PgPool) {
	let f = F::new(db).await;
	let web = f.application("web", None).await;
	let pg = f.database("main", None).await;
	f.deployed(web).await;
	let role = member_role(&f.db, f.team, "alice").await;
	// The last step is the redeploy; without it not even the variable may be written.
	revoke_permission(&f.db, role, "create", "deployment").await;
	let err = f.wire(a(web), d(pg)).await.unwrap_err();
	assert!(matches!(err, ServerError::Forbidden), "{err:?}");
	assert_eq!(
		count(&f.db, "env_vars").await,
		0,
		"nothing was half-applied"
	);

	revoke_permission(&f.db, role, "read", "project").await;
	let err = build(&f.state, f.user, f.t(), f.p()).await.unwrap_err();
	assert!(matches!(err, ServerError::Forbidden), "{err:?}");
}

#[db_test]
async fn graph_another_teams_project_and_resources_are_not_found(db: PgPool) {
	let f = F::new(db).await;
	let web = f.application("web", None).await;
	let pg = f.database("main", None).await;
	let (bob_token, bob_team) = user_with_team(&f.app, "bob").await;
	let (_, bob_project) = call(
		&f.app,
		"POST",
		&format!("/api/v1/teams/{bob_team}/projects"),
		Some(&bob_token),
		Some(json!({ "name": "mine", "slug": "mine" })),
	)
	.await;
	let bob = UserId(user_id(&f.db, "bob").await);
	let bob_team = TeamId(bob_team);
	let bob_project = ProjectId(bob_project["id"].as_i64().unwrap());

	// Alice's project through Bob's team is not there.
	let err = build(&f.state, bob, bob_team, f.p()).await.unwrap_err();
	assert!(matches!(err, ServerError::ProjectNotFound), "{err:?}");
	// Alice's resources named inside Bob's own project are not in its graph: 404, no change.
	let err = wire::wire(&f.state, bob, bob_team, bob_project, a(web), d(pg))
		.await
		.unwrap_err();
	assert!(matches!(err, ServerError::NotFound), "{err:?}");
	// Alice's project through Alice's team, by Bob (not a member): refused.
	assert!(
		wire::wire(&f.state, bob, f.t(), f.p(), a(web), d(pg))
			.await
			.is_err()
	);
	assert_eq!(count(&f.db, "env_vars").await, 0);
}
