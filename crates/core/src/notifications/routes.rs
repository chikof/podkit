use axum::extract::{Path, State};
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use super::alerts::MetricAlert;
use super::channel::{Channel, ChannelFields, ChannelKind};
use super::event::{EventKind, Severity};
use super::service::{
	self, AlertInput, ChannelEdit, DeliveryRow, FeedItem, Rule, RuleInput, SmtpInput, TestOutcome,
};
use super::settings::Settings;
use crate::auth::AuthUser;
use crate::error::{AppResult, ServerError};
use crate::ids::{
	MetricAlertId, NotificationChannelId, NotificationDeliveryId, NotificationEventId,
	NotificationRuleId, TeamId,
};
use crate::state::AppState;

pub fn router() -> Router<AppState> {
	Router::new()
		.route("/events", get(event_kinds))
		.route("/channels", get(list_channels).post(create_channel))
		.route(
			"/channels/{channel_id}",
			patch(update_channel).delete(remove_channel),
		)
		.route("/channels/{channel_id}/test", post(test_channel))
		.route("/smtp", get(get_smtp).put(put_smtp).delete(delete_smtp))
		.route("/settings", get(get_settings).put(put_settings))
		.route("/rules", get(list_rules).post(create_rule))
		.route("/rules/{rule_id}", patch(update_rule).delete(remove_rule))
		.route("/alerts", get(list_alerts).post(create_alert))
		.route(
			"/alerts/{alert_id}",
			patch(update_alert).delete(remove_alert),
		)
		.route("/deliveries", get(list_deliveries))
		.route("/deliveries/{delivery_id}/retry", post(retry_delivery))
		.route("/feed", get(feed))
		.route("/feed/read-all", post(read_all))
		.route("/feed/{event_id}/read", post(read_one))
}

#[derive(Serialize)]
struct EventKindResponse {
	kind: &'static str,
	label: &'static str,
	group: &'static str,
	severity: Severity,
}

/// Every event kind a rule can list.
async fn event_kinds() -> Json<Vec<EventKindResponse>> {
	Json(
		EventKind::ALL
			.into_iter()
			.map(|k| EventKindResponse {
				kind: k.as_str(),
				label: k.label(),
				group: k.group(),
				severity: k.severity(),
			})
			.collect(),
	)
}

// --- channels -------------------------------------------------------------------------------

/// No secret in here: not the URL, the token or the signing key. `hint` is the host.
#[derive(Serialize)]
struct ChannelResponse {
	id: NotificationChannelId,
	name: String,
	kind: ChannelKind,
	hint: String,
	enabled: bool,
	last_test_at: Option<String>,
	last_test_ok: Option<bool>,
	last_test_error: Option<String>,
}

impl From<&Channel> for ChannelResponse {
	fn from(c: &Channel) -> Self {
		Self {
			id: c.id,
			name: c.name.clone(),
			kind: c.kind,
			hint: c.hint.clone(),
			enabled: c.enabled,
			last_test_at: c.last_test_at.map(|t| t.to_string()),
			last_test_ok: c.last_test_ok,
			last_test_error: c.last_test_error.clone(),
		}
	}
}

#[derive(Deserialize)]
struct CreateChannelRequest {
	name: String,
	kind: ChannelKind,
	#[serde(flatten)]
	fields: ChannelFields,
}

#[derive(Deserialize)]
struct UpdateChannelRequest {
	#[serde(default)]
	name: Option<String>,
	#[serde(default)]
	enabled: Option<bool>,
	#[serde(flatten)]
	fields: ChannelFields,
}

async fn list_channels(
	State(s): State<AppState>,
	user: AuthUser,
	Path(team): Path<TeamId>,
) -> AppResult<Json<Vec<ChannelResponse>>> {
	let channels = service::channels(&s.db, user.id, team).await?;
	Ok(Json(channels.iter().map(ChannelResponse::from).collect()))
}

async fn create_channel(
	State(s): State<AppState>,
	user: AuthUser,
	Path(team): Path<TeamId>,
	Json(body): Json<CreateChannelRequest>,
) -> AppResult<Json<ChannelResponse>> {
	let channel =
		service::create_channel(&s, user.id, team, &body.name, body.kind, &body.fields).await?;
	Ok(Json(ChannelResponse::from(&channel)))
}

async fn update_channel(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, id)): Path<(TeamId, NotificationChannelId)>,
	Json(body): Json<UpdateChannelRequest>,
) -> AppResult<Json<ChannelResponse>> {
	let edit = ChannelEdit {
		name: body.name,
		enabled: body.enabled,
		fields: Some(body.fields),
	};
	let channel = service::update_channel(&s, user.id, team, id, edit).await?;
	Ok(Json(ChannelResponse::from(&channel)))
}

