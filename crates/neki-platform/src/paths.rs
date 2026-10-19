//! Default locations for the store, state and per-user configuration.

use std::path::PathBuf;

/// Default store directory. Fixed per OS family so store paths (which hash
/// the store directory) are identical on every machine of that family.
pub fn default_store_dir() -> PathBuf {
    if cfg!(windows) {
        PathBuf::from(r"C:\neki\store")
    } else {
        PathBuf::from("/opt/neki/store")
    }
}

/// Default state directory: metadata DB, GC roots, profiles and build logs.
pub fn default_state_dir() -> PathBuf {
    if cfg!(windows) {
        PathBuf::from(r"C:\neki\var")
    } else {
        PathBuf::from("/opt/neki/var")
    }
}

/// Per-user configuration directory (`<OS config dir>/neki`).
pub fn user_config_dir() -> Option<PathBuf> {
    dirs::config_dir().map(|dir| dir.join("neki"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_dirs_are_absolute() {
        assert!(default_store_dir().is_absolute());
        assert!(default_state_dir().is_absolute());
    }
}
