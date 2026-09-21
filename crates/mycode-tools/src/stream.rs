//! `ToolStream` — the per-call progress/terminal channel tools write into
//! while executing (design doc `02-tools-permissions.md` §3).
//!
//! Stream invariant: any number of [`ToolStreamItem::Progress`] items
//! followed by **exactly one** [`ToolStreamItem::Terminal`]. The producer
//! side enforces "at most one terminal" atomically across clones: the
//! check→claim→send sequence is one critical section, so two clones racing
//! `terminal()` cannot both deliver and a `progress()` that passed the check
//! can never land after a `Terminal`. Once a terminal item has been sent,
//! every further item is *silently ignored* (returns `false`). This follows
//! the general single-terminal stream principle, so a tool that already
//! finished can never corrupt the stream.
//!
//! Builtin tools return their final result from `Tool::execute`; the
//! dispatcher sends that result as the terminal item unless the tool already
//! sent one. The internal tool channel is currently unbounded. `ToolStream`
//! remains the tool-dispatch stream and is not the provider event API.

use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

use crate::tool::ToolResult;

/// Incremental progress update from a running tool, rendered live by the UI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolProgress {
    pub message: String,
}

impl ToolProgress {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

/// One item on a tool's output stream.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ToolStreamItem {
    /// Incremental progress (zero or more, before the terminal).
    Progress(ToolProgress),
    /// The final result of the call (exactly one; ends the stream).
    Terminal(ToolResult),
}

/// Producer half of a [`ToolStream`]. Cloneable and shareable between
/// tasks; enforces the single-terminal invariant.
#[derive(Clone)]
pub struct ToolStream {
    tx: UnboundedSender<ToolStreamItem>,
    /// `true` once a [`ToolStreamItem::Terminal`] has been sent. Guarded
    /// by a mutex (not a bare atomic) so check→claim→send is one
    /// critical section shared by every clone: exactly one `terminal()`
    /// wins, and no in-flight `progress()` can be enqueued after the
    /// terminal — an atomic claim alone cannot order channel sends.
    state: Arc<Mutex<bool>>,
}

impl ToolStream {
    /// Create a channel pair: producer + consumer.
    pub fn channel() -> (Self, ToolStreamReceiver) {
        let (tx, rx) = mpsc::unbounded_channel();
        (
            Self {
                tx,
                state: Arc::new(Mutex::new(false)),
            },
            ToolStreamReceiver { rx },
        )
    }

    /// A stream nobody listens on — progress and terminal items are
    /// dropped. Useful for fire-and-forget dispatch and tests.
    pub fn closed() -> Self {
        let (stream, rx) = Self::channel();
        drop(rx);
        stream
    }

    /// Send a progress item. Returns `false` (and drops the item) once
    /// the stream has terminated or the receiver is gone.
    pub fn progress(&self, message: impl Into<String>) -> bool {
        self.send(ToolStreamItem::Progress(ToolProgress::new(message)))
    }

    /// Send the terminal result. Only the **first** terminal wins; later
    /// calls are ignored and return `false`.
    pub fn terminal(&self, result: ToolResult) -> bool {
        self.send(ToolStreamItem::Terminal(result))
    }

    /// Whether the stream has terminated (or has no receiver): further
    /// sends would be dropped.
    pub fn is_done(&self) -> bool {
        *self.state.lock().expect("tool stream state lock poisoned") || self.tx.is_closed()
    }

    fn send(&self, item: ToolStreamItem) -> bool {
        // One critical section across all clones: the check, the
        // terminal claim, and the channel send are indivisible, so a
        // racing terminal() can't double-deliver and a progress() that
        // passed the check can't be enqueued after a Terminal.
        let mut terminated = self.state.lock().expect("tool stream state lock poisoned");
        if *terminated {
            return false;
        }
        if matches!(item, ToolStreamItem::Terminal(_)) {
            *terminated = true;
        }
        self.tx.send(item).is_ok()
    }
}

/// Consumer half of a [`ToolStream`].
#[derive(Debug)]
pub struct ToolStreamReceiver {
    rx: UnboundedReceiver<ToolStreamItem>,
}

impl ToolStreamReceiver {
    /// Await the next item; `None` once all senders are dropped.
    pub async fn recv(&mut self) -> Option<ToolStreamItem> {
        self.rx.recv().await
    }

