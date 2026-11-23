//! `podkit-firewall`: the one small root process of podkit.
//!
//! It owns exactly one nftables table, `inet podkit`, and nothing else. The podkit agent
//! (an unprivileged user) talks to it over a unix socket that only that user may use:
//!
//! * [`policy`] is what a caller may ask for: SSH port and sources plus explicit allows.
//! * [`render`] turns a policy into an nft script and mirrors it in [`render::allows_input`],
//!   so the "SSH is never locked out" invariant can be tested without root.
//! * [`engine`] applies with confirm: a change is reverted after a deadline unless it is
//!   confirmed, like `iptables-apply`.
//! * [`protocol`] and [`daemon`] are the wire format and the socket server with its peer
//!   credential check; [`host`] is the real `nft` and `/proc` side.

pub mod daemon;
pub mod diff;
pub mod engine;
pub mod host;
pub mod policy;
pub mod protocol;
pub mod render;
