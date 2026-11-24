//! Stand-ins for the GitHub, GitLab and Gitea APIs: real axum servers on a loopback port, so
//! the clients run their real HTTP against them. Each records what it was asked.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU16, AtomicUsize, Ordering};

use axum::Json;
use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rsa::{Pkcs1v15Sign, RsaPublicKey};
use serde_json::{Value, json};

use super::jwt;

/// The private key every stubbed GitHub App has.
pub const APP_KEY: &str = include_str!("../ssh_keys/fixtures/rsa_pkcs1");
/// The webhook secret the stubbed GitHub hands out.
pub const GITHUB_WEBHOOK_SECRET: &str = "whsec-stub";
pub const GITLAB_TOKEN: &str = "glpat-good";
pub const GITEA_TOKEN: &str = "gitea-good";

/// A stub that is listening.
pub struct Running<T> {
	/// `http://127.0.0.1:<port>`.
	pub url: String,
	pub state: Arc<T>,
}

async fn serve<T: Send + Sync + 'static>(app: Router, state: Arc<T>) -> Running<T> {
	let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
	let url = format!("http://{}", listener.local_addr().unwrap());
	tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
	Running { url, state }
}

// ---- What the three say about commits ------------------------------------------------------

/// What a stub answers about a commit's checks and changed files, and what was posted to it.
/// Tests set the answers; the endpoints read them. Shared by the three stubs.
#[derive(Default)]
pub struct Commits {
	/// GitHub's check-suites answer. Unset: no suites.
	pub suites: Mutex<Option<Value>>,
	/// GitHub's combined status, GitLab's pipelines, Gitea's combined status. Unset: none.
	pub checks: Mutex<Option<Value>>,
	/// The files every changed-files request answers with.
	pub files: Mutex<Vec<String>>,
	/// Non-zero: the HTTP status the checks requests answer with.
	pub checks_http: AtomicU16,
	/// Non-zero: the HTTP status the changed-files requests answer with.
	pub files_http: AtomicU16,
	/// How often the checks were asked for.
	pub checks_asked: AtomicUsize,
	/// What each changed-files request named (a sha, or `before...after`).
	pub file_reads: Mutex<Vec<String>>,
	/// `(repository@sha, body)` of every commit status posted.
	pub posted: Mutex<Vec<(String, Value)>>,
}

impl Commits {
	fn refuse(code: &AtomicU16) -> Option<Response> {
		match code.load(Ordering::SeqCst) {
			0 => None,
			c => Some(StatusCode::from_u16(c).unwrap().into_response()),
		}
	}

	fn checks_answer(&self, none: impl Fn() -> Value) -> Response {
		self.checks_asked.fetch_add(1, Ordering::SeqCst);
		if let Some(r) = Self::refuse(&self.checks_http) {
			return r;
		}
		Json(self.checks.lock().unwrap().clone().unwrap_or_else(none)).into_response()
	}

	fn suites_answer(&self) -> Response {
		if let Some(r) = Self::refuse(&self.checks_http) {
			return r;
		}
		Json(
			self.suites
				.lock()
				.unwrap()
				.clone()
				.unwrap_or_else(|| json!({ "total_count": 0, "check_suites": [] })),
		)
		.into_response()
	}

	fn files_answer(&self, shape: impl Fn(&[String]) -> Value) -> Response {
		if let Some(r) = Self::refuse(&self.files_http) {
			return r;
		}
		Json(shape(&self.files.lock().unwrap())).into_response()
	}

	fn record_post(&self, key: String, body: Value) -> Response {
		self.posted.lock().unwrap().push((key, body));
		(StatusCode::CREATED, Json(json!({ "id": 1 }))).into_response()
	}
}

fn page_of(q: &HashMap<String, String>, key: &str, default: usize) -> usize {
	q.get(key).and_then(|v| v.parse().ok()).unwrap_or(default)
}

// ---- GitHub -------------------------------------------------------------------------------

