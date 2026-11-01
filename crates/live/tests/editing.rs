//! Co-editing through the hub: region locks, updates with their seq and ack, replay, saves by the
//! leader and cursors.
use std::{net::IpAddr, sync::Arc, thread::sleep, time::Duration};

use office_live::{Event, Hub, LiveConfig, Membership, Mode, Participant};
use serde_json::{Value, json};
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

struct Client {
    membership: Membership,
    events: Receiver<Event>,
    id: Value,
    hello: Value,
}

fn join(hub: &Arc<Hub>, doc: Uuid, who: Participant, allowed: Mode) -> Client {
    let address = Some(IpAddr::from([10, 0, 0, 1]));
    let (membership, events) = hub.join(doc, who, allowed, address).expect("join");
    let mut client = Client {
        membership,
        events,
        id: Value::Null,
        hello: Value::Null,
    };
    let hello = client.next();
    assert_eq!(hello["type"], "hello");
    client.id = hello["you"].clone();
    client.hello = hello;
    client
}

impl Client {
    fn next(&mut self) -> Value {
        match self.events.try_recv().expect("an event") {
            Event::Send(text) => serde_json::from_str(&text.to_text()).expect("json"),
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

    /// Every queued message of `kind`, the others dropped.
    fn all_of(&mut self, kind: &str) -> Vec<Value> {
        let mut found = Vec::new();
        while let Ok(Event::Send(text)) = self.events.try_recv() {
            let message: Value = serde_json::from_str(&text.to_text()).expect("json");
            if message["type"] == kind {
                found.push(message);
            }
        }
        found
    }

    fn drain(&mut self) {
        while self.events.try_recv().is_ok() {}
    }

    fn idle(&mut self) -> bool {
        self.events.try_recv().is_err()
    }

    /// The next message of one of `kinds`, the others skipped.
    fn answer(&mut self, kinds: &[&str]) -> Value {
        loop {
            let message = self.next();
            if kinds.iter().any(|k| message["type"] == *k) {
                return message;
            }
        }
    }

    fn acquire(&mut self, region: &str, cid: &str) -> Value {
        self.membership.acquire_lock(region, cid);
        let answer = self.answer(&["lock.granted", "lock.denied", "error"]);
        assert_eq!(answer["cid"], cid, "{answer}");
        answer
    }

    fn update(&mut self, region: &str, cid: &str, payload: &str) -> Value {
        self.membership.update(region, cid, payload);
        self.answer(&["ack", "error"])
    }

    fn replay(&self, since: u64) -> Vec<Value> {
        self.membership
            .replay(since)
            .iter()
            .map(|text| serde_json::from_str(&text.to_text()).expect("json"))
            .collect()
    }
}

/// The lock table of a `locks` message: region to holder.
fn table(locks: &Value) -> Vec<(String, Value)> {
    locks["locks"]
        .as_object()
        .expect("locks")
        .iter()
        .map(|(region, entry)| (region.clone(), entry["holder"].clone()))
        .collect()
}

#[test]
fn a_region_lock_is_granted_once_announced_and_released() {
    let hub = hub(|_| {});
    let doc = Uuid::new_v4();
    let mut a = join(&hub, doc, user("a"), Mode::Edit);
    let mut b = join(&hub, doc, user("b"), Mode::Edit);
    a.drain();

    let granted = a.acquire("para:1", "c1");
    assert_eq!(granted["type"], "lock.granted");
    assert_eq!(granted["region"], "para:1");
    assert_eq!(granted["holder"], a.id);
    assert_eq!(granted["expires_ms"], 5000);
    let seen = b.next_of("locks");
    assert_eq!(table(&seen), [("para:1".to_owned(), a.id.clone())]);
    assert_eq!(seen["locks"]["para:1"]["colour"], a.hello["colour"]);
    a.drain();

    let denied = b.acquire("para:1", "c2");
    assert_eq!(denied["type"], "lock.denied");
    assert_eq!(denied["holder"], a.id);
    assert!(denied["expires_ms"].as_u64().expect("ms") <= 5000);
    assert!(a.idle(), "a refusal changes nothing");

    assert_eq!(a.acquire("para:1", "c3")["type"], "lock.granted", "again");
    assert!(b.idle(), "a renewal is not announced");

    a.membership.release_lock("para:1");
    assert!(table(&b.next_of("locks")).is_empty());
    a.drain();
    assert_eq!(b.acquire("para:1", "c4")["type"], "lock.granted");
    assert_eq!(hub.locks_held(doc), 1);
    let mut c = join(&hub, doc, user("c"), Mode::View);
    let locks = c.next();
    assert_eq!(
        locks["type"], "locks",
        "a joiner hears the table after hello"
    );
    assert_eq!(table(&locks), [("para:1".to_owned(), b.id.clone())]);
}

#[test]
fn a_lock_lapses_unless_renewed() {
    let hub = hub(|c| c.lock_ttl = Duration::from_millis(400));
    let doc = Uuid::new_v4();
    let mut a = join(&hub, doc, user("a"), Mode::Edit);
    let mut b = join(&hub, doc, user("b"), Mode::Edit);
    a.drain();
    assert_eq!(a.acquire("p", "c1")["expires_ms"], 400);
    for _ in 0..6 {
        sleep(Duration::from_millis(100));
        a.membership.renew_lock("p");
        a.membership.expire_locks();
    }
    b.drain();
    assert_eq!(
        b.acquire("p", "c2")["type"],
        "lock.denied",
        "kept by renewals"
    );

    sleep(Duration::from_millis(500));
    a.drain();
    b.membership.expire_locks();
    assert!(table(&a.next_of("locks")).is_empty(), "lapsed and swept");
    assert_eq!(b.acquire("p", "c3")["type"], "lock.granted");

    a.drain();
    a.membership.renew_lock("p");
    let refused = a.next();
    assert_eq!(refused["type"], "lock.denied");
    assert_eq!(refused["cid"], Value::Null, "a refused renewal");
    assert_eq!(refused["holder"], b.id);
}

#[test]
fn a_lapsed_lock_is_taken_again_by_a_renewal_if_free() {
    let hub = hub(|c| c.lock_ttl = Duration::from_millis(50));
    let doc = Uuid::new_v4();
    let mut a = join(&hub, doc, user("a"), Mode::Edit);
    a.acquire("p", "c1");
    a.drain();
    sleep(Duration::from_millis(80));
    a.membership.expire_locks();
    assert!(table(&a.next_of("locks")).is_empty());
    a.membership.renew_lock("p");
    assert_eq!(table(&a.next_of("locks")), [("p".to_owned(), a.id.clone())]);
}

#[test]
fn locks_go_when_their_holder_leaves_loses_edit_or_is_revoked() {
    let hub = hub(|_| {});
    let doc = Uuid::new_v4();
    let mut watcher = join(&hub, doc, user("w"), Mode::View);
    let mut a = join(&hub, doc, user("a"), Mode::Edit);
    a.acquire("r1", "c1");
    a.acquire("r2", "c2");
    watcher.drain();
    drop(a);
    assert!(table(&watcher.next_of("locks")).is_empty(), "disconnect");

    let mut b = join(&hub, doc, user("b"), Mode::Edit);
    b.acquire("r1", "c1");
    watcher.drain();
    b.membership.set_allowed(Mode::Comment);
    assert!(table(&watcher.next_of("locks")).is_empty(), "downgrade");
    b.drain();
    b.membership.acquire_lock("r1", "c2");
    let refused = b.next();
    assert_eq!(refused["code"], "FORBIDDEN_MODE");
    assert_eq!(refused["cid"], "c2");

    let mut c = join(&hub, doc, user("c"), Mode::Edit);
    c.acquire("r1", "c1");
    watcher.drain();
    c.membership.revoke();
    assert!(table(&watcher.next_of("locks")).is_empty(), "revoked");
    assert_eq!(hub.locks_held(doc), 0);
}

#[test]
fn the_whole_document_lock_excludes_every_other() {
    let hub = hub(|_| {});
    let doc = Uuid::new_v4();
    let mut a = join(&hub, doc, user("a"), Mode::Edit);
    let mut b = join(&hub, doc, user("b"), Mode::Edit);
    a.drain();
    a.acquire("para:1", "c1");
    b.drain();
    assert_eq!(b.acquire("*", "c2")["holder"], a.id);
    assert_eq!(
        a.acquire("*", "c3")["type"],
        "lock.granted",
        "own locks allowed"
    );
    b.drain();
    let denied = b.acquire("para:2", "c4");
    assert_eq!(denied["type"], "lock.denied");
    assert_eq!(denied["holder"], a.id);
    a.membership.release_lock("*");
    a.membership.release_lock("para:1");
    b.drain();
    assert_eq!(b.acquire("*", "c5")["type"], "lock.granted");
    a.drain();
    assert_eq!(a.acquire("table:9", "c6")["type"], "lock.denied");
}

#[test]
fn order_locks_exclude_what_their_id_names() {
    let hub = hub(|_| {});
    let doc = Uuid::new_v4();
    let mut a = join(&hub, doc, user("a"), Mode::Edit);
    let mut b = join(&hub, doc, user("b"), Mode::Edit);
    a.drain();
    a.acquire("cell:s1:A1", "c1");
    a.acquire("annot:N1", "c2");
    a.acquire("shape:s1:h1", "c3");
    b.drain();
    assert_eq!(b.acquire("sheet:s1", "c4")["type"], "lock.denied");
    assert_eq!(b.acquire("sheet:s2", "c5")["type"], "lock.granted");
    assert_eq!(b.acquire("pages", "c6")["type"], "lock.granted");
    assert_eq!(b.acquire("slides", "c7")["type"], "lock.granted");
    a.drain();
    assert_eq!(a.acquire("cell:s2:B2", "c8")["type"], "lock.denied");
    assert_eq!(a.acquire("pages", "c9")["type"], "lock.denied");
    assert_eq!(a.acquire("cell:s1:A2", "c10")["type"], "lock.granted");
}

#[test]
fn a_member_holds_a_limited_number_of_locks() {
    let hub = hub(|c| c.max_locks_per_member = 2);
    let doc = Uuid::new_v4();
    let mut a = join(&hub, doc, user("a"), Mode::Edit);
    a.acquire("r1", "c1");
    a.acquire("r2", "c2");
    a.drain();
    let refused = a.acquire("r3", "c3");
    assert_eq!(refused["type"], "error");
    assert_eq!(refused["code"], "LOCK_LIMIT");
}

#[test]
fn updates_need_a_covering_lock_and_get_a_seq() {
    let hub = hub(|_| {});
    let doc = Uuid::new_v4();
    let mut a = join(&hub, doc, user("a"), Mode::Edit);
    let mut b = join(&hub, doc, user("b"), Mode::Edit);
    let mut v = join(&hub, doc, Participant::Link, Mode::View);
    a.drain();
    b.drain();

    let refused = a.update("para:1", "u1", "x");
    assert_eq!(
        refused,
        json!({ "type": "error", "v": 1, "code": "LOCK_REQUIRED", "cid": "u1" })
    );
    assert!(b.idle());

    a.acquire("para:1", "c1");
    a.drain();
    b.drain();
    v.drain();
    let ack = a.update("para:1", "u2", "hello");
    assert_eq!(ack["type"], "ack");
    assert_eq!(ack["cid"], "u2");
    let first = ack["seq"].as_u64().expect("seq");
    assert!(first > a.hello["seq"].as_u64().expect("hello seq"));
    let relayed = b.next();
    assert_eq!(
        relayed,
        json!({ "type": "region.update", "v": 1, "seq": first, "region": "para:1", "author": a.id, "payload": "hello" })
    );
    assert_eq!(v.next()["seq"], first, "viewers see changes live");
    assert!(a.idle(), "no echo to the author");

    let second = a.update("para:1", "u3", "hello!")["seq"]
        .as_u64()
        .expect("seq");
    assert!(second > first);
    assert_eq!(b.next()["seq"], second);

    b.drain();
    let refused = b.update("para:1", "u4", "mine");
    assert_eq!(refused["code"], "LOCK_REQUIRED", "a's lock is not b's");
}

#[test]
fn whole_document_and_sheet_locks_cover_their_regions() {
    let hub = hub(|_| {});
    let doc = Uuid::new_v4();
    let mut a = join(&hub, doc, user("a"), Mode::Edit);
    a.acquire("sheet:s1", "c1");
    a.drain();
    assert_eq!(a.update("cell:s1:C3", "u1", "1")["type"], "ack");
    assert_eq!(a.update("cell:s2:C3", "u2", "1")["code"], "LOCK_REQUIRED");
    a.acquire("*", "c2");
    a.drain();
    assert_eq!(a.update("para:7", "u3", "1")["type"], "ack");
}

#[test]
fn a_resent_cid_gets_its_old_ack_even_after_a_reconnect() {
    let hub = hub(|_| {});
    let doc = Uuid::new_v4();
    let mut a = join(&hub, doc, user("a"), Mode::Edit);
    let mut b = join(&hub, doc, user("b"), Mode::Edit);
    a.acquire("p", "c1");
    a.drain();
    b.drain();
    let seq = a.update("p", "same", "v1")["seq"].clone();
    assert_eq!(b.all_of("region.update").len(), 1);
    assert_eq!(a.update("p", "same", "v1")["seq"], seq);
    assert!(b.idle(), "not applied twice");

    drop(a);
    let mut again = join(&hub, doc, user("a"), Mode::Edit);
    again.drain();
    b.drain();
    let ack = again.update("p", "same", "v1");
    assert_eq!(
        ack,
        json!({ "type": "ack", "v": 1, "cid": "same", "seq": seq })
    );
    assert!(b.idle());

    let mut other = join(&hub, doc, user("c"), Mode::Edit);
    other.drain();
    assert_eq!(
        other.update("p", "same", "v1")["code"],
        "LOCK_REQUIRED",
        "cids are per account"
    );
}

#[test]
fn a_replay_sends_the_log_after_since_or_says_it_is_gone() {
    let hub = hub(|_| {});
    let doc = Uuid::new_v4();
    let mut a = join(&hub, doc, user("a"), Mode::Edit);
    let saved = a.hello["saved_seq"].as_u64().expect("saved_seq");
    assert_eq!(a.hello["saved_version"], Value::Null);
    a.acquire("p", "c1");
    a.drain();
    let s1 = a.update("p", "u1", "one")["seq"].as_u64().expect("seq");
    let s2 = a.update("p", "u2", "two")["seq"].as_u64().expect("seq");

    let c = join(&hub, doc, user("c"), Mode::View);
    let all = c.replay(saved);
    let payloads: Vec<&str> = all[..2]
        .iter()
        .map(|m| m["payload"].as_str().expect("p"))
        .collect();
    assert_eq!(payloads, ["one", "two"]);
    assert_eq!(all[2]["type"], "region.replayed");
    assert_eq!(all[2]["since"], saved);
    assert!(all[2]["seq"].as_u64().expect("seq") >= s2);

    let later = c.replay(s1);
    assert_eq!(later.len(), 2);
    assert_eq!(later[0]["seq"], s2);

    for since in [saved - 1, all[2]["seq"].as_u64().expect("seq") + 1, 0] {
        let gone = c.replay(since);
        assert_eq!(gone.len(), 1);
        assert_eq!(gone[0]["code"], "REPLAY_GONE", "{since}");
        assert_eq!(gone[0]["saved_seq"], saved);
    }
}

#[test]
fn a_full_log_asks_the_leader_to_save_and_a_save_trims_it() {
    let hub = hub(|c| c.max_log_bytes = 600);
    let doc = Uuid::new_v4();
    let mut a = join(&hub, doc, user("a"), Mode::Edit);
    let mut b = join(&hub, doc, user("b"), Mode::Edit);
    let saved = a.hello["saved_seq"].as_u64().expect("saved");
    a.acquire("p", "c1");
    a.drain();
    b.drain();
    let payload = "x".repeat(100);
    let mut last = 0;
    for n in 0..3 {
        last = a.update("p", &format!("u{n}"), &payload)["seq"]
            .as_u64()
            .expect("seq");
    }
    assert!(b.all_of("save.request").is_empty());
    a.update("p", "u3", &payload);
    let asked = b.all_of("save.request");
    assert_eq!(asked.len(), 1, "b, the newest editor, leads");
    assert!(a.all_of("save.request").is_empty());
    a.update("p", "u4", &payload);
    assert!(b.all_of("save.request").is_empty(), "asked once");

    let gone = b.replay(saved);
    assert_eq!(gone[0]["code"], "REPLAY_GONE");

    hub.saved(doc, "a", last, 9);
    assert!(
        b.all_of("saved").is_empty(),
        "only the leader's save counts"
    );
    hub.saved(doc, "b", u64::MAX, 9);
    assert!(b.all_of("saved").is_empty(), "not a seq of this room");
    hub.saved(doc, "b", last, 9);
    let note = a.next_of("saved");
    assert_eq!(
        note,
        json!({ "type": "saved", "v": 1, "saved_seq": last, "version": 9 })
    );
    assert_eq!(b.next_of("saved")["saved_seq"], last);
    let after = b.replay(last);
    assert_eq!(after.len(), 3, "two updates after the save, then the end");
    assert_eq!(b.replay(saved)[0]["saved_version"], 9);
    hub.saved(doc, "b", last, 10);
    assert!(a.all_of("saved").is_empty(), "not newer than the last save");

    let c = join(&hub, doc, user("c"), Mode::View);
    assert_eq!(c.hello["saved_seq"], last);
    assert_eq!(c.hello["saved_version"], 9);

    for n in 5..10 {
        a.update("p", &format!("u{n}"), &payload);
    }
    assert_eq!(
        b.all_of("save.request").len(),
        1,
        "a new overflow asks again"
    );
}

#[test]
fn a_save_older_than_the_log_asks_again() {
    let hub = hub(|c| c.max_log_bytes = 300);
    let doc = Uuid::new_v4();
    let mut a = join(&hub, doc, user("a"), Mode::Edit);
    let mut b = join(&hub, doc, user("b"), Mode::Edit);
    a.acquire("p", "c1");
    a.drain();
    b.drain();
    let payload = "x".repeat(100);
    let first = a.update("p", "u0", &payload)["seq"].as_u64().expect("seq");
    for n in 1..6 {
        a.update("p", &format!("u{n}"), &payload);
    }
    assert_eq!(b.all_of("save.request").len(), 1);
    hub.saved(doc, "b", first, 3);
    assert_eq!(a.next_of("saved")["saved_seq"], first);
    assert_eq!(
        b.all_of("save.request").len(),
        1,
        "what follows the save is no longer all logged"
    );
}

/// A node budget of 16 000 bytes; each room's remembered ids take 3072 of it, and its fair share
/// is at least 10 000 (a sixteenth of a 160 000-byte log).
fn tight(c: &mut LiveConfig) {
    c.max_total_log_bytes = 16_000;
    c.max_log_bytes = 160_000;
    c.max_acked_cids = 64;
}

/// Two editors in `doc`, the second leading, holding lock `p`.
fn pair(hub: &Arc<Hub>, doc: Uuid, names: [&str; 2]) -> (Client, Client) {
    let mut editor = join(hub, doc, user(names[0]), Mode::Edit);
    let mut leader = join(hub, doc, user(names[1]), Mode::Edit);
    editor.acquire("p", "lock");
    editor.drain();
    leader.drain();
    (editor, leader)
}

#[test]
fn the_node_sheds_a_room_above_its_share_and_keeps_its_cids() {
    let hub = hub(tight);
    let (big, small) = (Uuid::new_v4(), Uuid::new_v4());
    let (mut a, mut leader) = pair(&hub, big, ["a", "l"]);
    let (mut v, _) = pair(&hub, small, ["v", "w"]);
    let start = a.hello["saved_seq"].as_u64().expect("saved");
    let kept = v.hello["saved_seq"].as_u64().expect("saved");
    v.update("p", "v1", "small");
    let payload = "x".repeat(500);
    let first = a.update("p", "u0", &payload)["seq"].clone();
    for n in 1..40 {
        assert_eq!(a.update("p", &format!("u{n}"), &payload)["type"], "ack");
    }
    assert_eq!(
        a.replay(start)[0]["code"],
        "REPLAY_GONE",
        "the heavy log shed"
    );
    assert_eq!(leader.all_of("save.request").len(), 1);
    let replayed = v.replay(kept);
    assert_eq!(
        replayed.len(),
        2,
        "the light room keeps its update: {replayed:?}"
    );

    leader.drain();
    let again = a.update("p", "u0", &payload);
    assert_eq!(again["type"], "ack");
    assert_eq!(again["seq"], first, "shed from the log, still remembered");
    assert!(
        leader.all_of("region.update").is_empty(),
        "not applied twice"
    );
}

#[test]
fn a_full_node_never_forgets_a_rooms_cids() {
    let hub = hub(tight);
    let (many, big) = (Uuid::new_v4(), Uuid::new_v4());
    let (mut a, mut leader) = pair(&hub, many, ["a", "l"]);
    let cid = |n: usize| format!("{n:0>128}");
    let mut last = 0;
    let first = a.update("p", &cid(0), "x")["seq"].clone();
    for n in 1..30 {
        last = a.update("p", &cid(n), "x")["seq"].as_u64().expect("seq");
    }
    hub.saved(many, "l", last, 2);
    let (mut b, _) = pair(&hub, big, ["b", "m"]);
    assert_eq!(b.update("p", "big", &"y".repeat(6000))["type"], "ack");

    leader.drain();
    let again = a.update("p", &cid(0), "x");
    assert_eq!(again["seq"], first, "the room still knows the cid");
    assert!(
        leader.all_of("region.update").is_empty(),
        "not applied twice"
    );
}

#[test]
fn a_heavy_room_waits_rather_than_shed_a_light_one() {
    let hub = hub(tight);
    let (big, small) = (Uuid::new_v4(), Uuid::new_v4());
    let (mut a, mut leader) = pair(&hub, big, ["a", "l"]);
    let (mut v, _) = pair(&hub, small, ["v", "w"]);
    let kept = v.hello["saved_seq"].as_u64().expect("saved");
    let payload = "x".repeat(500);
    for n in 0..6 {
        assert_eq!(v.update("p", &format!("v{n}"), &payload)["type"], "ack");
    }
    let mut last = 0;
    let refused = (0..40)
        .map(|n| a.update("p", &format!("u{n}"), &payload))
        .find(|answer| {
            let busy = answer["type"] == "error";
            if !busy {
                last = answer["seq"].as_u64().expect("seq");
            }
            busy
        })
        .expect("refused once the budget is full");
    assert_eq!(refused["code"], "SERVER_BUSY");
    let cid = refused["cid"].as_str().expect("cid").to_owned();
    assert_eq!(v.replay(kept).len(), 7, "the light room keeps every update");

    hub.saved(big, "l", last, 2);
    leader.drain();
    assert_eq!(
        a.update("p", &cid, &payload)["type"],
        "ack",
        "sent again after the save"
    );
}

#[test]
fn a_refused_update_never_makes_another_room_shed() {
    let hub = hub(tight);
    let (big, small) = (Uuid::new_v4(), Uuid::new_v4());
    let (mut a, mut leader) = pair(&hub, big, ["a", "l"]);
    let (mut v, mut w) = pair(&hub, small, ["v", "w"]);
    let mut viewer = join(&hub, small, user("x"), Mode::View);
    viewer.drain();
    let start = a.hello["saved_seq"].as_u64().expect("saved");
    assert_eq!(v.update("p", "s1", "s")["type"], "ack");
    let payload = "x".repeat(500);
    for n in 0..13 {
        assert_eq!(a.update("p", &format!("u{n}"), &payload)["type"], "ack");
    }
    leader.drain();
    w.drain();

    // Each would overflow the node and shed the big room, were it taken.
    let large = "y".repeat(4000);
    assert_eq!(w.update("p", "w1", &large)["code"], "LOCK_REQUIRED");
    assert_eq!(v.update("p", "s1", &large)["type"], "ack", "a resend");
    assert_eq!(viewer.update("p", "x1", &large)["code"], "FORBIDDEN_MODE");

    assert!(leader.all_of("save.request").is_empty(), "nothing shed");
    assert_eq!(a.replay(start).len(), 14, "the big room keeps its log");
}

#[test]
fn rooms_idle_since_a_save_give_back_their_reserve_to_a_full_node() {
    for (idle, accepted) in [(Duration::ZERO, true), (Duration::from_secs(600), false)] {
        let hub = hub(|c| {
            tight(c);
            c.idle_log_release = idle;
        });
        let mut kept = Vec::new();
        for n in 0..5 {
            let doc = Uuid::new_v4();
            let (mut editor, leader) = pair(&hub, doc, ["e", "l"]);
            let seq = editor.update("p", &format!("c{n}"), "x")["seq"]
                .as_u64()
                .expect("seq");
            hub.saved(doc, "l", seq, 2);
            kept.push((editor, leader));
        }
        let (mut a, _b) = pair(&hub, Uuid::new_v4(), ["a", "b"]);
        let answer = a.update("p", "new", "x");
        if accepted {
            assert_eq!(answer["type"], "ack", "{answer}");
        } else {
            assert_eq!(answer["code"], "SERVER_BUSY", "within 10 minutes: {answer}");
        }
    }
}

#[test]
fn a_new_leader_is_asked_to_save_when_the_old_one_was() {
    let hub = hub(|c| c.max_log_bytes = 100);
    let doc = Uuid::new_v4();
    let mut a = join(&hub, doc, user("a"), Mode::Edit);
    let b = join(&hub, doc, user("b"), Mode::Edit);
    a.acquire("p", "c1");
    a.update("p", "u1", &"x".repeat(80));
    a.update("p", "u2", &"x".repeat(80));
    a.drain();
    drop(b);
    assert_eq!(a.next_of("leader")["leader"], a.id);
    assert_eq!(a.next()["type"], "save.request");
}

#[test]
fn view_and_comment_members_cannot_lock_or_update() {
    let hub = hub(|_| {});
    let doc = Uuid::new_v4();
    for mode in [Mode::View, Mode::Comment] {
        let mut m = join(&hub, doc, user("m"), mode);
        m.drain();
        let refused = m.acquire("p", "c1");
        assert_eq!(refused["code"], "FORBIDDEN_MODE");
        let refused = m.update("p", "c2", "x");
        assert_eq!(refused["code"], "FORBIDDEN_MODE");
        assert_eq!(refused["cid"], "c2");
        m.membership.renew_lock("p");
        let refused = m.next();
        assert_eq!(refused["code"], "FORBIDDEN_MODE");
        assert!(refused.get("cid").is_none());
    }
    // An editor past the room's editor limit only views.
    let hub = self::hub(|c| c.max_editors = 1);
    let _first = join(&hub, doc, user("a"), Mode::Edit);
    let mut capped = join(&hub, doc, user("b"), Mode::Edit);
    assert_eq!(capped.hello["mode"], "view");
    capped.drain();
    assert_eq!(capped.acquire("p", "c1")["code"], "FORBIDDEN_MODE");
}

#[test]
fn cursors_reach_the_others_and_are_never_logged() {
    let hub = hub(|c| c.outbox = 8);
    let doc = Uuid::new_v4();
    let mut a = join(&hub, doc, user("a"), Mode::Comment);
    let mut b = join(&hub, doc, user("b"), Mode::Edit);
    let mut v = join(&hub, doc, Participant::Link, Mode::View);
    a.drain();
    b.drain();
    v.drain();
    a.membership.cursor("para:1", "3-5");
    assert_eq!(
        b.next(),
        json!({ "type": "cursor", "v": 1, "author": a.id, "region": "para:1", "range": "3-5" })
    );
    assert_eq!(v.next()["type"], "cursor");
    assert!(a.idle());
    v.membership.cursor("para:1", "1");
    assert_eq!(v.next()["code"], "FORBIDDEN_MODE");
    assert!(b.idle());

    let replay = b.replay(b.hello["saved_seq"].as_u64().expect("saved"));
    assert_eq!(replay.len(), 1, "only the end of the replay");

    // A queue full of cursors drops cursors, not the member.
    for n in 0..20 {
        a.membership.cursor("para:1", &n.to_string());
    }
    assert_eq!(hub.members_of(doc), 3);
}

#[test]
fn cursors_leave_room_for_what_must_arrive() {
    let hub = hub(|c| c.outbox = 8);
    let doc = Uuid::new_v4();
    let mut a = join(&hub, doc, user("a"), Mode::Edit);
    let mut b = join(&hub, doc, user("b"), Mode::Edit);
    a.drain();
    b.drain();
    for n in 0..20 {
        a.membership.cursor("para:1", &n.to_string());
    }
    let granted = a.acquire("p", "c1");
    assert_eq!(granted["type"], "lock.granted");
    assert_eq!(
        hub.members_of(doc),
        2,
        "b, slow with cursors, is not dropped"
    );
    let mut cursors = 0;
    loop {
        let message = b.next();
        if message["type"] == "locks" {
            break;
        }
        assert_eq!(message["type"], "cursor");
        cursors += 1;
    }
    assert!(cursors <= 4, "at most half the queue: {cursors}");
}

#[test]
fn a_room_opened_again_numbers_above_the_last_one() {
    let hub = hub(|_| {});
    let doc = Uuid::new_v4();
    let mut a = join(&hub, doc, user("a"), Mode::Edit);
    a.acquire("p", "c1");
    a.drain();
    let last = a.update("p", "u1", "x")["seq"].as_u64().expect("seq");
    drop(a);
    assert_eq!(hub.members_of(doc), 0);
    let b = join(&hub, doc, user("b"), Mode::Edit);
    let base = b.hello["saved_seq"].as_u64().expect("saved");
    assert!(base >= last, "{base} < {last}");
    assert_eq!(b.replay(last - 1)[0]["code"], "REPLAY_GONE");
    assert!(base < (1u64 << 53), "exact in JavaScript");
}