async fn remove_channel(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, id)): Path<(TeamId, NotificationChannelId)>,
) -> AppResult<Json<serde_json::Value>> {
	service::remove_channel(&s.db, user.id, team, id).await?;
	Ok(Json(serde_json::json!({ "id": id })))
}

#[derive(Serialize)]
struct TestResponse {
	ok: bool,
	message: String,
}

async fn test_channel(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, id)): Path<(TeamId, NotificationChannelId)>,
) -> AppResult<Json<TestResponse>> {
	let TestOutcome { ok, message } = service::test_channel(&s, user.id, team, id).await?;
	Ok(Json(TestResponse { ok, message }))
}

// --- smtp and noise control -----------------------------------------------------------------

/// No password: only whether one is stored.
#[derive(Serialize)]
struct SmtpResponse {
	host: String,
	port: i32,
	security: String,
	username: Option<String>,
	has_password: bool,
	from_addr: String,
}

#[derive(Deserialize)]
struct SmtpRequest {
	host: String,
	port: i32,
	security: String,
	#[serde(default)]
	username: String,
	#[serde(default)]
	password: String,
	from_addr: String,
}

async fn get_smtp(
	State(s): State<AppState>,
	user: AuthUser,
	Path(team): Path<TeamId>,
) -> AppResult<Json<Option<SmtpResponse>>> {
	let smtp = service::smtp(&s.db, user.id, team).await?;
	Ok(Json(smtp.map(|m| SmtpResponse {
		host: m.host,
		port: m.port,
		security: m.security,
		username: m.username,
		has_password: m.password.is_some(),
		from_addr: m.from_addr,
	})))
}

async fn put_smtp(
	State(s): State<AppState>,
	user: AuthUser,
	Path(team): Path<TeamId>,
	Json(body): Json<SmtpRequest>,
) -> AppResult<Json<serde_json::Value>> {
	let input = SmtpInput {
		host: body.host,
		port: body.port,
		security: body.security,
		username: body.username,
		password: body.password,
		from_addr: body.from_addr,
	};
	service::save_smtp(&s, user.id, team, input).await?;
	Ok(Json(serde_json::json!({ "saved": true })))
}

async fn delete_smtp(
	State(s): State<AppState>,
	user: AuthUser,
	Path(team): Path<TeamId>,
) -> AppResult<Json<serde_json::Value>> {
	service::clear_smtp(&s.db, user.id, team).await?;
	Ok(Json(serde_json::json!({ "saved": false })))
}

#[derive(Serialize, Deserialize)]
struct SettingsBody {
	dedup_minutes: i32,
	quiet_enabled: bool,
	quiet_start_min: i32,
	quiet_end_min: i32,
	utc_offset_min: i32,
}

impl From<Settings> for SettingsBody {
	fn from(s: Settings) -> Self {
		Self {
			dedup_minutes: s.dedup_minutes,
			quiet_enabled: s.quiet_enabled,
			quiet_start_min: s.quiet_start_min,
			quiet_end_min: s.quiet_end_min,
			utc_offset_min: s.utc_offset_min,
		}
	}
}

async fn get_settings(
	State(s): State<AppState>,
	user: AuthUser,
	Path(team): Path<TeamId>,
) -> AppResult<Json<SettingsBody>> {
	Ok(Json(service::settings(&s.db, user.id, team).await?.into()))
}

async fn put_settings(
	State(s): State<AppState>,
	user: AuthUser,
	Path(team): Path<TeamId>,
	Json(body): Json<SettingsBody>,
) -> AppResult<Json<SettingsBody>> {
	let new = Settings {
		dedup_minutes: body.dedup_minutes,
		quiet_enabled: body.quiet_enabled,
		quiet_start_min: body.quiet_start_min,
		quiet_end_min: body.quiet_end_min,
		utc_offset_min: body.utc_offset_min,
	};
	service::save_settings(&s.db, user.id, team, &new).await?;
	Ok(Json(new.into()))
}

// --- rules ----------------------------------------------------------------------------------

#[derive(Serialize)]
struct RuleResponse {
	id: NotificationRuleId,
	name: String,
	kinds: Vec<String>,
	channel_ids: Vec<i64>,
	project_id: Option<i64>,
	environment_id: Option<i64>,
	min_severity: Severity,
	enabled: bool,
}