#[derive(Default)]
pub struct Github {
	pub commits: Commits,
	pub pulls: Pulls,
	/// Tokens handed out.
	pub minted: AtomicUsize,
	/// Requests whose JWT was a valid RS256 signature of the app's key.
	pub jwt_ok: AtomicUsize,
	pub jwt_bad: AtomicUsize,
	/// Every JWT seen.
	pub jwts: Mutex<Vec<String>>,
	/// Every `(path, bearer)` pair seen with an installation token.
	pub token_calls: Mutex<Vec<(String, String)>>,
}

/// Checks an `Authorization: Bearer <jwt>` the way GitHub does: RS256 over `header.claims`
/// under the app's public key, issued by the app, ten minutes at most.
fn verify_jwt(s: &Github, headers: &HeaderMap) -> bool {
	let ok = (|| {
		let token = headers
			.get(header::AUTHORIZATION)?
			.to_str()
			.ok()?
			.strip_prefix("Bearer ")?;
		s.jwts.lock().unwrap().push(token.to_owned());
		let mut parts = token.split('.');
		let (h, c, sig) = (parts.next()?, parts.next()?, parts.next()?);
		if parts.next().is_some() {
			return None;
		}
		let header: Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(h).ok()?).ok()?;
		let claims: Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(c).ok()?).ok()?;
		if header["alg"] != "RS256" || claims["iss"] != "4242" {
			return None;
		}
		let (iat, exp) = (claims["iat"].as_i64()?, claims["exp"].as_i64()?);
		let now = time::OffsetDateTime::now_utc().unix_timestamp();
		if exp - iat > 10 * 60 || exp < now || iat > now {
			return None;
		}
		let key = RsaPublicKey::from(&jwt::parse_key(APP_KEY).ok()?);
		key.verify(
			Pkcs1v15Sign::new_unprefixed(),
			&jwt::digest_info(&format!("{h}.{c}")),
			&URL_SAFE_NO_PAD.decode(sig).ok()?,
		)
		.ok()
	})()
	.is_some();
	if ok {
		s.jwt_ok.fetch_add(1, Ordering::SeqCst);
	} else {
		s.jwt_bad.fetch_add(1, Ordering::SeqCst);
	}
	ok
}

fn unauthorized() -> Response {
	(
		StatusCode::UNAUTHORIZED,
		Json(json!({ "message": "Bad credentials" })),
	)
		.into_response()
}

fn installation_token(headers: &HeaderMap, path: &str, s: &Github) -> Option<i64> {
	let token = headers
		.get(header::AUTHORIZATION)?
		.to_str()
		.ok()?
		.strip_prefix("Bearer ")?;
	s.token_calls
		.lock()
		.unwrap()
		.push((path.to_owned(), token.to_owned()));
	token.strip_prefix("ghs_")?.split('_').next()?.parse().ok()
}

