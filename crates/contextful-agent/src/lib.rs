//! The read face's transports: the tool protocol over standard input and output, and
//! MCP Streamable HTTP.

#[cfg(feature = "http")]
pub mod http;
pub mod mcp;
