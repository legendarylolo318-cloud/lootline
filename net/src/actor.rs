use crate::proto::{self, Msg, RosterMember, ALPN, MAX_FRAME};
use anyhow::{anyhow, Result};
use iroh::endpoint::{presets, Connection};
use iroh::{Endpoint, EndpointAddr, EndpointId, SecretKey};
use lootline_core::{Event, EventLog};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};

const MAX_DIAL_ATTEMPTS: u32 = 6;

pub struct NetConfig {
    /// 32-byte secret key; its public key is this player's id and party address.
    pub secret: [u8; 32],
}

#[derive(Debug)]
pub enum NetCmd {
    Host,
    Join(String),
    Leave,
    Kick(String),
    /// Send a locally created event (already inserted in the shared log) to the party.
    Publish(Event),
    Shutdown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkState {
    Idle,
    Connecting,
    Online,
    Reconnecting,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PartyState {
    pub link: LinkState,
    pub host: String,
    pub i_am_host: bool,
    /// Code others paste to join the *current* host.
    pub code: String,
    pub members: Vec<RosterMember>,
}

impl PartyState {
    fn idle() -> Self {
        PartyState {
            link: LinkState::Idle,
            host: String::new(),
            i_am_host: false,
            code: String::new(),
            members: vec![],
        }
    }
}

#[derive(Debug)]
pub enum NetEvent {
    /// Emitted once at start: this player's id and the party code others would use to reach them.
    Identity {
        id: String,
        code: String,
    },
    State(PartyState),
    /// Events received from the party that were new to the shared log.
    Merged(Vec<Event>),
    Kicked,
    Notice(String),
}

pub struct NetHandle {
    tx: UnboundedSender<NetCmd>,
}

impl NetHandle {
    pub fn send(&self, c: NetCmd) {
        let _ = self.tx.send(c);
    }
}

impl Drop for NetHandle {
    fn drop(&mut self) {
        let _ = self.tx.send(NetCmd::Shutdown);
    }
}

pub fn spawn(cfg: NetConfig, log: Arc<Mutex<EventLog>>, emit: impl Fn(NetEvent) + Send + Sync + 'static) -> NetHandle {
    let (tx, rx) = unbounded_channel();
    let sk = SecretKey::from_bytes(&cfg.secret);
    let me = proto::id_to_string(sk.public().as_bytes());
    emit(NetEvent::Identity { id: me.clone(), code: proto::make_code(&me) });
    let emit: Arc<dyn Fn(NetEvent) + Send + Sync> = Arc::new(emit);
    let _ = std::thread::Builder::new().name("lootline-net".into()).spawn(move || {
        let Ok(rt) = tokio::runtime::Builder::new_current_thread().enable_all().build() else {
            emit(NetEvent::Notice("could not start the network runtime".into()));
            return;
        };
        let (itx, irx) = unbounded_channel();
        let actor = Actor {
            me,
            sk,
            log,
            emit,
            ep: None,
            itx,
            mode: Mode::Idle,
            host_id: String::new(),
            members: vec![],
            peers: HashMap::new(),
            banned: HashSet::new(),
            dead: HashSet::new(),
            next_gen: 1,
            ever_connected: false,
        };
        rt.block_on(actor.run(rx, irx));
    });
    NetHandle { tx }
}

#[derive(PartialEq, Clone, Copy)]
enum Mode {
    Idle,
    Host,
    Client,
}

struct Peer {
    tx: UnboundedSender<Msg>,
    gen: u64,
}

enum Internal {
    Incoming(Connection),
    Dialed { host: String, attempt: u32, res: Result<Connection, String> },
    Msg { from: String, gen: u64, msg: Msg },
    Closed { from: String, gen: u64 },
    Redial { host: String, attempt: u32 },
}

struct Actor {
    me: String,
    sk: SecretKey,
    log: Arc<Mutex<EventLog>>,
    emit: Arc<dyn Fn(NetEvent) + Send + Sync>,
    ep: Option<Endpoint>,
    itx: UnboundedSender<Internal>,
    mode: Mode,
    host_id: String,
    /// Roster order: host first, then join order.
    members: Vec<String>,
    peers: HashMap<String, Peer>,
    banned: HashSet<String>,
    /// Hosts we failed to reach during handoff.
    dead: HashSet<String>,
    next_gen: u64,
    ever_connected: bool,
}

impl Actor {
    async fn run(mut self, mut cmds: UnboundedReceiver<NetCmd>, mut internal: UnboundedReceiver<Internal>) {
        loop {
            tokio::select! {
                c = cmds.recv() => match c {
                    None | Some(NetCmd::Shutdown) => break,
                    Some(c) => self.on_cmd(c).await,
                },
                Some(i) = internal.recv() => self.on_internal(i).await,
            }
        }
        self.teardown().await;
    }

    fn note(&self, s: impl Into<String>) {
        (self.emit)(NetEvent::Notice(s.into()));
    }

    // ------------------------------------------------------------ state emission
    fn link(&self) -> LinkState {
        match self.mode {
            Mode::Idle => LinkState::Idle,
            Mode::Host => LinkState::Online,
            Mode::Client => {
                if self.peers.contains_key(&self.host_id) {
                    LinkState::Online
                } else if self.ever_connected {
                    LinkState::Reconnecting
                } else {
                    LinkState::Connecting
                }
            }
        }
    }

    fn roster(&self) -> Vec<RosterMember> {
        self.members
            .iter()
            .map(|id| RosterMember { id: id.clone(), connected: *id == self.me || self.peers.contains_key(id) })
            .collect()
    }

    fn emit_state(&self) {
        let st = if self.mode == Mode::Idle {
            PartyState::idle()
        } else {
            PartyState {
                link: self.link(),
                host: self.host_id.clone(),
                i_am_host: self.mode == Mode::Host,
                code: proto::make_code(&self.host_id),
                members: self.roster(),
            }
        };
        (self.emit)(NetEvent::State(st));
    }

    fn broadcast_roster(&self) {
        let m = Msg::Roster { host: self.me.clone(), members: self.roster() };
        for p in self.peers.values() {
            let _ = p.tx.send(m.clone());
        }
        self.emit_state();
    }

    // ------------------------------------------------------------ endpoint
    async fn ensure_endpoint(&mut self) -> Result<()> {
        if self.ep.is_some() {
            return Ok(());
        }
        let ep = Endpoint::builder(presets::N0)
            .secret_key(self.sk.clone())
            .alpns(vec![ALPN.to_vec()])
            .bind()
            .await
            .map_err(|e| anyhow!("{e}"))?;
        let accept_ep = ep.clone();
        let itx = self.itx.clone();
        tokio::spawn(async move {
            while let Some(incoming) = accept_ep.accept().await {
                let itx = itx.clone();
                tokio::spawn(async move {
                    if let Ok(conn) = incoming.await {
                        let _ = itx.send(Internal::Incoming(conn));
                    }
                });
            }
        });
        self.ep = Some(ep);
        Ok(())
    }

    async fn teardown(&mut self) {
        self.peers.clear();
        if let Some(ep) = self.ep.take() {
            ep.close().await;
        }
    }

    fn go_idle(&mut self) {
        self.mode = Mode::Idle;
        self.members.clear();
        self.host_id.clear();
        self.peers.clear();
        self.dead.clear();
        self.ever_connected = false;
        if let Some(ep) = self.ep.take() {
            tokio::spawn(async move { ep.close().await });
        }
        self.emit_state();
    }

    // ------------------------------------------------------------ commands
    async fn on_cmd(&mut self, c: NetCmd) {
        match c {
            NetCmd::Shutdown => {}
            NetCmd::Host => {
                if self.mode != Mode::Idle {
                    return;
                }
                if let Err(e) = self.ensure_endpoint().await {
                    self.note(format!("could not start networking: {e}"));
                    return;
                }
                self.mode = Mode::Host;
                self.host_id = self.me.clone();
                self.members = vec![self.me.clone()];
                self.banned.clear();
                self.emit_state();
            }
            NetCmd::Join(code) => {
                if self.mode != Mode::Idle {
                    return;
                }
                let Some(host) = proto::parse_code(&code) else {
                    self.note("that doesn't look like a party code");
                    return;
                };
                if host == self.me {
                    self.note("that's your own party code");
                    return;
                }
                if let Err(e) = self.ensure_endpoint().await {
                    self.note(format!("could not start networking: {e}"));
                    return;
                }
                self.mode = Mode::Client;
                self.host_id = host.clone();
                self.members = vec![host.clone()];
                self.ever_connected = false;
                self.dead.clear();
                self.emit_state();
                self.dial(host, 0);
            }
            NetCmd::Leave => {
                if self.mode == Mode::Host {
                    let next = proto::next_host(&self.members, &self.me);
                    for p in self.peers.values() {
                        let _ = p.tx.send(Msg::HostLeaving { next: next.clone() });
                    }
                    // give the writer tasks a moment to flush before the connections close
                    self.peers.clear();
                    tokio::time::sleep(Duration::from_millis(400)).await;
                }
                self.go_idle();
            }
            NetCmd::Kick(player) => {
                if self.mode != Mode::Host || player == self.me {
                    return;
                }
                if let Some(p) = self.peers.remove(&player) {
                    let _ = p.tx.send(Msg::Kicked);
                }
                self.banned.insert(player.clone());
                self.members.retain(|m| *m != player);
                self.broadcast_roster();
            }
            NetCmd::Publish(e) => {
                let m = Msg::Events { events: vec![e] };
                match self.mode {
                    Mode::Host => {
                        for p in self.peers.values() {
                            let _ = p.tx.send(m.clone());
                        }
                    }
                    Mode::Client => {
                        if let Some(p) = self.peers.get(&self.host_id) {
                            let _ = p.tx.send(m);
                        }
                    }
                    Mode::Idle => {}
                }
            }
        }
    }

    // ------------------------------------------------------------ dialing
    fn dial(&self, host: String, attempt: u32) {
        let Some(ep) = self.ep.clone() else { return };
        let itx = self.itx.clone();
        tokio::spawn(async move {
            let res = match proto::string_to_id(&host).map(|b| EndpointId::from_bytes(&b)) {
                Some(Ok(id)) => {
                    match tokio::time::timeout(Duration::from_secs(15), ep.connect(EndpointAddr::from(id), ALPN)).await
                    {
                        Ok(Ok(c)) => Ok(c),
                        Ok(Err(e)) => Err(e.to_string()),
                        Err(_) => Err("timed out".to_string()),
                    }
                }
                _ => Err("bad party code".to_string()),
            };
            let _ = itx.send(Internal::Dialed { host, attempt, res });
        });
    }

    fn schedule_redial(&self, host: String, attempt: u32) {
        let itx = self.itx.clone();
        let delay = Duration::from_secs((attempt as u64 + 1).min(5));
        tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            let _ = itx.send(Internal::Redial { host, attempt });
        });
    }

