//! Rooms through the hub: what each member is told, editor slots, the leader and the limits.
use std::{net::IpAddr, sync::Arc};

use office_live::{
    ByeReason, Event, Hub, JoinRefused, LiveConfig, Membership, Mode, Participant, PresenceState,
};
use serde_json::Value;
use tokio::sync::mpsc::Receiver;
use uuid::Uuid;

fn hub(tweak: impl FnOnce(&mut LiveConfig)) -> Arc<Hub> {
    let mut config = LiveConfig::default();
    tweak(&mut config);
    Arc::new(Hub::new(config))
}

fn user(id: &str) -> Participant {
    Participant::User {
        actor_id: id.into(),
        name: format!("Name {id}"),
    }
}

fn ip(last: u8) -> Option<IpAddr> {
    Some(IpAddr::from([10, 0, 0, last]))
}

struct Client {
    membership: Membership,
    events: Receiver<Event>,
}

fn join(hub: &Arc<Hub>, doc: Uuid, who: Participant, allowed: Mode) -> Client {
    let (membership, events) = hub.join(doc, who, allowed, ip(1)).expect("join");
    Client { membership, events }
}

impl Client {
    /// The next queued message, which must be there.
    fn next(&mut self) -> Value {
        match self.events.try_recv().expect("an event") {
            Event::Send(text) => serde_json::from_str(&text).expect("json"),
            Event::Close(reason) => panic!("closed: {reason:?}"),
            Event::Recheck => panic!("recheck"),
        }
    }

    fn next_of(&mut self, kind: &str) -> Value {
        loop {
            let message = self.next();
            if message["type"] == kind {
                return message;
            }
        }
    }

    fn drain(&mut self) {
        while self.events.try_recv().is_ok() {}
    }

    fn idle(&mut self) -> bool {
        self.events.try_recv().is_err()
    }
}

#[test]
fn members_see_each_other_and_the_most_recent_editor_leads() {
    let hub = hub(|_| {});
    let doc = Uuid::new_v4();
    let mut a = join(&hub, doc, user("a"), Mode::Edit);
    let hello = a.next();
    assert_eq!(hello["type"], "hello");
    assert_eq!(hello["v"], 1);
    assert_eq!(hello["mode"], "edit");
    assert_eq!(hello["notice"], Value::Null);
    assert_eq!(hello["leader"], hello["you"]);
    assert_eq!(hello["members"].as_array().map(Vec::len), Some(1));
    assert_eq!(hello["members"][0]["name"], "Name a");
    assert_eq!(hello["limits"]["ping_secs"], 20);

    let mut b = join(&hub, doc, user("b"), Mode::Edit);
    let b_hello = b.next();
    assert_eq!(b_hello["leader"], b_hello["you"]);
    let presence = a.next();
    assert_eq!(presence["type"], "presence");
    assert_eq!(presence["members"].as_array().map(Vec::len), Some(2));
    let leader = a.next();
    assert_eq!(leader["type"], "leader");
    assert_eq!(leader["leader"], b_hello["you"]);
    assert!(leader["seq"].as_u64() >= presence["seq"].as_u64());

    let mut c = join(&hub, doc, user("c"), Mode::View);
    assert_eq!(c.next()["leader"], b_hello["you"]);
    assert_eq!(a.next()["type"], "presence");
    assert!(a.idle(), "a viewer does not change the leader");

    drop(b);
    assert_eq!(a.next()["members"].as_array().map(Vec::len), Some(2));
    assert_eq!(a.next()["leader"], hello["you"]);
    c.drain();
    assert_eq!(hub.members_of(doc), 2);
    drop(a);
    drop(c);
    assert_eq!(hub.members_of(doc), 0);
    assert_eq!(hub.connections(), 0);
}

#[test]
fn saves_count_only_from_the_leader_while_several_edit() {
    let hub = hub(|_| {});
    let doc = Uuid::new_v4();
    assert!(hub.may_save(doc, "anyone"), "no room");
    let a = join(&hub, doc, user("a"), Mode::Edit);
    let viewer = join(&hub, doc, user("v"), Mode::Comment);
    assert!(hub.may_save(doc, "a"));
    assert!(
        hub.may_save(doc, "outsider"),
        "one editor saves as without a room"
    );
    let b = join(&hub, doc, user("b"), Mode::Edit);
    assert!(hub.may_save(doc, "b"));
    assert!(!hub.may_save(doc, "a"));
    assert!(!hub.may_save(doc, "outsider"));
    drop(b);
    assert!(hub.may_save(doc, "a"));
    drop((a, viewer));
}

#[test]
fn past_the_editor_limit_editors_join_in_view_mode_until_a_slot_frees() {
    let hub = hub(|c| c.max_editors = 2);
    let doc = Uuid::new_v4();
    let a = join(&hub, doc, user("a"), Mode::Edit);
    let _b = join(&hub, doc, user("b"), Mode::Edit);
    let mut c = join(&hub, doc, user("c"), Mode::Edit);
    let hello = c.next();
    assert_eq!(hello["mode"], "view");
    assert_eq!(hello["notice"], "editor_limit");
    assert!(
        !hub.may_save(doc, "c"),
        "a waiting editor is not the leader"
    );
    drop(a);
    let mode = c.next_of("mode");
    assert_eq!(mode["mode"], "edit");
    assert_eq!(mode["notice"], Value::Null);
}

