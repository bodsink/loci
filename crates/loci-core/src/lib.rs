//! Shared primitives for the loci code intelligence engine.
//!
//! Nothing in this crate touches the network. File reads go through [`Sandbox`],
//! which is the single enforcement point for "only read under the project root
//! the user chose".

pub mod error;
pub mod lang;
pub mod paths;
pub mod sandbox;

pub use error::{LociError, Result};
pub use lang::LanguageId;
pub use sandbox::Sandbox;

/// Bumped whenever the on-disk graph layout changes in a way that makes an
/// existing store unreadable, or when a parser recovery pass would change
/// which files are `parse_partial`. Stores tagged with a different version
/// are fully re-indexed instead of keeping stale coverage records.
pub const SCHEMA_VERSION: u32 = 4;

/// Files above this size are recorded as `skipped` with reason `oversized`
/// rather than being parsed. Keeps worst-case memory bounded on large repos.
pub const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;

/// Hash used for incremental change detection.
pub fn content_hash(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_hash_is_stable_and_distinguishing() {
        assert_eq!(content_hash(b"fn main() {}"), content_hash(b"fn main() {}"));
        assert_ne!(content_hash(b"a"), content_hash(b"b"));
    }
}