    fn attach(&mut self, id: String, conn: Connection, dialer: bool) {
        let gen = self.next_gen;
        self.next_gen += 1;
        let (tx, rx) = unbounded_channel();
        tokio::spawn(peer_task(conn, dialer, rx, self.itx.clone(), id.clone(), gen));
        self.peers.insert(id, Peer { tx, gen });
    }

    fn send_have(&self, id: &str) {
        let ids = self.log.lock().map(|l| l.ids()).unwrap_or_default();
        if let Some(p) = self.peers.get(id) {
            let _ = p.tx.send(Msg::Have { ids });
        }
    }

    // ------------------------------------------------------------ internal events
    async fn on_internal(&mut self, i: Internal) {
        match i {
            Internal::Incoming(conn) => {
                let id = proto::id_to_string(conn.remote_id().as_bytes());
                // Handoff race: a member noticed the host was gone before we did and is already
                // dialing us as the next host. If we've lost the host too and the handoff rule
                // picks us, take over now instead of rejecting the member.
                if self.mode == Mode::Client
                    && self.members.contains(&id)
                    && !self.peers.contains_key(&self.host_id)
                    && self.ever_connected
                    && proto::should_take_over(&self.members, &self.dead, &self.host_id, &self.me)
                {
                    self.dead.insert(self.host_id.clone());
                    self.handoff(None);
                }
                if self.mode != Mode::Host || self.banned.contains(&id) || id == self.me {
                    conn.close(1u32.into(), b"not accepted");
                    return;
                }
                self.attach(id.clone(), conn, false);
                if !self.members.contains(&id) {
                    self.members.push(id.clone());
                }
                self.send_have(&id);
                self.broadcast_roster();
            }
            Internal::Dialed { host, attempt, res } => {
                if self.mode != Mode::Client || self.host_id != host {
                    return;
                }
                match res {
                    Ok(conn) => {
                        self.ever_connected = true;
                        self.attach(host.clone(), conn, true);
                        self.send_have(&host);
                        self.emit_state();
                    }
                    Err(e) => {
                        if attempt + 1 < MAX_DIAL_ATTEMPTS {
                            self.schedule_redial(host, attempt + 1);
                            self.emit_state();
                        } else {
                            self.host_unreachable(&e);
                        }
                    }
                }
            }
            Internal::Redial { host, attempt } => {
                if self.mode == Mode::Client && self.host_id == host && !self.peers.contains_key(&host) {
                    self.dial(host, attempt);
                }
            }
            Internal::Closed { from, gen } => {
                if self.peers.get(&from).map(|p| p.gen) != Some(gen) {
                    return;
                }
                self.peers.remove(&from);
                match self.mode {
                    Mode::Host => self.broadcast_roster(),
                    Mode::Client if from == self.host_id => {
                        self.emit_state();
                        self.schedule_redial(from, 0);
                    }
                    _ => {}
                }
            }
            Internal::Msg { from, gen, msg } => {
                if self.peers.get(&from).map(|p| p.gen) != Some(gen) {
                    return;
                }
                self.on_msg(from, msg);
            }
        }
    }

