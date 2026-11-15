//! touchos.toml: the configuration file of the panel.
//!
//! - [`toml`]: a parser for the TOML subset we use that keeps every line, so
//!   the file can be edited (by the web UI) without losing comments.
//! - [`schema`]: one description of every setting (type, default, limits,
//!   help text, whether a change applies live). The typed [`Config`] and the
//!   web form are both derived from it.
//!
//! - [`client`] (on touchos only): the config server's client.
//!
//! `no_std` + `alloc`, so the same code runs in the config server, in the
//! servers that read settings and in host-side `cargo test`.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

#[cfg(target_os = "none")]
pub mod client;
pub mod patch;
pub mod proto;
pub mod schema;
pub mod toml;
mod typed;

pub use patch::Effects;
pub use schema::{Apply, FIELDS, Field, Kind, Service, TILE_FIELDS};
pub use toml::{Document, ParseError, Raw, Table, Value};
pub use typed::*;