#[allow(clippy::too_many_lines, reason = "one route table")]
pub async fn github() -> Running<Github> {
	let state = Arc::new(Github::default());
	let app = Router::new()
		.route(
			"/api/v3/app-manifests/{code}/conversions",
			post(|Path(code): Path<String>| async move {
				if code == "good-code" {
					(
						StatusCode::CREATED,
						Json(json!({
							"id": 4242, "slug": "podkit-stub", "name": "podkit-stub",
							"pem": APP_KEY, "webhook_secret": GITHUB_WEBHOOK_SECRET,
							"client_secret": "never-stored",
						})),
					)
						.into_response()
				} else {
					StatusCode::NOT_FOUND.into_response()
				}
			}),
		)
		.route(
			"/api/v3/app",
			get(|State(s): State<Arc<Github>>, h: HeaderMap| async move {
				if !verify_jwt(&s, &h) {
					return unauthorized();
				}
				Json(json!({ "slug": "podkit-stub", "name": "podkit-stub" })).into_response()
			}),
		)
		.route(
			"/api/v3/app/installations",
			get(
				|State(s): State<Arc<Github>>,
				 h: HeaderMap,
				 Query(q): Query<HashMap<String, String>>| async move {
					if !verify_jwt(&s, &h) {
						return unauthorized();
					}
					if page_of(&q, "page", 1) > 1 {
						return Json(json!([])).into_response();
					}
					Json(json!([
						{ "id": 1, "account": { "login": "acme" } },
						{ "id": 2, "account": { "login": "solo" } },
					]))
					.into_response()
				},
			),
		)
		.route(
			"/api/v3/app/installations/{id}/access_tokens",
			post(
				|State(s): State<Arc<Github>>, h: HeaderMap, Path(id): Path<i64>| async move {
					if !verify_jwt(&s, &h) {
						return unauthorized();
					}
					let n = s.minted.fetch_add(1, Ordering::SeqCst);
					(
						StatusCode::CREATED,
						Json(
							json!({ "token": format!("ghs_{id}_{n}"), "expires_at": "2099-01-01T00:00:00Z" }),
						),
					)
						.into_response()
				},
			),
		)
		.route(
			"/api/v3/repos/{owner}/{repo}/installation",
			get(
				|State(s): State<Arc<Github>>,
				 h: HeaderMap,
				 Path((owner, _repo)): Path<(String, String)>| async move {
					if !verify_jwt(&s, &h) {
						return unauthorized();
					}
					match owner.as_str() {
						"acme" => Json(json!({ "id": 1 })).into_response(),
						"solo" => Json(json!({ "id": 2 })).into_response(),
						_ => StatusCode::NOT_FOUND.into_response(),
					}
				},
			),
		)
		.route(
			"/api/v3/installation/repositories",
			get(
				|State(s): State<Arc<Github>>,
				 h: HeaderMap,
				 Query(q): Query<HashMap<String, String>>| async move {
					let Some(installation) =
						installation_token(&h, "/installation/repositories", &s)
					else {
						return unauthorized();
					};
					let page = page_of(&q, "page", 1);
					let names: Vec<String> = match (installation, page) {
						(1, 1) => (1..=100).map(|i| format!("acme/repo-{i:03}")).collect(),
						(1, 2) => (101..=104)
							.map(|i| format!("acme/repo-{i:03}"))
							.chain(["acme/needle-service".to_owned()])
							.collect(),
						(2, 1) => vec!["solo/dockerized".to_owned()],
						_ => Vec::new(),
					};
					let repos: Vec<Value> = names
						.into_iter()
						.map(
							|n| json!({ "full_name": n, "default_branch": "trunk", "private": true }),
						)
						.collect();
					Json(json!({ "total_count": repos.len(), "repositories": repos }))
						.into_response()
				},
			),
		)
		.route(
			"/api/v3/repos/{owner}/{repo}",
			get(|Path((o, r)): Path<(String, String)>| async move {
				Json(json!({ "full_name": format!("{o}/{r}"), "default_branch": "trunk" }))
			}),
		)
		.route(
			"/api/v3/repos/{owner}/{repo}/branches",
			get(|| async { Json(json!([{ "name": "dev" }, { "name": "trunk" }])) }),
		)
		.route(
			"/api/v3/repos/{owner}/{repo}/commits/{sha}/check-suites",
			get(
				|State(s): State<Arc<Github>>,
				 h: HeaderMap,
				 Path((_, _, _)): Path<(String, String, String)>| async move {
					if installation_token(&h, "check-suites", &s).is_none() {
						return unauthorized();
					}
					s.commits.suites_answer()
				},
			),
		)
		.route(
			"/api/v3/repos/{owner}/{repo}/commits/{sha}/status",
			get(
				|State(s): State<Arc<Github>>,
				 h: HeaderMap,
				 Path((_, _, _)): Path<(String, String, String)>| async move {
					if installation_token(&h, "status", &s).is_none() {
						return unauthorized();
					}
					s.commits
						.checks_answer(|| json!({ "state": "pending", "statuses": [] }))
				},
			),
		)
		.route(
			"/api/v3/repos/{owner}/{repo}/compare/{basehead}",
			get(
				|State(s): State<Arc<Github>>,
				 h: HeaderMap,
				 Path((_, _, basehead)): Path<(String, String, String)>| async move {
					if installation_token(&h, "compare", &s).is_none() {
						return unauthorized();
					}
					s.commits.file_reads.lock().unwrap().push(basehead);
					s.commits.files_answer(|files| {
						json!({ "files": files.iter().map(|f| json!({ "filename": f })).collect::<Vec<_>>() })
					})
				},
			),
		)
		.route(
			"/api/v3/repos/{owner}/{repo}/commits/{sha}",
			get(
				|State(s): State<Arc<Github>>,
				 h: HeaderMap,
				 Path((_, _, sha)): Path<(String, String, String)>| async move {
					if installation_token(&h, "commit", &s).is_none() {
						return unauthorized();
					}
					s.commits.file_reads.lock().unwrap().push(sha);
					s.commits.files_answer(|files| {
						json!({ "files": files.iter().map(|f| json!({ "filename": f })).collect::<Vec<_>>() })
					})
				},
			),
		)
		.route(
			"/api/v3/repos/{owner}/{repo}/statuses/{sha}",
			post(
				|State(s): State<Arc<Github>>,
				 h: HeaderMap,
				 Path((o, r, sha)): Path<(String, String, String)>,
				 body: String| async move {
					if installation_token(&h, "statuses", &s).is_none() {
						return unauthorized();
					}
					s.commits.record_post(
						format!("{o}/{r}@{sha}"),
						serde_json::from_str(&body).unwrap_or(Value::Null),
					)
				},
			),
		)
		.route(
			"/api/v3/repos/{owner}/{repo}/pulls/{n}",
			get(|State(s): State<Arc<Github>>, h: HeaderMap| async move {
				if installation_token(&h, "pulls", &s).is_none() {
					return unauthorized();
				}
				s.pulls.hub_pr()
			}),
		)
		.route(
			"/api/v3/repos/{owner}/{repo}/issues/{n}/comments",
			post(
				|State(s): State<Arc<Github>>,
				 h: HeaderMap,
				 Path((o, r, n)): Path<(String, String, i64)>,
				 body: String| async move {
					if installation_token(&h, "comments", &s).is_none() {
						return unauthorized();
					}
					s.pulls.create(format!("{o}/{r}"), n, &body)
				},
			),
		)
		.route(
			"/api/v3/repos/{owner}/{repo}/issues/comments/{id}",
			axum::routing::patch(
				|State(s): State<Arc<Github>>,
				 h: HeaderMap,
				 Path((_, _, id)): Path<(String, String, i64)>,
				 body: String| async move {
					if installation_token(&h, "comments", &s).is_none() {
						return unauthorized();
					}
					s.pulls.edit(id, &body)
				},
			),
		)
		.route(
			"/api/v3/repos/{owner}/{repo}/collaborators/{user}/permission",
			get(
				|State(s): State<Arc<Github>>,
				 h: HeaderMap,
				 Path((_, _, user)): Path<(String, String, String)>| async move {
					if installation_token(&h, "permission", &s).is_none() {
						return unauthorized();
					}
					s.pulls.permission(&user)
				},
			),
		)
		.with_state(state.clone());
	serve(app, state).await
}