    fn on_msg(&mut self, from: String, msg: Msg) {
        match msg {
            Msg::Have { ids } => {
                let have: HashSet<_> = ids.into_iter().collect();
                let missing = self.log.lock().map(|l| l.missing_for(&have)).unwrap_or_default();
                if let Some(p) = self.peers.get(&from) {
                    for m in proto::event_msgs(missing) {
                        let _ = p.tx.send(m);
                    }
                }
            }
            Msg::Events { events } => {
                let added = match self.log.lock() {
                    Ok(mut l) => l.merge(events),
                    Err(_) => return,
                };
                if added.is_empty() {
                    return;
                }
                if self.mode == Mode::Host {
                    for (id, p) in &self.peers {
                        if *id != from {
                            for m in proto::event_msgs(added.clone()) {
                                let _ = p.tx.send(m);
                            }
                        }
                    }
                }
                (self.emit)(NetEvent::Merged(added));
            }
            Msg::Roster { host, members } => {
                if self.mode == Mode::Client && from == self.host_id && host == self.host_id {
                    // keep our own view of "who is connected" from the host
                    self.members = members.iter().map(|m| m.id.clone()).collect();
                    let st = PartyState {
                        link: self.link(),
                        host: self.host_id.clone(),
                        i_am_host: false,
                        code: proto::make_code(&self.host_id),
                        members,
                    };
                    (self.emit)(NetEvent::State(st));
                }
            }
            Msg::Kicked => {
                if self.mode == Mode::Client && from == self.host_id {
                    (self.emit)(NetEvent::Kicked);
                    self.go_idle();
                }
            }
            Msg::HostLeaving { next } => {
                if self.mode == Mode::Client && from == self.host_id {
                    self.peers.remove(&from);
                    self.dead.insert(from.clone());
                    self.handoff(next);
                }
            }
        }
    }

