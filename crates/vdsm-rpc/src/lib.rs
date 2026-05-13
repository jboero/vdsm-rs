//! JSON-RPC over TLS server for vdsm-rs.
//!
//! The transport stack:
//!
//!   TCP  ->  rustls (engine-issued certs)  ->  newline-delimited JSON-RPC 2.0  ->  Dispatcher
//!
//! Newline framing is **not** what real ovirt-engine speaks — engine wraps
//! JSON-RPC in STOMP frames over the same TLS connection. We do the simpler
//! transport first so the listener is testable with `openssl s_client`, and
//! layer STOMP in a follow-up session before doing actual engine bring-up.
//!
//! Per-handler logic lives in the verb crates (vdsm-host, vdsm-virt, ...);
//! `vdsmd` wires them into the Dispatcher at startup.

pub mod dispatch;
pub mod protocol;
pub mod server;
pub mod stomp;
pub mod tls;

pub use dispatch::{DispatchFn, Dispatcher};
pub use protocol::{JsonRpcError, Request, Response};
pub use server::{Framing, Server, ServerConfig};
