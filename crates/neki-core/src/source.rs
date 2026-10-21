//! Which files of a local directory go into its copy in the store.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::Result;

/// The entries of `root` to copy: in a git work tree, what git lists
/// ([`neki_fetch::git_source_files`]), so build results and other ignored
/// files don't change the source; everything otherwise.
pub(crate) struct SourceFiles {
    root: PathBuf,
    /// The listed files and the directories that hold them.
    listed: Option<(BTreeSet<PathBuf>, BTreeSet<PathBuf>)>,
}

impl SourceFiles {
    pub(crate) fn new(root: &Path) -> Result<Self> {
        let listed = neki_fetch::git_source_files(root)?.map(|files| {
            let dirs = files
                .iter()
                .flat_map(|f| f.ancestors().skip(1))
                .map(Path::to_path_buf)
                .collect();
            (files, dirs)
        });
        Ok(Self {
            root: root.to_path_buf(),
            listed,
        })
    }

    /// Whether `path`, under the root, is copied.
    pub(crate) fn contains(&self, path: &Path) -> bool {
        let (Some((files, dirs)), Ok(rel)) = (&self.listed, path.strip_prefix(&self.root)) else {
            return true;
        };
        // A listed directory is a submodule: everything in it is copied.
        dirs.contains(rel) || rel.ancestors().any(|a| files.contains(a))
    }
}