// ---- GitLab -------------------------------------------------------------------------------

#[derive(Default)]
pub struct Gitlab {
	pub commits: Commits,
	pub pulls: Pulls,
	/// `(project path, request body)` of each webhook made.
	pub hooks_made: Mutex<Vec<(String, Value)>>,
	pub hooks_deleted: Mutex<Vec<(String, String)>>,
	/// `(page, search)` of each project listing.
	pub listings: Mutex<Vec<(usize, String)>>,
}

fn gitlab_ok(h: &HeaderMap) -> bool {
	h.get("private-token").and_then(|v| v.to_str().ok()) == Some(GITLAB_TOKEN)
}

#[allow(clippy::too_many_lines, reason = "one arm per stubbed endpoint")]
pub async fn gitlab() -> Running<Gitlab> {
	let state = Arc::new(Gitlab::default());
	let app = Router::new()
		.route(
			"/api/v4/user",
			get(|h: HeaderMap| async move {
				if !gitlab_ok(&h) {
					return unauthorized();
				}
				Json(json!({ "username": "root" })).into_response()
			}),
		)
		.route(
			"/api/v4/projects",
			get(
				|State(s): State<Arc<Gitlab>>,
				 h: HeaderMap,
				 Query(q): Query<HashMap<String, String>>| async move {
					if !gitlab_ok(&h) {
						return unauthorized();
					}
					let (page, per) = (page_of(&q, "page", 1), page_of(&q, "per_page", 20));
					let search = q.get("search").cloned().unwrap_or_default();
					s.listings.lock().unwrap().push((page, search.clone()));
					let all: Vec<String> = (1..=45)
						.map(|i| format!("group/proj-{i:02}"))
						.filter(|n| n.contains(&search))
						.collect();
					let rows: Vec<Value> = all
						.iter()
						.skip((page - 1) * per)
						.take(per)
						.map(
							|n| json!({ "path_with_namespace": n, "default_branch": "main", "visibility": "private" }),
						)
						.collect();
					let next = if all.len() > page * per {
						(page + 1).to_string()
					} else {
						String::new()
					};
					([("x-next-page", next)], Json(Value::Array(rows))).into_response()
				},
			),
		)
		.route(
			"/api/v4/projects/{id}/repository/branches",
			get(|h: HeaderMap| async move {
				if !gitlab_ok(&h) {
					return unauthorized();
				}
				Json(
					json!([{ "name": "dev", "default": false }, { "name": "main", "default": true }]),
				)
				.into_response()
			}),
		)
		.route(
			"/api/v4/projects/{id}/hooks",
			post(
				|State(s): State<Arc<Gitlab>>,
				 h: HeaderMap,
				 Path(id): Path<String>,
				 body: String| async move {
					if !gitlab_ok(&h) {
						return unauthorized();
					}
					let body: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
					s.hooks_made.lock().unwrap().push((id, body));
					(StatusCode::CREATED, Json(json!({ "id": 77 }))).into_response()
				},
			),
		)
		.route(
			"/api/v4/projects/{id}/hooks/{hook}",
			delete(
				|State(s): State<Arc<Gitlab>>, Path((id, hook)): Path<(String, String)>| async move {
					s.hooks_deleted.lock().unwrap().push((id, hook));
					StatusCode::NO_CONTENT
				},
			),
		)
		.route(
			"/api/v4/projects/{id}/pipelines",
			get(
				|State(s): State<Arc<Gitlab>>, h: HeaderMap, Path(_): Path<String>| async move {
					if !gitlab_ok(&h) {
						return unauthorized();
					}
					s.commits.checks_answer(|| json!([]))
				},
			),
		)
		.route(
			"/api/v4/projects/{id}/repository/compare",
			get(
				|State(s): State<Arc<Gitlab>>,
				 h: HeaderMap,
				 Path(_): Path<String>,
				 Query(q): Query<HashMap<String, String>>| async move {
					if !gitlab_ok(&h) {
						return unauthorized();
					}
					s.commits.file_reads.lock().unwrap().push(format!("{}..{}", q["from"], q["to"]));
					s.commits.files_answer(|files| {
						json!({ "diffs": files.iter().map(|f| json!({ "old_path": f, "new_path": f })).collect::<Vec<_>>() })
					})
				},
			),
		)
		.route(
			"/api/v4/projects/{id}/repository/commits/{sha}/diff",
			get(
				|State(s): State<Arc<Gitlab>>, h: HeaderMap, Path((_, sha)): Path<(String, String)>| async move {
					if !gitlab_ok(&h) {
						return unauthorized();
					}
					s.commits.file_reads.lock().unwrap().push(sha);
					s.commits.files_answer(|files| {
						json!(files.iter().map(|f| json!({ "old_path": f, "new_path": f })).collect::<Vec<_>>())
					})
				},
			),
		)
		.route(
			"/api/v4/projects/{id}/statuses/{sha}",
			post(
				|State(s): State<Arc<Gitlab>>,
				 h: HeaderMap,
				 Path((id, sha)): Path<(String, String)>,
				 Query(q): Query<HashMap<String, String>>| async move {
					if !gitlab_ok(&h) {
						return unauthorized();
					}
					s.commits.record_post(format!("{id}@{sha}"), json!(q))
				},
			),
		)
		.route(
			"/api/v4/projects/{id}/merge_requests/{n}",
			get(|State(s): State<Arc<Gitlab>>, h: HeaderMap| async move {
				if !gitlab_ok(&h) {
					return unauthorized();
				}
				s.pulls.gitlab_pr()
			}),
		)
		.route(
			"/api/v4/projects/77",
			get(|State(s): State<Arc<Gitlab>>, h: HeaderMap| async move {
				if !gitlab_ok(&h) {
					return unauthorized();
				}
				let head = s.pulls.pr.lock().unwrap().clone().map(|p| p.head_repo);
				Json(json!({ "path_with_namespace": head.unwrap_or_default() })).into_response()
			}),
		)
		.route(
			"/api/v4/projects/{id}/merge_requests/{n}/notes",
			post(
				|State(s): State<Arc<Gitlab>>,
				 h: HeaderMap,
				 Path((id, n)): Path<(String, i64)>,
				 body: String| async move {
					if !gitlab_ok(&h) {
						return unauthorized();
					}
					s.pulls.create(id, n, &body)
				},
			),
		)
		.route(
			"/api/v4/projects/{id}/merge_requests/{n}/notes/{note}",
			axum::routing::put(
				|State(s): State<Arc<Gitlab>>,
				 h: HeaderMap,
				 Path((_, _, note)): Path<(String, i64, i64)>,
				 body: String| async move {
					if !gitlab_ok(&h) {
						return unauthorized();
					}
					s.pulls.edit(note, &body)
				},
			),
		)
		.route(
			"/api/v4/projects/{id}/members/all/{user}",
			get(
				|State(s): State<Arc<Gitlab>>, h: HeaderMap, Path((_, user)): Path<(String, String)>| async move {
					if !gitlab_ok(&h) {
						return unauthorized();
					}
					s.pulls.level(&user)
				},
			),
		)
		.with_state(state.clone());
	serve(app, state).await
}

