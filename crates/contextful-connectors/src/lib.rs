//! The native sources compiled into the binary (`connector.package.built-in-registry`).

/// The shared record decoders (`connector.source.body-format`).
pub use contextful_decode as decode;
pub mod boundary;
pub mod derive;
#[cfg(feature = "drive")]
pub mod drive;
pub mod http;

/// The Google Drive source's name, listed whether or not the build compiles it in.
pub const DRIVE: &str = "drive";
/// The feature compiling the Google Drive source in.
pub const DRIVE_FEATURE: &str = "drive";

/// The names resolving to compiled-in sources.
pub const BUILT_IN: [&str; 3] = [http::NAME, derive::NAME, DRIVE];

/// Whether a listed source is compiled into this build; a feature-gated one that is not
/// answers with the feature to rebuild with.
pub fn compiled_in(name: &str) -> Result<(), String> {
    match name {
        DRIVE if !cfg!(feature = "drive") => Err(format!("source `{DRIVE}` is compiled out of this build; rebuild with `--features {DRIVE_FEATURE}`")),
        _ => Ok(()),
    }
}