impl From<&Rule> for RuleResponse {
	fn from(r: &Rule) -> Self {
		Self {
			id: r.id,
			name: r.name.clone(),
			kinds: r.kinds.clone(),
			channel_ids: r.channel_ids.clone(),
			project_id: r.project_id.map(|p| p.0),
			environment_id: r.environment_id.map(|e| e.0),
			min_severity: Severity::from_rank(r.min_severity),
			enabled: r.enabled,
		}
	}
}

#[derive(Deserialize)]
struct RuleRequest {
	name: String,
	kinds: Vec<String>,
	channel_ids: Vec<i64>,
	#[serde(default)]
	project_id: Option<i64>,
	#[serde(default)]
	environment_id: Option<i64>,
	#[serde(default = "info")]
	min_severity: Severity,
	#[serde(default = "yes")]
	enabled: bool,
}

fn info() -> Severity {
	Severity::Info
}

fn yes() -> bool {
	true
}

impl From<RuleRequest> for RuleInput {
	fn from(b: RuleRequest) -> Self {
		Self {
			name: b.name,
			kinds: b.kinds,
			channel_ids: b.channel_ids,
			project_id: b.project_id,
			environment_id: b.environment_id,
			min_severity: b.min_severity,
			enabled: b.enabled,
		}
	}
}

async fn list_rules(
	State(s): State<AppState>,
	user: AuthUser,
	Path(team): Path<TeamId>,
) -> AppResult<Json<Vec<RuleResponse>>> {
	let rules = service::rules(&s.db, user.id, team).await?;
	Ok(Json(rules.iter().map(RuleResponse::from).collect()))
}

async fn create_rule(
	State(s): State<AppState>,
	user: AuthUser,
	Path(team): Path<TeamId>,
	Json(body): Json<RuleRequest>,
) -> AppResult<Json<serde_json::Value>> {
	let id = service::create_rule(&s.db, user.id, team, body.into()).await?;
	Ok(Json(serde_json::json!({ "id": id })))
}

async fn update_rule(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, id)): Path<(TeamId, NotificationRuleId)>,
	Json(body): Json<RuleRequest>,
) -> AppResult<Json<serde_json::Value>> {
	service::update_rule(&s.db, user.id, team, id, body.into()).await?;
	Ok(Json(serde_json::json!({ "id": id })))
}

async fn remove_rule(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, id)): Path<(TeamId, NotificationRuleId)>,
) -> AppResult<Json<serde_json::Value>> {
	service::remove_rule(&s.db, user.id, team, id).await?;
	Ok(Json(serde_json::json!({ "id": id })))
}

// --- metric alerts --------------------------------------------------------------------------

#[derive(Serialize)]
struct AlertResponse {
	id: MetricAlertId,
	name: String,
	scope: String,
	server_id: Option<i64>,
	resource_kind: Option<String>,
	resource_id: Option<i64>,
	metric: String,
	threshold: f64,
	duration_minutes: i32,
	severity: Severity,
	enabled: bool,
	firing: bool,
}

impl From<&MetricAlert> for AlertResponse {
	fn from(a: &MetricAlert) -> Self {
		Self {
			id: a.id,
			name: a.name.clone(),
			scope: a.scope.clone(),
			server_id: a.server_id.map(|s| s.0),
			resource_kind: a.resource_kind.clone(),
			resource_id: a.resource_id,
			metric: a.metric.clone(),
			threshold: a.threshold,
			duration_minutes: a.duration_minutes,
			severity: Severity::from_rank(a.severity),
			enabled: a.enabled,
			firing: a.state == "firing",
		}
	}
}

#[derive(Deserialize)]
struct AlertRequest {
	name: String,
	scope: String,
	#[serde(default)]
	server_id: Option<i64>,
	#[serde(default)]
	resource_kind: Option<String>,
	#[serde(default)]
	resource_id: Option<i64>,
	metric: String,
	threshold: f64,
	duration_minutes: i32,
	#[serde(default = "warning")]
	severity: Severity,
	#[serde(default = "yes")]
	enabled: bool,
}

fn warning() -> Severity {
	Severity::Warning
}

#[derive(Deserialize)]
struct AlertEnabledRequest {
	enabled: bool,
}

async fn list_alerts(
	State(s): State<AppState>,
	user: AuthUser,
	Path(team): Path<TeamId>,
) -> AppResult<Json<Vec<AlertResponse>>> {
	let alerts = service::alerts(&s.db, user.id, team).await?;
	Ok(Json(alerts.iter().map(AlertResponse::from).collect()))
}