// ---- Gitea --------------------------------------------------------------------------------

#[derive(Default)]
pub struct Gitea {
	pub commits: Commits,
	pub pulls: Pulls,
	pub hooks_made: Mutex<Vec<(String, Value)>>,
	pub hooks_deleted: Mutex<Vec<(String, String)>>,
}

fn gitea_ok(h: &HeaderMap) -> bool {
	h.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok())
		== Some(format!("token {GITEA_TOKEN}").as_str())
}

#[allow(clippy::too_many_lines, reason = "one arm per stubbed endpoint")]
pub async fn gitea() -> Running<Gitea> {
	let state = Arc::new(Gitea::default());
	let app = Router::new()
		.route(
			"/api/v1/user",
			get(|h: HeaderMap| async move {
				if !gitea_ok(&h) {
					return unauthorized();
				}
				Json(json!({ "login": "ops" })).into_response()
			}),
		)
		.route(
			"/api/v1/repos/search",
			get(
				|h: HeaderMap, Query(q): Query<HashMap<String, String>>| async move {
					if !gitea_ok(&h) {
						return unauthorized();
					}
					let (page, limit) = (page_of(&q, "page", 1), page_of(&q, "limit", 20));
					let needle = q.get("q").cloned().unwrap_or_default();
					let rows: Vec<Value> = (1..=45)
						.map(|i| format!("ops/app-{i:02}"))
						.filter(|n| n.contains(&needle))
						.skip((page - 1) * limit)
						.take(limit)
						.map(
							|n| json!({ "full_name": n, "default_branch": "master", "private": false }),
						)
						.collect();
					Json(json!({ "ok": true, "data": rows })).into_response()
				},
			),
		)
		.route(
			"/api/v1/repos/{owner}/{repo}",
			get(|Path((o, r)): Path<(String, String)>| async move {
				Json(json!({ "full_name": format!("{o}/{r}"), "default_branch": "master" }))
			}),
		)
		.route(
			"/api/v1/repos/{owner}/{repo}/branches",
			get(|| async { Json(json!([{ "name": "feature" }, { "name": "master" }])) }),
		)
		.route(
			"/api/v1/repos/{owner}/{repo}/hooks",
			post(
				|State(s): State<Arc<Gitea>>,
				 h: HeaderMap,
				 Path((o, r)): Path<(String, String)>,
				 body: String| async move {
					if !gitea_ok(&h) {
						return unauthorized();
					}
					let body: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
					s.hooks_made
						.lock()
						.unwrap()
						.push((format!("{o}/{r}"), body));
					(StatusCode::CREATED, Json(json!({ "id": 5 }))).into_response()
				},
			),
		)
		.route(
			"/api/v1/repos/{owner}/{repo}/hooks/{hook}",
			delete(
				|State(s): State<Arc<Gitea>>,
				 Path((o, r, hook)): Path<(String, String, String)>| async move {
					s.hooks_deleted
						.lock()
						.unwrap()
						.push((format!("{o}/{r}"), hook));
					StatusCode::NO_CONTENT
				},
			),
		)
		.route(
			"/api/v1/repos/{owner}/{repo}/commits/{sha}/status",
			get(
				|State(s): State<Arc<Gitea>>, h: HeaderMap, Path((_, _, _)): Path<(String, String, String)>| async move {
					if !gitea_ok(&h) {
						return unauthorized();
					}
					s.commits.checks_answer(|| json!({ "state": "", "total_count": 0, "statuses": [] }))
				},
			),
		)
		.route(
			"/api/v1/repos/{owner}/{repo}/git/commits/{sha}",
			get(
				|State(s): State<Arc<Gitea>>, h: HeaderMap, Path((_, _, sha)): Path<(String, String, String)>| async move {
					if !gitea_ok(&h) {
						return unauthorized();
					}
					s.commits.file_reads.lock().unwrap().push(sha);
					s.commits.files_answer(|files| {
						json!({ "files": files.iter().map(|f| json!({ "filename": f, "status": "modified" })).collect::<Vec<_>>() })
					})
				},
			),
		)
		.route(
			"/api/v1/repos/{owner}/{repo}/statuses/{sha}",
			post(
				|State(s): State<Arc<Gitea>>,
				 h: HeaderMap,
				 Path((o, r, sha)): Path<(String, String, String)>,
				 body: String| async move {
					if !gitea_ok(&h) {
						return unauthorized();
					}
					s.commits.record_post(format!("{o}/{r}@{sha}"), serde_json::from_str(&body).unwrap_or(Value::Null))
				},
			),
		)
		.route(
			"/api/v1/repos/{owner}/{repo}/pulls/{n}",
			get(|State(s): State<Arc<Gitea>>, h: HeaderMap| async move {
				if !gitea_ok(&h) {
					return unauthorized();
				}
				s.pulls.hub_pr()
			}),
		)
		.route(
			"/api/v1/repos/{owner}/{repo}/issues/{n}/comments",
			post(
				|State(s): State<Arc<Gitea>>,
				 h: HeaderMap,
				 Path((o, r, n)): Path<(String, String, i64)>,
				 body: String| async move {
					if !gitea_ok(&h) {
						return unauthorized();
					}
					s.pulls.create(format!("{o}/{r}"), n, &body)
				},
			),
		)
		.route(
			"/api/v1/repos/{owner}/{repo}/issues/comments/{id}",
			axum::routing::patch(
				|State(s): State<Arc<Gitea>>,
				 h: HeaderMap,
				 Path((_, _, id)): Path<(String, String, i64)>,
				 body: String| async move {
					if !gitea_ok(&h) {
						return unauthorized();
					}
					s.pulls.edit(id, &body)
				},
			),
		)
		.route(
			"/api/v1/repos/{owner}/{repo}/collaborators/{user}/permission",
			get(
				|State(s): State<Arc<Gitea>>, h: HeaderMap, Path((_, _, user)): Path<(String, String, String)>| async move {
					if !gitea_ok(&h) {
						return unauthorized();
					}
					s.pulls.permission(&user)
				},
			),
		)
		.with_state(state.clone());
	serve(app, state).await
}

