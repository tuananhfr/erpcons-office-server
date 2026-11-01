use uuid::Uuid;

/// The live rooms as the document service sees them (plan-next §2.1). Implemented by the HTTP
/// layer; the service only reports access changes and asks before a save.
pub trait LiveRooms: Send + Sync {
    /// Access to `document` changed in this process, after the commit: members re-read their role
    /// at once instead of at the next periodic check.
    fn access_changed(&self, document: Uuid);

    /// Whether `actor_id` may save `document` now: while several members edit it, only the leader.
    fn may_save(&self, document: Uuid, actor_id: &str) -> bool;

    /// `actor_id` saved `document` as `version` (committed), holding every room update up to
    /// `live_seq` (`X-Live-Seq`); the room trims its log if the actor leads it.
    fn saved(&self, document: Uuid, actor_id: &str, live_seq: u64, version: i32);
}
