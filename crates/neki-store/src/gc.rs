use std::time::Duration;

use neki_hash::StorePath;

#[derive(Debug, Clone, Default)]
pub struct GcOptions {
    /// Only report what would be deleted.
    pub dry_run: bool,
    /// Also delete profile generations older than this.
    pub delete_older_than: Option<Duration>,
}

#[derive(Debug, Clone, Default)]
pub struct GcResult {
    pub deleted: Vec<StorePath>,
    pub bytes_freed: u64,
}
