//! End-to-end party test over real iroh endpoints (needs internet for discovery/relay).
//! Run with: cargo test -p lootline-net --test party -- --ignored --nocapture

use lootline_core::{Event, EventLog};
use lootline_net::{spawn, NetCmd, NetConfig, NetEvent};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use uuid::Uuid;

struct Peer {
    log: Arc<Mutex<EventLog>>,
    handle: lootline_net::NetHandle,
    rx: mpsc::Receiver<NetEvent>,
    code: String,
}

fn peer(seed: u8) -> Peer {
    let log = Arc::new(Mutex::new(EventLog::new()));
    let (tx, rx) = mpsc::channel();
    let tx = Mutex::new(tx);
    let handle = spawn(NetConfig { secret: [seed; 32] }, log.clone(), move |e| {
        let _ = tx.lock().unwrap().send(e);
    });
    let code = match rx.recv_timeout(Duration::from_secs(2)).unwrap() {
        NetEvent::Identity { code, .. } => code,
        other => panic!("expected identity, got {other:?}"),
    };
    Peer { log, handle, rx, code }
}

fn wait_for(deadline_secs: u64, mut f: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + Duration::from_secs(deadline_secs);
    while Instant::now() < end {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

#[test]
#[ignore = "needs internet"]
fn host_and_two_joiners_converge() {
    let m = Uuid::new_v4();
    let host = peer(1);
    let a = peer(2);
    let b = peer(3);

    // events made before joining must reach everyone after the Have exchange
    let pre = Event::new_chest("host", 1, m);
    host.log.lock().unwrap().insert(pre.clone());
    let a_pre = Event::new_chest("a", 2, m);
    a.log.lock().unwrap().insert(a_pre.clone());

    host.handle.send(NetCmd::Host);
    std::thread::sleep(Duration::from_secs(2));
    a.handle.send(NetCmd::Join(host.code.clone()));
    b.handle.send(NetCmd::Join(host.code.clone()));

    assert!(wait_for(60, || [&host, &a, &b].iter().all(|p| p.log.lock().unwrap().len() >= 2)), "initial sync failed");
    assert!(a.log.lock().unwrap().contains(&pre.id()));
    assert!(host.log.lock().unwrap().contains(&a_pre.id()));
    assert!(wait_for(30, || b.log.lock().unwrap().contains(&a_pre.id())), "host must relay a's event to b");

    // live event from b reaches a through the host
    let live = Event::new_chest("b", 10, m);
    b.log.lock().unwrap().insert(live.clone());
    b.handle.send(NetCmd::Publish(live.clone()));
    assert!(wait_for(30, || a.log.lock().unwrap().contains(&live.id())));
    assert!(wait_for(30, || host.log.lock().unwrap().contains(&live.id())));

    // the party total is the sum over everyone
    let (_, total) = a.log.lock().unwrap().chest_counts();
    assert_eq!(total, 3);

    // drain to check a roster with 3 members showed up
    let mut saw_three = false;
    while let Ok(e) = a.rx.try_recv() {
        if let NetEvent::State(s) = e {
            saw_three |= s.members.len() == 3;
        }
    }
    assert!(saw_three, "joiner should see a 3-member roster");

    // kick b
    host.handle.send(NetCmd::Kick(lootline_net::proto::id_to_string(
        iroh::SecretKey::from_bytes(&[3; 32]).public().as_bytes(),
    )));
    let mut kicked = false;
    let end = Instant::now() + Duration::from_secs(20);
    while Instant::now() < end && !kicked {
        if let Ok(NetEvent::Kicked) = b.rx.recv_timeout(Duration::from_millis(200)) {
            kicked = true;
        }
    }
    assert!(kicked, "b should be told it was kicked");
}
