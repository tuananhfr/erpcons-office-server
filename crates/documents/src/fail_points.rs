use std::sync::{
    Arc, Mutex, PoisonError,
    atomic::{AtomicBool, Ordering},
};

use tokio::sync::Notify;

/// Places in a durable write where a test can make it fail, as a crash or a lost database would.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailPoint {
    /// The bytes are stored; no transaction has started.
    AfterPut,
    /// Version, document and audit rows are written; the commit has not happened.
    BeforeCommit,
}

/// Armed fail points fire once; a hold pauses the next request there once. Never used outside
/// tests.
#[derive(Debug, Default)]
pub struct FailPoints {
    after_put: AtomicBool,
    before_commit: AtomicBool,
    after_put_hold: Mutex<Option<Arc<Gate>>>,
    before_commit_hold: Mutex<Option<Arc<Gate>>>,
}

#[derive(Debug, Default)]
struct Gate {
    reached: Notify,
    released: Notify,
}

/// A request paused at a fail point (see [`FailPoints::hold`]).
pub struct Held(Arc<Gate>);

impl Held {
    /// Resolves once a request has arrived at the point.
    pub async fn reached(&self) {
        self.0.reached.notified().await;
    }

    /// Lets the paused request go on.
    pub fn release(&self) {
        self.0.released.notify_one();
    }
}

impl FailPoints {
    pub fn arm(&self, point: FailPoint) {
        self.flag(point).store(true, Ordering::SeqCst);
    }

    /// Pauses the next request that reaches `point` until [`Held::release`].
    pub fn hold(&self, point: FailPoint) -> Held {
        let gate = Arc::new(Gate::default());
        *self
            .slot(point)
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(gate.clone());
        Held(gate)
    }

    pub(crate) fn fires(&self, point: FailPoint) -> bool {
        self.flag(point).swap(false, Ordering::SeqCst)
    }

    pub(crate) async fn pause(&self, point: FailPoint) {
        let gate = self
            .slot(point)
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(gate) = gate {
            // notify_one keeps a permit, so neither side can miss the other.
            gate.reached.notify_one();
            gate.released.notified().await;
        }
    }

    fn flag(&self, point: FailPoint) -> &AtomicBool {
        match point {
            FailPoint::AfterPut => &self.after_put,
            FailPoint::BeforeCommit => &self.before_commit,
        }
    }

    fn slot(&self, point: FailPoint) -> &Mutex<Option<Arc<Gate>>> {
        match point {
            FailPoint::AfterPut => &self.after_put_hold,
            FailPoint::BeforeCommit => &self.before_commit_hold,
        }
    }
}
