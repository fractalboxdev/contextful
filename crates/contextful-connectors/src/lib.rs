//! The native sources compiled into the binary (`connector.package.built-in-registry`).

/// The shared record decoders (`connector.source.body-format`).
pub use contextful_decode as decode;
pub mod body;
pub mod boundary;
pub mod derive;
#[cfg(feature = "drive")]
pub mod drive;
pub mod file;
pub mod http;
pub mod image;
pub mod object;
pub mod proxy;
pub mod receive;
pub mod report;

/// The Google Drive source's name, listed whether or not the build compiles it in.
pub const DRIVE: &str = "drive";
/// The Picker-scoped Google Drive source.
pub const DRIVE_PICKS: &str = "gdrive-picks";
/// The feature compiling the Google Drive source in.
pub const DRIVE_FEATURE: &str = "drive";
/// The binary's feature linking the S3 bucket adapter the `s3` source reads through.
pub const OBJECT_FEATURE: &str = "s3-sync";

/// The names resolving to compiled-in sources.
pub const BUILT_IN: [&str; 9] = [http::NAME, derive::NAME, DRIVE, DRIVE_PICKS, object::NAME, file::NAME, image::NAME, receive::NAME, report::NAME];

/// Whether a listed source is compiled into this build; a feature-gated one that is not
/// answers with the feature to rebuild with.
pub fn compiled_in(name: &str) -> Result<(), String> {
    match name {
        DRIVE | DRIVE_PICKS if !cfg!(feature = "drive") => Err(format!("source `{name}` is compiled out of this build; rebuild with `--features {DRIVE_FEATURE}`")),
        object::NAME if !cfg!(feature = "s3") => Err(format!("source `{}` is compiled out of this build; rebuild with `--features {OBJECT_FEATURE}`", object::NAME)),
        _ => Ok(()),
    }
}