    /// Collect items until the terminal item arrives (or the producer is
    /// dropped without one — an early-exiting tool must not hang its
    /// consumer). The terminal item, when present, is included.
    pub async fn drain(&mut self) -> Vec<ToolStreamItem> {
        let mut items = Vec::new();
        while let Some(item) = self.recv().await {
            let is_terminal = matches!(item, ToolStreamItem::Terminal(_));
            items.push(item);
            if is_terminal {
                break;
            }
        }
        items
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mycode_core::message::ContentBlock;

    fn terminal_text(s: &str) -> ToolResult {
        ToolResult::text(s)
    }

    #[tokio::test]
    async fn progress_then_exactly_one_terminal() {
        let (stream, mut rx) = ToolStream::channel();

        assert!(stream.progress("step 1"));
        assert!(stream.progress("step 2"));
        assert!(stream.terminal(terminal_text("done")));

        let items = rx.drain().await;
        assert_eq!(
            items,
            vec![
                ToolStreamItem::Progress(ToolProgress::new("step 1")),
                ToolStreamItem::Progress(ToolProgress::new("step 2")),
                ToolStreamItem::Terminal(terminal_text("done")),
            ]
        );
        assert!(stream.is_done());
    }

    #[tokio::test]
    async fn second_terminal_is_ignored() {
        // Documented choice: not an error, not delivered — the first terminal
        // wins under the general single-terminal stream principle.
        let (stream, mut rx) = ToolStream::channel();

        assert!(stream.terminal(terminal_text("first")));
        assert!(!stream.terminal(terminal_text("second")));
        assert!(!stream.progress("too late"));

        let items = rx.drain().await;
        assert_eq!(
            items,
            vec![ToolStreamItem::Terminal(terminal_text("first"))]
        );
    }

    #[tokio::test]
    async fn drain_ends_when_producer_drops_without_terminal() {
        let (stream, mut rx) = ToolStream::channel();
        stream.progress("orphan progress");
        drop(stream);

        let items = rx.drain().await;
        assert_eq!(
            items,
            vec![ToolStreamItem::Progress(ToolProgress::new(
                "orphan progress"
            ))]
        );
    }

    #[tokio::test]
    async fn closed_stream_drops_everything() {
        let stream = ToolStream::closed();
        assert!(stream.is_done());
        assert!(!stream.progress("nobody listens"));
        assert!(!stream.terminal(terminal_text("nobody listens")));
    }

    #[tokio::test]
    async fn clones_share_the_termination_flag() {
        let (stream, mut rx) = ToolStream::channel();
        let clone = stream.clone();

        assert!(clone.terminal(terminal_text("via clone")));
        assert!(!stream.progress("original is now muted"));

        let items = rx.drain().await;
        assert_eq!(
            items,
            vec![ToolStreamItem::Terminal(terminal_text("via clone"))]
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_clones_deliver_exactly_one_terminal() {
        // The load-then-store check let every racing clone through; the
        // claim must be atomic, so hammer terminal() from blocking tasks.
        for _ in 0..64 {
            let (stream, mut rx) = ToolStream::channel();
            let senders: Vec<_> = (0..4)
                .map(|i| {
                    let clone = stream.clone();
                    tokio::task::spawn_blocking(move || {
                        clone.terminal(terminal_text(&format!("winner-{i}")))
                    })
                })
                .collect();
            let mut delivered = 0;
            for handle in senders {
                if handle.await.unwrap() {
                    delivered += 1;
                }
            }
            assert_eq!(delivered, 1, "exactly one terminal must win");

            let items = rx.drain().await;
            assert_eq!(items.len(), 1, "{items:?}");
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_progress_never_lands_after_terminal() {
        // Progress that passed the open-stream check used to be able to
        // enqueue after a racing terminal; drain() would hide it, so read
        // the channel dry and inspect the order.
        for _ in 0..64 {
            let (stream, mut rx) = ToolStream::channel();
            let mut handles = Vec::new();
            for i in 0..4 {
                let clone = stream.clone();
                handles.push(tokio::task::spawn_blocking(move || {
                    clone.progress(format!("p{i}"))
                }));
            }
            let term = stream.clone();
            handles.push(tokio::task::spawn_blocking(move || {
                term.terminal(terminal_text("end"))
            }));
            for handle in handles {
                let _ = handle.await.unwrap();
            }
            drop(stream);

            let mut items = Vec::new();
            while let Some(item) = rx.recv().await {
                items.push(item);
            }
            let terminal_pos = items
                .iter()
                .position(|item| matches!(item, ToolStreamItem::Terminal(_)));
            assert_eq!(terminal_pos, Some(items.len() - 1), "{items:?}");
        }
    }

    #[test]
    fn items_serialize_for_ui_transport() {
        let item = ToolStreamItem::Progress(ToolProgress::new("42 files"));
        let json = serde_json::to_string(&item).unwrap();
        assert_eq!(json, r#"{"Progress":{"message":"42 files"}}"#);

        let terminal = ToolStreamItem::Terminal(ToolResult {
            content: vec![ContentBlock::Text("ok".into())],
            is_error: false,
            details: None,
        });
        let back: ToolStreamItem =
            serde_json::from_str(&serde_json::to_string(&terminal).unwrap()).unwrap();
        assert_eq!(back, terminal);
    }
}
