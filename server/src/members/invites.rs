//! Email invites to a team.
//!
//! A member with `create membership` invites an address with a role. The link carries 32
//! random bytes; only their SHA-256 is stored (`team_invites`, migration 0078), so the
//! database cannot hand out a working link and a lost link cannot be shown again: "Resend"
//! makes a new token and the old link stops working. An invite works once (accepting deletes
//! the row in the same transaction that adds the member), lasts [`TTL`], and can be revoked.
//!
//! The link is the permission. It lets its holder create an account for the invited address
//! even when `REGISTRATION` is `closed` or `first-user`, and only for that address; someone
//! who already has an account with that address signs in and joins. A signed-in account with
//! another address cannot use it.
//!
//! The email goes through the team's own SMTP relay (Settings, Notifications), whose address
//! the outbound policy vets at the moment of sending. The link in it is built from
//! `PUBLIC_URL` only, never from the request's `Host`, which a caller can set to anything.
//! Without a relay or without `PUBLIC_URL` nothing is sent, and the person who invited gets
//! the link to pass on themselves.

use sha2::{Digest, Sha256};
use sqlx::PgPool;
use time::{Duration, OffsetDateTime};

use crate::audit::{self, Diff, Target};
use crate::auth::permission::{Action, Resource, authorize};
use crate::auth::{service as accounts, users, validate_email};
use crate::error::{AppResult, ServerError};
use crate::ids::{RoleId, TeamId, TeamInviteId, UserId};
use crate::state::AppState;

/// How long an invite link works.
pub const TTL: Duration = Duration::days(7);
/// Invite emails (new invites and resends) one team may send per hour.
pub const SENDS_PER_HOUR: i64 = 20;
/// Open invites one team may have at a time.
pub const MAX_PENDING: i64 = 100;
/// Random bytes in an invite token.
const TOKEN_BYTES: usize = 32;

fn digest(token: &str) -> Vec<u8> {
	Sha256::digest(token.as_bytes()).to_vec()
}

