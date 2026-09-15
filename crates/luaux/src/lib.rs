//! luaux — a compiler for LuauX, JSX-style markup for Luau.
//!
//! See PLAN.md for architecture and PROPOSAL.md for syntax and semantics.
//!
//! This is Alloy's fork; see NOTICE for the patches.
#![allow(clippy::all)]

pub mod backend;
pub mod compile;
pub mod config;
pub mod imports;
pub mod lexer;
pub mod lint;
pub mod markup;
pub mod markup_scan;
pub mod resolve;
pub mod roblox;

pub use backend::{Backend, Element, Table};
pub use compile::{compile, compile_verified, CompileError};
pub use config::{Config, ConfigError};
pub use lexer::{tokenize, LexError, Token, TokenKind};
pub use markup_scan::{find_luaux_sites, LuauxSite};
