//! The sandboxed component host. A guest connector built against `wit/connector.wit`
//! runs under wasmtime with a linear-memory cap and per-call wall-clock deadlines, reaches
//! the network only through its `wasi:http` import — which this host answers with the
//! mediated client — and hands batches over as Arrow IPC.

pub mod batch;
pub mod host;
pub mod limits;
mod mediate;
pub mod source;

pub use host::{ComponentHost, Connector, Cursor, CursorKind, DataType, Field, Grant, LogLine, Schema, Session, Target, WORLD};
pub use limits::Limits;
pub use mediate::{Reservation, Reserve, Traffic};
pub use source::GuestSource;
