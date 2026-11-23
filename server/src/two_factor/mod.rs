//! Two-step verification: an authenticator app (TOTP), recovery codes and passkeys, the
//! team policy that requires them, and the second step of a sign-in.
//!
//! `totp` is the pure RFC 6238 part; `service` holds the authenticator app and recovery
//! codes, `passkeys` the WebAuthn ceremonies, `policy` the team setting and `challenge`
//! the sign-in step between a right password and a session.

pub mod challenge;
pub mod passkeys;
pub mod policy;
pub mod service;
pub mod totp;

#[cfg(test)]
mod tests;