    /// The host is gone (announced or not reachable): pick or follow the next host.
    fn handoff(&mut self, announced: Option<String>) {
        let candidates: Vec<String> = self.members.iter().filter(|m| !self.dead.contains(*m)).cloned().collect();
        let next =
            announced.filter(|n| !self.dead.contains(n)).or_else(|| proto::next_host(&candidates, &self.host_id));
        match next {
            None => {
                self.note("the party host left and nobody could take over");
                self.go_idle();
            }
            Some(n) if n == self.me => {
                self.mode = Mode::Host;
                self.host_id = self.me.clone();
                self.members = {
                    let mut m = vec![self.me.clone()];
                    m.extend(candidates.into_iter().filter(|x| *x != self.me));
                    m
                };
                self.peers.clear();
                self.banned.clear();
                self.note("you are now the party host");
                self.emit_state();
            }
            Some(n) => {
                self.host_id = n.clone();
                self.peers.clear();
                self.ever_connected = true;
                self.emit_state();
                self.dial(n, 0);
            }
        }
    }

    fn host_unreachable(&mut self, why: &str) {
        if !self.ever_connected {
            self.note(format!("couldn't reach that party ({why}). Check the code and that the host is online"));
            self.go_idle();
        } else {
            self.dead.insert(self.host_id.clone());
            self.handoff(None);
        }
    }
}

// ------------------------------------------------------------------ per-connection task

async fn peer_task(
    conn: Connection,
    dialer: bool,
    mut rx: UnboundedReceiver<Msg>,
    itx: UnboundedSender<Internal>,
    id: String,
    gen: u64,
) {
    let streams = if dialer { conn.open_bi().await } else { conn.accept_bi().await };
    let Ok((mut send, mut recv)) = streams else {
        let _ = itx.send(Internal::Closed { from: id, gen });
        return;
    };
    let writer = async {
        while let Some(m) = rx.recv().await {
            if send.write_all(&proto::encode_frame(&m)).await.is_err() {
                return false;
            }
        }
        let _ = send.finish();
        true
    };
    let itx2 = itx.clone();
    let id2 = id.clone();
    let reader = async move {
        loop {
            let mut len = [0u8; 4];
            if recv.read_exact(&mut len).await.is_err() {
                return;
            }
            let n = u32::from_be_bytes(len) as usize;
            if n > MAX_FRAME {
                return;
            }
            let mut body = vec![0u8; n];
            if recv.read_exact(&mut body).await.is_err() {
                return;
            }
            match proto::decode_body(&body) {
                Ok(msg) => {
                    if itx2.send(Internal::Msg { from: id2.clone(), gen, msg }).is_err() {
                        return;
                    }
                }
                Err(_) => return,
            }
        }
    };
    let graceful = tokio::select! {
        g = writer => g,
        _ = reader => false,
    };
    if graceful {
        // we removed the peer on purpose: let queued frames (e.g. Kicked) reach it first
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    conn.close(0u32.into(), b"bye");
    let _ = itx.send(Internal::Closed { from: id, gen });
}
