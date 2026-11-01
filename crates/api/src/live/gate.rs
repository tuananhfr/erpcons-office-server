use std::sync::Arc;

use office_documents::LiveRooms;
use office_live::Hub;
use uuid::Uuid;

/// The rooms, as the document service reaches them.
pub(crate) struct LiveGate(pub Arc<Hub>);

impl LiveRooms for LiveGate {
    fn access_changed(&self, document: Uuid) {
        self.0.access_changed(document);
    }

    fn may_save(&self, document: Uuid, actor_id: &str) -> bool {
        self.0.may_save(document, actor_id)
    }

    fn saved(&self, document: Uuid, actor_id: &str, live_seq: u64, version: i32) {
        self.0.saved(document, actor_id, live_seq, version);
    }

    fn comments_changed(&self, document: Uuid, thread: Option<Uuid>) {
        self.0.comments_changed(document, thread);
    }
}