/// Whether `token` has the shape podkit makes: 64 lowercase hex characters. Anything else is
/// refused before a lookup, so what reaches a page or a URL is always plain hex.
pub fn well_formed(token: &str) -> bool {
	token.len() == TOKEN_BYTES * 2
		&& token
			.bytes()
			.all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// The path an invite link opens, on this site.
pub fn path(token: &str) -> String {
	format!("/invite/{token}")
}

/// An open invite as the Members page and the API list it. Never carries the token.
#[derive(Debug, Clone)]
pub struct Invite {
	pub id: TeamInviteId,
	pub email: String,
	pub role_id: RoleId,
	pub role_name: String,
	/// Who sent it; `None` once their account is gone.
	pub invited_by: Option<String>,
	pub created_at: OffsetDateTime,
	pub expires_at: OffsetDateTime,
}

impl Invite {
	pub fn expired(&self, now: OffsetDateTime) -> bool {
		self.expires_at <= now
	}
}

/// What became of the email of a new or resent invite.
#[derive(Debug, Clone)]
pub struct Issued {
	pub invite: Invite,
	/// The email went out through the team's relay.
	pub emailed: bool,
	/// The link, only when it was not emailed: the person who invited passes it on. Absolute
	/// with `PUBLIC_URL`, else a path on this site.
	pub link: Option<String>,
	/// Why it was not emailed, as a sentence.
	pub note: Option<String>,
}

/// The team's open invites, expired ones included until they are resent or revoked; needs
/// `read membership`.
pub async fn list(db: &PgPool, user: UserId, team: TeamId) -> AppResult<Vec<Invite>> {
	authorize(db, user, team, Action::Read, Resource::Membership).await?;
	Ok(sqlx::query_as!(
		Invite,
		r#"
			SELECT i.id AS "id: TeamInviteId", i.email, i.role_id AS "role_id: RoleId",
				r.name AS role_name, u.name AS "invited_by?", i.created_at, i.expires_at
			FROM team_invites i
			JOIN roles r ON r.id = i.role_id
			LEFT JOIN users u ON u.id = i.invited_by
			WHERE i.team_id = $1
			ORDER BY i.created_at, i.id
		"#,
		team as TeamId
	)
	.fetch_all(db)
	.await?)
}

async fn find(db: impl sqlx::PgExecutor<'_>, team: TeamId, id: TeamInviteId) -> AppResult<Invite> {
	sqlx::query_as!(
		Invite,
		r#"
			SELECT i.id AS "id: TeamInviteId", i.email, i.role_id AS "role_id: RoleId",
				r.name AS role_name, u.name AS "invited_by?", i.created_at, i.expires_at
			FROM team_invites i
			JOIN roles r ON r.id = i.role_id
			LEFT JOIN users u ON u.id = i.invited_by
			WHERE i.team_id = $1 AND i.id = $2
		"#,
		team as TeamId,
		id as TeamInviteId
	)
	.fetch_optional(db)
	.await?
	.ok_or(ServerError::NotFound)
}

/// Holds the team's invite lock for the transaction and refuses once the team has sent
/// [`SENDS_PER_HOUR`] invite emails in the last hour; books one more.
async fn book_send(tx: &mut sqlx::PgConnection, team: TeamId) -> AppResult<()> {
	sqlx::query!(
		"SELECT pg_advisory_xact_lock(hashtextextended('team_invites', $1))",
		team.0
	)
	.execute(&mut *tx)
	.await?;
	let window = sqlx::query!(
		r#"
			SELECT COUNT(*) AS "count!", MIN(at) AS oldest
			FROM team_invite_sends WHERE team_id = $1 AND at > NOW() - INTERVAL '1 hour'
		"#,
		team as TeamId
	)
	.fetch_one(&mut *tx)
	.await?;
	if window.count >= SENDS_PER_HOUR {
		let left = window.oldest.map_or(60, |t| {
			(t + Duration::hours(1) - OffsetDateTime::now_utc()).whole_seconds()
		});
		return Err(ServerError::TooManyAttempts {
			retry_after: u64::try_from(left).unwrap_or(1).max(1),
		});
	}
	sqlx::query!(
		"INSERT INTO team_invite_sends (team_id) VALUES ($1)",
		team as TeamId
	)
	.execute(&mut *tx)
	.await?;
	Ok(())
}

/// Invites `email` to `team` with `role`; needs `create membership`, and the role may not
/// reach further than the caller's own. Sends the email when it can (see the module docs).
///
/// # Errors
/// `Validation` for a bad email or a role wider than the caller's, `RoleNotAllowed` for
/// another team's role, `AlreadyMember`, `Conflict` when the address already has an open
/// invite or the team has too many, `TooManyAttempts` past the hourly limit.
pub async fn create(
	s: &AppState,
	user: UserId,
	team: TeamId,
	email: &str,
	role: RoleId,
) -> AppResult<Issued> {
	authorize(&s.db, user, team, Action::Create, Resource::Membership).await?;
	let email = validate_email(email.trim())
		.map_err(|e| ServerError::Validation(e.to_string()))?
		.normalize()
		// Stored and compared in lower case: one address, one invite, one account.
		.to_lowercase();
	if email.chars().count() > 100 {
		return Err(ServerError::Validation(
			"That email is too long for an account.".to_owned(),
		));
	}
	super::ensure_assignable(&s.db, team, role).await?;
	super::ensure_within(&s.db, user, team, role).await?;
	let member = sqlx::query_scalar!(
		r#"
			SELECT EXISTS (
				SELECT 1 FROM team_members m JOIN users u ON u.id = m.user_id
				WHERE m.team_id = $1 AND lower(u.email) = lower($2)
			) AS "exists!"
		"#,
		team as TeamId,
		email
	)
	.fetch_one(&s.db)
	.await?;
	if member {
		return Err(ServerError::AlreadyMember);
	}

	let token = crypto::generate_token(TOKEN_BYTES);
	let id = TeamInviteId::generate();
	let mut tx = s.db.begin().await?;
	book_send(&mut tx, team).await?;
	// An expired invite for the same address makes room for the new one.
	sqlx::query!(
		"DELETE FROM team_invites WHERE team_id = $1 AND lower(email) = lower($2) AND expires_at <= NOW()",
		team as TeamId,
		email
	)
	.execute(&mut *tx)
	.await?;
	let pending = sqlx::query_scalar!(
		r#"SELECT COUNT(*) AS "count!" FROM team_invites WHERE team_id = $1"#,
		team as TeamId
	)
	.fetch_one(&mut *tx)
	.await?;
	if pending >= MAX_PENDING {
		return Err(ServerError::Conflict(format!(
			"This team already has {MAX_PENDING} open invites; revoke some first."
		)));
	}
	let inserted = sqlx::query!(
		"INSERT INTO team_invites (id, team_id, email, role_id, token_hash, invited_by, expires_at)
		VALUES ($1, $2, $3, $4, $5, $6, $7)",
		id as TeamInviteId,
		team as TeamId,
		email,
		role as RoleId,
		digest(&token),
		user as UserId,
		OffsetDateTime::now_utc() + TTL
	)
	.execute(&mut *tx)
	.await;
	if let Err(e) = inserted {
		return Err(
			if crate::db::is_unique_violation(&e, "team_invites_team_email") {
				ServerError::Conflict(format!(
					"{email} already has an open invite; resend it instead."
				))
			} else {
				e.into()
			},
		);
	}
	tx.commit().await?;

	let invite = find(&s.db, team, id).await?;
	let issued = deliver(s, user, team, invite, &token).await;
	audit::log(
		&s.db,
		user,
		team,
		"invite.create",
		Target::new("invite", id.0).named(email),
		Diff::new()
			.set("role_id", role.0)
			.set("emailed", issued.emailed),
	)
	.await;
	Ok(issued)
}

/// Makes a new link for an open (or expired) invite and sends it again; the old link stops
/// working and the week starts over. Needs `create membership`, and the role may not reach
/// further than the caller's own.
///
/// # Errors
/// `NotFound` for an invite that is not the team's, `TooManyAttempts` past the hourly limit.
pub async fn resend(
	s: &AppState,
	user: UserId,
	team: TeamId,
	id: TeamInviteId,
) -> AppResult<Issued> {
	authorize(&s.db, user, team, Action::Create, Resource::Membership).await?;
	let before = find(&s.db, team, id).await?;
	super::ensure_within(&s.db, user, team, before.role_id).await?;
	let token = crypto::generate_token(TOKEN_BYTES);
	let mut tx = s.db.begin().await?;
	book_send(&mut tx, team).await?;
	let changed = sqlx::query!(
		"UPDATE team_invites SET token_hash = $3, expires_at = $4, invited_by = $5
		WHERE team_id = $1 AND id = $2",
		team as TeamId,
		id as TeamInviteId,
		digest(&token),
		OffsetDateTime::now_utc() + TTL,
		user as UserId
	)
	.execute(&mut *tx)
	.await?
	.rows_affected();
	if changed == 0 {
		return Err(ServerError::NotFound);
	}
	tx.commit().await?;

	let invite = find(&s.db, team, id).await?;
	let issued = deliver(s, user, team, invite, &token).await;
	audit::log(
		&s.db,
		user,
		team,
		"invite.resend",
		Target::new("invite", id.0).named(issued.invite.email.clone()),
		Diff::new().set("emailed", issued.emailed),
	)
	.await;
	Ok(issued)
}

/// Withdraws an invite: its link stops working at once. Needs `delete membership`.
///
/// # Errors
/// `NotFound` for an invite that is not the team's.
pub async fn revoke(db: &PgPool, user: UserId, team: TeamId, id: TeamInviteId) -> AppResult<()> {
	authorize(db, user, team, Action::Delete, Resource::Membership).await?;
	let email = sqlx::query_scalar!(
		"DELETE FROM team_invites WHERE team_id = $1 AND id = $2 RETURNING email",
		team as TeamId,
		id as TeamInviteId
	)
	.fetch_optional(db)
	.await?
	.ok_or(ServerError::NotFound)?;
	audit::log(
		db,
		user,
		team,
		"invite.revoke",
		Target::new("invite", id.0).named(email),
		Diff::new(),
	)
	.await;
	Ok(())
}

/// Removes control characters, so a team or person's name cannot add a mail header line.
fn one_line(s: &str) -> String {
	s.chars().filter(|c| !c.is_control()).collect()
}

/// The subject and body of an invite email.
pub fn email_text(
	team: &str,
	inviter: &str,
	role: &str,
	link: &str,
	expires: OffsetDateTime,
) -> (String, String) {
	let team = one_line(team);
	let inviter = one_line(inviter);
	let role = one_line(role);
	let subject = format!("{inviter} invited you to {team} on podkit");
	let body = format!(
		"{inviter} invited you to join the team {team} on podkit, as {role}.\n\n\
		To accept, open this link:\n\n{link}\n\n\
		It works once and expires on {} UTC. If you did not expect this, ignore this email: \
		nothing happens unless the link is opened.\n",
		expires.date()
	);
	(subject, body)
}

/// Sends the invite email if the team has a relay and podkit knows its own address; otherwise
/// hands back the link. Never fails the invite: it exists either way.
async fn deliver(s: &AppState, user: UserId, team: TeamId, invite: Invite, token: &str) -> Issued {
	let relative = path(token);
	let not_sent = |invite: Invite, link: String, note: &str| Issued {
		invite,
		emailed: false,
		link: Some(link),
		note: Some(note.to_owned()),
	};
	let Some(base) = s.config.public_url.as_deref() else {
		return not_sent(
			invite,
			relative,
			"Not emailed: podkit has no PUBLIC_URL, so an email could not carry a working link. Copy the link and send it to them yourself.",
		);
	};
	let link = format!("{base}{relative}");
	let smtp = match crate::notifications::settings::load_smtp(&s.db, team).await {
		Ok(Some(smtp)) => smtp,
		Ok(None) => {
			return not_sent(
				invite,
				link,
				"Not emailed: this team has no SMTP relay (Settings, Notifications). Copy the link and send it to them yourself.",
			);
		}
		Err(e) => {
			tracing::warn!("cannot load the SMTP settings for an invite: {e}");
			return not_sent(
				invite,
				link,
				"The email could not be sent. Copy the link and send it to them yourself.",
			);
		}
	};
	let team_name = sqlx::query_scalar!("SELECT name FROM teams WHERE id = $1", team.0)
		.fetch_optional(&s.db)
		.await
		.ok()
		.flatten()
		.unwrap_or_default();
	let inviter = users::name_of(&s.db, user)
		.await
		.ok()
		.flatten()
		.unwrap_or_else(|| "A podkit user".to_owned());
	let (subject, body) = email_text(
		&team_name,
		&inviter,
		&invite.role_name,
		&link,
		invite.expires_at,
	);
	match crate::notifications::send::send_mail(s, &smtp, &invite.email, &subject, body).await {
		Ok(()) => Issued {
			invite,
			emailed: true,
			link: None,
			note: None,
		},
		Err(reason) => {
			// The reason can name the relay's answer; it goes to the log, not to the person.
			tracing::warn!(%reason, "an invite email could not be sent");
			not_sent(
				invite,
				link,
				"The email could not be sent. Copy the link and send it to them yourself.",
			)
		}
	}
}

/// What the invite page shows to whoever opens a working link.
#[derive(Debug, Clone)]
pub struct Preview {
	pub team_name: String,
	pub email: String,
	pub role_name: String,
	pub invited_by: Option<String>,
	pub expires_at: OffsetDateTime,
	/// An account with the invited address exists: it signs in and joins, it does not sign up.
	pub account_exists: bool,
}

/// The invite behind `token`, if it still works. Nothing is consumed.
///
/// # Errors
/// `InviteNotFound` for a token that is unknown, used, revoked or expired, all alike.
pub async fn preview(db: &PgPool, token: &str) -> AppResult<Preview> {
	if !well_formed(token) {
		return Err(ServerError::InviteNotFound);
	}
	sqlx::query_as!(
		Preview,
		r#"
			SELECT t.name AS team_name, i.email,
				r.name AS role_name, u.name AS "invited_by?", i.expires_at,
				EXISTS (SELECT 1 FROM users a WHERE lower(a.email) = lower(i.email))
					AS "account_exists!"
			FROM team_invites i
			JOIN teams t ON t.id = i.team_id
			JOIN roles r ON r.id = i.role_id
			LEFT JOIN users u ON u.id = i.invited_by
			WHERE i.token_hash = $1 AND i.expires_at > NOW()
		"#,
		digest(token)
	)
	.fetch_optional(db)
	.await?
	.ok_or(ServerError::InviteNotFound)
}

struct Taken {
	id: i64,
	team: TeamId,
	email: String,
	role: RoleId,
}

/// Locks the invite behind `token` for the rest of the transaction, if it still works. An
/// invite is only as good as its sender's authority now: one whose sender is gone, lost
/// `create membership` or no longer holds everything its role grants answers like an unknown
/// link.
async fn lock(tx: &mut sqlx::PgConnection, token: &str) -> AppResult<Taken> {
	if !well_formed(token) {
		return Err(ServerError::InviteNotFound);
	}
	let invite = sqlx::query_as!(
		Taken,
		r#"
			SELECT id, team_id AS "team: TeamId", email, role_id AS "role: RoleId"
			FROM team_invites WHERE token_hash = $1 AND expires_at > NOW()
			FOR UPDATE
		"#,
		digest(token)
	)
	.fetch_optional(&mut *tx)
	.await?
	.ok_or(ServerError::InviteNotFound)?;
	let backed = sqlx::query_scalar!(
		r#"
			SELECT (
				i.invited_by IS NOT NULL
				AND EXISTS (
					SELECT 1 FROM team_members tm
					JOIN role_permissions rp ON rp.role_id = tm.role_id
					JOIN permissions p ON p.id = rp.permission_id
					WHERE tm.team_id = i.team_id AND tm.user_id = i.invited_by
					AND p.action = 'create' AND p.resource = 'membership'
				)
				AND NOT EXISTS (
					SELECT 1 FROM role_permissions rp
					WHERE rp.role_id = i.role_id
					AND rp.permission_id NOT IN (
						SELECT own.permission_id FROM team_members tm
						JOIN role_permissions own ON own.role_id = tm.role_id
						WHERE tm.team_id = i.team_id AND tm.user_id = i.invited_by
					)
				)
			) AS "backed!"
			FROM team_invites i WHERE i.id = $1
		"#,
		invite.id
	)
	.fetch_one(&mut *tx)
	.await?;
	if !backed {
		return Err(ServerError::InviteNotFound);
	}
	Ok(invite)
}

/// Deletes the team's open invites whose sender is gone, no longer holds `create membership`
/// or no longer holds everything the invite's role grants. Called whenever a role changes
/// (`members::set_role`); a link would refuse them anyway (see `lock`), this keeps the list
/// honest. Returns how many went.
pub async fn prune(db: &PgPool, team: TeamId) -> Result<u64, sqlx::Error> {
	let gone = sqlx::query!(
		r#"
			DELETE FROM team_invites i
			WHERE i.team_id = $1 AND (
				i.invited_by IS NULL
				OR NOT EXISTS (
					SELECT 1 FROM team_members tm
					JOIN role_permissions rp ON rp.role_id = tm.role_id
					JOIN permissions p ON p.id = rp.permission_id
					WHERE tm.team_id = i.team_id AND tm.user_id = i.invited_by
					AND p.action = 'create' AND p.resource = 'membership'
				)
				OR EXISTS (
					SELECT 1 FROM role_permissions rp
					WHERE rp.role_id = i.role_id
					AND rp.permission_id NOT IN (
						SELECT own.permission_id FROM team_members tm
						JOIN role_permissions own ON own.role_id = tm.role_id
						WHERE tm.team_id = i.team_id AND tm.user_id = i.invited_by
					)
				)
			)
		"#,
		team as TeamId
	)
	.execute(db)
	.await?
	.rows_affected();
	if gone > 0 {
		tracing::info!(%team, gone, "dropped invites their sender may no longer make");
	}
	Ok(gone)
}

/// Uses up the invite and adds `user` to its team with its role, in the caller's transaction.
/// The role is checked again: it must still be built-in or the team's own.
async fn join(tx: &mut sqlx::PgConnection, invite: &Taken, user: UserId) -> AppResult<()> {
	sqlx::query!("DELETE FROM team_invites WHERE id = $1", invite.id)
		.execute(&mut *tx)
		.await?;
	super::ensure_assignable(&mut *tx, invite.team, invite.role).await?;
	if super::find(&mut *tx, invite.team, user).await?.is_none() {
		super::insert(&mut *tx, invite.team, user, invite.role).await?;
	}
	Ok(())
}

/// Creates the account of the invited address and adds it to the team, whatever
/// `REGISTRATION` says: the invite is the permission, for that address only. `email`, when
/// given (the API asks for it), must be the invited one. Returns the new account and its team.
///
/// # Errors
/// `InviteNotFound`, `Validation` for another address, a bad name or password, `EmailTaken`
/// when the address has an account already (it signs in and accepts instead), and
/// `RegistrationClosed` on an instance with no account at all, whose first account only the
/// setup token makes.
pub async fn sign_up(
	db: &PgPool,
	token: &str,
	name: &str,
	email: Option<&str>,
	password: &str,
) -> AppResult<(UserId, TeamId)> {
	let invited = preview(db, token).await?;
	if let Some(email) = email
		&& crate::auth::normalize_email_lenient(email).to_lowercase()
			!= invited.email.to_lowercase()
	{
		return Err(ServerError::Validation(
			"This invite is for another email address.".to_owned(),
		));
	}
	if invited.account_exists {
		return Err(ServerError::EmailTaken);
	}
	let name = accounts::check_details(name, password)?;
	let password_hash = accounts::hash_password(password).await?;

	let mut tx = db.begin().await?;
	let invite = lock(&mut tx, token).await?;
	if !users::any(&mut *tx).await? {
		return Err(ServerError::RegistrationClosed);
	}
	// `users.email` is unique as written, not without case: two sign-ups for one address
	// queue here, and the later one finds the first one's account whatever its case.
	sqlx::query!(
		"SELECT pg_advisory_xact_lock(hashtextextended('user_email:' || lower($1), 0))",
		invite.email
	)
	.execute(&mut *tx)
	.await?;
	let has_account = sqlx::query_scalar!(
		r#"SELECT EXISTS (SELECT 1 FROM users WHERE lower(email) = lower($1)) AS "exists!""#,
		invite.email
	)
	.fetch_one(&mut *tx)
	.await?;
	if has_account {
		return Err(ServerError::EmailTaken);
	}
	let account = users::insert(&mut *tx, name, &invite.email, &password_hash)
		.await
		.map_err(accounts::email_taken)?;
	join(&mut tx, &invite, account.id).await?;
	tx.commit().await?;
	audit::log(
		db,
		account.id,
		invite.team,
		"invite.accept",
		Target::new("invite", invite.id).named(invite.email.clone()),
		Diff::new()
			.set("role_id", invite.role.0)
			.set("new_account", true),
	)
	.await;
	Ok((account.id, invite.team))
}

/// Adds the signed-in `user` to the invite's team, when the invite is for their address
/// (compared without case). A link for someone else is refused and stays usable for them.
/// Accepting into a team one is already in just uses the invite up.
///
/// # Errors
/// `InviteNotFound`, `Validation` when the address is not the user's.
pub async fn accept(db: &PgPool, user: UserId, token: &str) -> AppResult<TeamId> {
	let mut tx = db.begin().await?;
	let invite = lock(&mut tx, token).await?;
	let own = users::email_of(&mut *tx, user)
		.await?
		.ok_or(ServerError::InvalidToken)?;
	if own.to_lowercase() != invite.email.to_lowercase() {
		return Err(ServerError::Validation(format!(
			"This invite is for {}, and you are signed in as {own}. Sign in with that account to accept it.",
			invite.email
		)));
	}
	join(&mut tx, &invite, user).await?;
	tx.commit().await?;
	audit::log(
		db,
		user,
		invite.team,
		"invite.accept",
		Target::new("invite", invite.id).named(invite.email.clone()),
		Diff::new().set("role_id", invite.role.0),
	)
	.await;
	Ok(invite.team)
}

/// Drops invites that expired more than 30 days ago and send records past the hour that
/// counts; hourly, from the maintenance loop.
pub async fn purge(db: &PgPool) {
	let expired =
		sqlx::query!("DELETE FROM team_invites WHERE expires_at < NOW() - INTERVAL '30 days'")
			.execute(db)
			.await;
	let sends = sqlx::query!("DELETE FROM team_invite_sends WHERE at < NOW() - INTERVAL '1 day'")
		.execute(db)
		.await;
	if let Err(e) = expired.and(sends) {
		tracing::warn!("failed to purge old team invites: {e}");
	}
}

#[cfg(test)]
mod tests;
