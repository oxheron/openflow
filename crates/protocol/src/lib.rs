//! Versioned, platform-neutral wire types shared by `OpenFlow` clients and servers.
//!
//! The protocol deliberately contains no desktop automation or inference-engine
//! implementation details. This keeps remote servers incapable of controlling a
//! client's keyboard and allows inference backends to evolve independently.

mod dictation;
mod models;
mod server;

pub use dictation::*;
pub use models::*;
pub use server::*;

/// Incremented when a backwards-incompatible wire change is made.
pub const PROTOCOL_VERSION: u16 = 1;
