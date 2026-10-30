use std::sync::{
    Arc, Mutex, PoisonError,
    atomic::{AtomicU32, Ordering},
};

use tokio::sync::Notify;

/// Places in a durable write where a test can make it fail, as a crash or a lost database would.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailPoint {
    /// The bytes are stored; no transaction has started.
    AfterPut,
    /// Version, document and audit rows are written; the commit has not happened.
    BeforeCommit,
    /// A sharing attempt has written its permissions; it fails as when a sign-in attached an
    /// invitation for the same person meanwhile (a unique index collision).
    ShareCollision,
    /// A version's deletion is committed and its bytes are unused; they are not deleted yet, as
    /// when the server stops right there.
    AfterVersionDelete,
}

/// Armed fail points fire once (or as often as armed); a hold pauses the next request there once.
/// Never used outside tests.
#[derive(Debug, Default)]
pub struct FailPoints {
    after_put: AtomicU32,
    before_commit: AtomicU32,
    share_collision: AtomicU32,
    after_version_delete: AtomicU32,
    after_put_hold: Mutex<Option<Arc<Gate>>>,
    before_commit_hold: Mutex<Option<Arc<Gate>>>,
    share_collision_hold: Mutex<Option<Arc<Gate>>>,
    after_version_delete_hold: Mutex<Option<Arc<Gate>>>,
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
        self.arm_times(point, 1);
    }

    /// Fires at the next `times` requests that reach `point`.
    pub fn arm_times(&self, point: FailPoint, times: u32) {
        self.flag(point).store(times, Ordering::SeqCst);
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
        self.flag(point)
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
            .is_ok()
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

    fn flag(&self, point: FailPoint) -> &AtomicU32 {
        match point {
            FailPoint::AfterPut => &self.after_put,
            FailPoint::BeforeCommit => &self.before_commit,
            FailPoint::ShareCollision => &self.share_collision,
            FailPoint::AfterVersionDelete => &self.after_version_delete,
        }
    }

    fn slot(&self, point: FailPoint) -> &Mutex<Option<Arc<Gate>>> {
        match point {
            FailPoint::AfterPut => &self.after_put_hold,
            FailPoint::BeforeCommit => &self.before_commit_hold,
            FailPoint::ShareCollision => &self.share_collision_hold,
            FailPoint::AfterVersionDelete => &self.after_version_delete_hold,
        }
    }
}