async fn create_alert(
	State(s): State<AppState>,
	user: AuthUser,
	Path(team): Path<TeamId>,
	Json(body): Json<AlertRequest>,
) -> AppResult<Json<serde_json::Value>> {
	let input = AlertInput {
		name: body.name,
		scope: body.scope,
		server_id: body.server_id,
		resource_kind: body.resource_kind,
		resource_id: body.resource_id,
		metric: body.metric,
		threshold: body.threshold,
		duration_minutes: body.duration_minutes,
		severity: body.severity,
		enabled: body.enabled,
	};
	let id = service::create_alert(&s.db, user.id, team, input).await?;
	Ok(Json(serde_json::json!({ "id": id })))
}

async fn update_alert(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, id)): Path<(TeamId, MetricAlertId)>,
	Json(body): Json<AlertEnabledRequest>,
) -> AppResult<Json<serde_json::Value>> {
	service::set_alert_enabled(&s.db, user.id, team, id, body.enabled).await?;
	Ok(Json(serde_json::json!({ "id": id })))
}

async fn remove_alert(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, id)): Path<(TeamId, MetricAlertId)>,
) -> AppResult<Json<serde_json::Value>> {
	service::remove_alert(&s.db, user.id, team, id).await?;
	Ok(Json(serde_json::json!({ "id": id })))
}

// --- delivery log and the bell ----------------------------------------------------------------

#[derive(Serialize)]
struct DeliveryResponse {
	id: NotificationDeliveryId,
	channel: String,
	title: Option<String>,
	status: String,
	attempts: i32,
	last_error: Option<String>,
	created_at: String,
	sent_at: Option<String>,
}

impl From<&DeliveryRow> for DeliveryResponse {
	fn from(d: &DeliveryRow) -> Self {
		Self {
			id: d.id,
			channel: d.channel_name.clone(),
			title: d.title.clone(),
			status: d.status.clone(),
			attempts: d.attempts,
			last_error: d.last_error.clone(),
			created_at: d.created_at.to_string(),
			sent_at: d.sent_at.map(|t| t.to_string()),
		}
	}
}

#[derive(Serialize)]
struct DeliveriesResponse {
	recent: Vec<DeliveryResponse>,
	dead: Vec<DeliveryResponse>,
}

async fn list_deliveries(
	State(s): State<AppState>,
	user: AuthUser,
	Path(team): Path<TeamId>,
) -> AppResult<Json<DeliveriesResponse>> {
	let (recent, dead) = service::delivery_log(&s.db, user.id, team).await?;
	Ok(Json(DeliveriesResponse {
		recent: recent.iter().map(DeliveryResponse::from).collect(),
		dead: dead.iter().map(DeliveryResponse::from).collect(),
	}))
}

async fn retry_delivery(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, id)): Path<(TeamId, NotificationDeliveryId)>,
) -> AppResult<Json<serde_json::Value>> {
	service::retry_delivery(&s, user.id, team, id).await?;
	Ok(Json(serde_json::json!({ "id": id })))
}

#[derive(Serialize)]
struct FeedResponse {
	unread: i64,
	items: Vec<FeedItemResponse>,
}

#[derive(Serialize)]
struct FeedItemResponse {
	id: NotificationEventId,
	kind: String,
	severity: Severity,
	title: String,
	body: String,
	link: Option<String>,
	count: i32,
	at: String,
	unread: bool,
}

impl From<&FeedItem> for FeedItemResponse {
	fn from(i: &FeedItem) -> Self {
		Self {
			id: i.id,
			kind: i.kind.clone(),
			severity: Severity::from_rank(i.severity),
			title: i.title.clone(),
			body: i.body.clone(),
			link: i.link.clone(),
			count: i.count,
			at: i.last_at.to_string(),
			unread: i.unread,
		}
	}
}

async fn feed(
	State(s): State<AppState>,
	user: AuthUser,
	Path(team): Path<TeamId>,
) -> AppResult<Json<FeedResponse>> {
	let items = service::feed(&s.db, user.id, team, 50).await?;
	let unread = service::unread_count(&s.db, user.id, team)
		.await
		.map_err(ServerError::from)?;
	Ok(Json(FeedResponse {
		unread,
		items: items.iter().map(FeedItemResponse::from).collect(),
	}))
}

async fn read_all(
	State(s): State<AppState>,
	user: AuthUser,
	Path(team): Path<TeamId>,
) -> AppResult<Json<serde_json::Value>> {
	service::mark_all_read(&s.db, user.id, team).await?;
	Ok(Json(serde_json::json!({ "unread": 0 })))
}

async fn read_one(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, id)): Path<(TeamId, NotificationEventId)>,
) -> AppResult<Json<serde_json::Value>> {
	service::mark_read(&s.db, user.id, team, id).await?;
	Ok(Json(serde_json::json!({ "id": id })))
}
