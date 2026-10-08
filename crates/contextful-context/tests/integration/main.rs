//! The store adapter's one integration binary, one module per operation. A test asserting
//! through SQL runs only in the build linking `read`; the rest run in both.
#![cfg_attr(not(feature = "read"), allow(unused_imports, dead_code))]

mod bound_time;
mod build;
mod catalog;
mod declare;
mod encrypt;
mod erase;
mod fold;
mod index;
mod init;
mod lay_out;
mod ledger;
mod read;
#[cfg(feature = "read")]
mod read_deadline;
mod reconcile;
mod retention;
mod redaction;
mod reserve;
mod rows;
mod run_commit;
mod support;
