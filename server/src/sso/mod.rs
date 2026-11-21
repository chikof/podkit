//! Single sign-on: sign in with an identity provider (generic OpenID Connect, or GitHub).
//!
//! `providers` is the administration of providers (the instance's, set by the instance
//! administrator, or one team's), `signin` is the round trip and what comes of it: a
//! session, a new account, or a request to prove the password before linking. `oidc` and
//! `github` speak the two protocols.

pub mod github;
pub mod oidc;
pub mod providers;
pub mod signin;

#[cfg(test)]
mod stub;
#[cfg(test)]
mod tests;

pub use providers::Kind;

/// Who an identity provider says the person is, after its answer was verified.
#[derive(Debug, Clone)]
pub struct Identity {
	/// The provider's stable id for the person (`sub`; GitHub's numeric id).
	pub subject: String,
	pub email: Option<String>,
	/// Whether the provider vouches for the email.
	pub email_verified: bool,
	pub name: String,
	pub groups: Vec<String>,
}
