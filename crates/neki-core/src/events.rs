use neki_hash::StorePath;

/// Progress reported by long-running operations.
#[derive(Debug, Clone)]
pub enum Event {
    /// `drv` will be built after the derivations in `waits_for`; sent for
    /// every derivation of a build before the first one starts.
    BuildPlanned {
        drv: StorePath,
        waits_for: Vec<StorePath>,
    },
    BuildStarted {
        drv: StorePath,
    },
    BuildLog {
        drv: StorePath,
        line: String,
    },
    BuildFinished {
        drv: StorePath,
        success: bool,
    },
    Download {
        path: StorePath,
        done: u64,
        total: Option<u64>,
    },
    /// A path was downloaded from a binary cache instead of being built.
    Substituted {
        path: StorePath,
    },
    Message(String),
}

/// Receiver of [`Event`]s. `Sync` because builds report from worker threads.
pub trait EventSink: Sync {
    /// Handle `event`. It's called from the thread that produced it.
    fn event(&self, event: Event);
}

/// Discards all events.
pub struct NullSink;

impl EventSink for NullSink {
    fn event(&self, _event: Event) {}
}