// ---- Pull requests --------------------------------------------------------------------------

/// The pull request a stub answers about.
#[derive(Clone)]
pub struct PrStub {
	pub number: i64,
	pub open: bool,
	pub merged: bool,
	pub title: String,
	pub sha: String,
	pub head_ref: String,
	pub head_repo: String,
	pub base_ref: String,
	pub base_repo: String,
}

/// A comment made in a stub.
#[derive(Clone)]
pub struct StubComment {
	pub id: i64,
	#[allow(dead_code, reason = "what a test may want to look at")]
	pub repo: String,
	pub number: i64,
	pub body: String,
	/// Someone deleted it: editing answers 404.
	pub deleted: bool,
}

/// What the three stubs say about pull requests, comments and who may write.
#[derive(Default)]
pub struct Pulls {
	pub pr: Mutex<Option<PrStub>>,
	pub comments: Mutex<Vec<StubComment>>,
	/// What the permission of a commenter reads: `admin`, `write`, `read`, `none`.
	pub permission: Mutex<String>,
	/// Who was asked about.
	pub asked: Mutex<Vec<String>>,
	next: std::sync::atomic::AtomicI64,
}

impl Pulls {
	fn hub_pr(&self) -> Response {
		match self.pr.lock().unwrap().clone() {
			None => StatusCode::NOT_FOUND.into_response(),
			Some(p) => Json(json!({
				"number": p.number,
				"title": p.title,
				"state": if p.open { "open" } else { "closed" },
				"merged": p.merged,
				"head": { "ref": p.head_ref, "sha": p.sha, "repo": { "full_name": p.head_repo } },
				"base": { "ref": p.base_ref, "repo": { "full_name": p.base_repo } },
			}))
			.into_response(),
		}
	}

