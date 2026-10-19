use neki_hash::StorePath;

/// Progress reported by long-running operations.
#[derive(Debug, Clone)]
pub enum Event {
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
    Message(String),
}

/// Receiver of [`Event`]s. `Sync` because builds report from worker threads.
pub trait EventSink: Sync {
    fn event(&self, event: Event);
}

/// Discards all events.
pub struct NullSink;

impl EventSink for NullSink {
    fn event(&self, _event: Event) {}
}