#[test]
fn a_new_role_changes_the_mode_and_a_revoked_member_is_closed() {
    let hub = hub(|c| c.max_editors = 1);
    let doc = Uuid::new_v4();
    let mut a = join(&hub, doc, user("a"), Mode::Edit);
    let mut b = join(&hub, doc, user("b"), Mode::Edit);
    assert_eq!(b.next()["notice"], "editor_limit");
    a.drain();

    a.membership.set_allowed(Mode::View);
    assert_eq!(a.next_of("mode")["mode"], "view");
    let promoted = b.next_of("mode");
    assert_eq!(promoted["mode"], "edit");
    a.drain();
    b.drain();

    a.membership.set_allowed(Mode::View);
    assert!(a.idle() && b.idle(), "no change, no message");

    b.membership.revoke();
    assert!(matches!(
        b.events.try_recv(),
        Ok(Event::Close(ByeReason::Revoked))
    ));
    assert_eq!(hub.members_of(doc), 1);
    let presence = a.next_of("presence");
    assert_eq!(presence["members"].as_array().map(Vec::len), Some(1));
}

#[test]
fn link_holders_are_anonymous_and_see_no_names() {
    let hub = hub(|_| {});
    let doc = Uuid::new_v4();
    let mut a = join(&hub, doc, user("a"), Mode::Edit);
    a.drain();
    let mut link = join(&hub, doc, Participant::Link, Mode::View);
    let hello = link.next();
    assert_eq!(hello["mode"], "view");
    for member in hello["members"].as_array().expect("members") {
        assert!(member.get("name").is_none(), "{member}");
    }
    let seen = a.next_of("presence");
    let kinds: Vec<&str> = seen["members"]
        .as_array()
        .expect("members")
        .iter()
        .map(|m| m["kind"].as_str().expect("kind"))
        .collect();
    assert_eq!(kinds, ["user", "link"]);
    assert_eq!(seen["members"][0]["name"], "Name a");
    assert!(seen["members"][1].get("name").is_none());
}

#[test]
fn presence_state_and_rechecks_reach_the_members() {
    let hub = hub(|_| {});
    let doc = Uuid::new_v4();
    let mut a = join(&hub, doc, user("a"), Mode::Edit);
    let mut b = join(&hub, doc, user("b"), Mode::View);
    a.drain();
    b.drain();
    b.membership.set_state(PresenceState::Idle);
    assert_eq!(a.next_of("presence")["members"][1]["state"], "idle");
    b.drain();
    hub.access_changed(Some(doc));
    assert!(matches!(a.events.try_recv(), Ok(Event::Recheck)));
    assert!(matches!(b.events.try_recv(), Ok(Event::Recheck)));
    hub.access_changed(None);
    assert!(matches!(a.events.try_recv(), Ok(Event::Recheck)));
    hub.access_changed(Some(Uuid::new_v4()));
    assert!(a.idle());
}

#[test]
fn rooms_addresses_and_the_server_have_limits() {
    let hub = hub(|c| {
        c.max_per_document = 2;
        c.max_per_address = 3;
        c.max_connections = 4;
    });
    let doc = Uuid::new_v4();
    let _a = hub.join(doc, user("a"), Mode::Edit, ip(1)).expect("a");
    let _b = hub.join(doc, user("b"), Mode::Edit, ip(1)).expect("b");
    assert_eq!(
        hub.join(doc, user("c"), Mode::View, ip(2)).err(),
        Some(JoinRefused::RoomFull)
    );
    let _c = hub
        .join(Uuid::new_v4(), user("c"), Mode::View, ip(1))
        .expect("c");
    assert_eq!(
        hub.join(Uuid::new_v4(), user("d"), Mode::View, ip(1)).err(),
        Some(JoinRefused::AddressFull)
    );
    let _d = hub
        .join(Uuid::new_v4(), user("d"), Mode::View, ip(2))
        .expect("d");
    assert_eq!(
        hub.join(Uuid::new_v4(), user("e"), Mode::View, ip(3)).err(),
        Some(JoinRefused::ServerFull)
    );
    hub.shutdown();
    assert!(hub.shutdown_token().is_cancelled());
    drop(_d);
    assert_eq!(
        hub.join(Uuid::new_v4(), user("e"), Mode::View, ip(3)).err(),
        Some(JoinRefused::ShuttingDown)
    );
}

#[test]
fn a_member_that_falls_behind_is_dropped() {
    let hub = hub(|c| c.outbox = 2);
    let doc = Uuid::new_v4();
    let mut a = join(&hub, doc, user("a"), Mode::Edit);
    let slow = join(&hub, doc, user("slow"), Mode::View);
    a.drain();
    let mut seen = Vec::new();
    for _ in 0..4 {
        for state in [PresenceState::Idle, PresenceState::Active] {
            a.membership.set_state(state);
            while let Ok(Event::Send(text)) = a.events.try_recv() {
                seen.push(serde_json::from_str::<Value>(&text).expect("json"));
            }
        }
    }
    assert_eq!(hub.members_of(doc), 1);
    assert_eq!(hub.connections(), 1);
    let last = seen.last().expect("messages");
    assert_eq!(last["members"].as_array().map(Vec::len), Some(1));
    drop(slow);
    assert_eq!(hub.connections(), 1, "leaving twice counts once");
}

#[test]
fn region_locks_start_empty() {
    let hub = hub(|_| {});
    let doc = Uuid::new_v4();
    let _a = join(&hub, doc, user("a"), Mode::Edit);
    assert_eq!(hub.locks_held(doc), 0);
}