	fn gitlab_pr(&self) -> Response {
		match self.pr.lock().unwrap().clone() {
			None => StatusCode::NOT_FOUND.into_response(),
			Some(p) => Json(json!({
				"iid": p.number,
				"title": p.title,
				"state": if p.open { "opened" } else if p.merged { "merged" } else { "closed" },
				"source_branch": p.head_ref,
				"target_branch": p.base_ref,
				"sha": p.sha,
				"source_project_id": 77,
			}))
			.into_response(),
		}
	}

	fn create(&self, repo: String, number: i64, body: &str) -> Response {
		let id = self.next.fetch_add(1, Ordering::SeqCst) + 100;
		let body: Value = serde_json::from_str(body).unwrap_or(Value::Null);
		self.comments.lock().unwrap().push(StubComment {
			id,
			repo,
			number,
			body: body["body"].as_str().unwrap_or_default().to_owned(),
			deleted: false,
		});
		(StatusCode::CREATED, Json(json!({ "id": id }))).into_response()
	}

	fn edit(&self, id: i64, body: &str) -> Response {
		let body: Value = serde_json::from_str(body).unwrap_or(Value::Null);
		let mut all = self.comments.lock().unwrap();
		match all.iter_mut().find(|c| c.id == id && !c.deleted) {
			None => StatusCode::NOT_FOUND.into_response(),
			Some(c) => {
				body["body"]
					.as_str()
					.unwrap_or_default()
					.clone_into(&mut c.body);
				Json(json!({ "id": id })).into_response()
			}
		}
	}

	fn permission(&self, who: &str) -> Response {
		self.asked.lock().unwrap().push(who.to_owned());
		let level = self.permission.lock().unwrap().clone();
		let level = if level.is_empty() {
			"none".to_owned()
		} else {
			level
		};
		Json(json!({ "permission": level })).into_response()
	}

	fn level(&self, who: &str) -> Response {
		self.asked.lock().unwrap().push(who.to_owned());
		let level = match self.permission.lock().unwrap().as_str() {
			"admin" => 50,
			"write" => 30,
			"read" => 20,
			_ => return StatusCode::NOT_FOUND.into_response(),
		};
		Json(json!({ "access_level": level })).into_response()
	}

	/// The comments of pull request `number`, oldest first.
	pub fn bodies(&self, number: i64) -> Vec<String> {
		self.comments
			.lock()
			.unwrap()
			.iter()
			.filter(|c| c.number == number && !c.deleted)
			.map(|c| c.body.clone())
			.collect()
	}
}
