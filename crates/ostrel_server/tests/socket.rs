//! The WebSocket socket of the server over real TCP connections (ARCHITECTURE 4.1, 5.4, 5.9;
//! SPEC 12.14; amir #2720).
//!
//! Each test starts a server thread (tokio `current_thread` with one `LocalSet`, as the server
//! runs) and drives it with blocking WebSocket clients from the test thread. The server fns are
//! hand written, as in `tests/ws/main.rs`: `bump` is locking and takes the next value of the
//! sequence `bumps` in its transaction, so its result shows how many calls wrote; `count` is
//! not locking and writes nothing.

use std::collections::{HashMap, HashSet};
use std::io::{ErrorKind, Read};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use ostrel_core::ids::{Hlc, ReplicaId, RowId, ServerSeq};
use ostrel_core::value::{SafeInt, Value};
use ostrel_db::api::{
    AppliedSchema, Connection, DbError, Driver, MigrationPlan, NewOp, OpId, Query, ReplayOutcome,
    Rows, StoredOp, Transaction, Write,
};
use ostrel_db_memory::MemoryDriver;
use ostrel_server::audit::MemoryAuditLog;
use ostrel_server::host::Clock;
use ostrel_server::identity::TokenRejected;
use ostrel_server::key_status::{
    KeyId, KeyStatus, KeyStatusBoard, KeyStatusSource, SessionToken, SessionTokenVerifier,
};
use ostrel_server::ws::socket::{Admission, HelloGate, Socket};
use ostrel_server::ws::{
    Admitted, CallDb, CallServer, FnFault, HelloRefused, Hellos, ReplicaStore, ServerFns,
    SessionEnd, SessionLimits, StoreFailed,
};
use ostrel_sync::protocol::{
    CallError, ClientMsg, ErrorCode, FieldId, FnId, Hello, ModelId, ServerMsg, SubId,
};
use ostrel_sync::replica_id::{Entropy, ReplicaRecord, ReplicaRecords, SECRET_BYTES};
use ostrel_sync::wire::{Requests, WireSchema, WireTy, decode_server, encode_client};
use ostrel_vm::Limits;
use ostrel_vm::host::{BoxFuture, ServerHost};
use tokio_tungstenite::tungstenite::{self, Message};

// ---------------------------------------------------------------------------------------------
// Schema and server fns
// ---------------------------------------------------------------------------------------------

const BUMP: FnId = 0;
const COUNT: FnId = 1;

struct Schema;

const FUNCTIONS: [&str; 2] = ["bump", "count"];

impl WireSchema for Schema {
    fn model_id(&self, _: &str) -> Option<ModelId> {
        None
    }
    fn model_name(&self, _: ModelId) -> Option<&str> {
        None
    }
    fn field_id(&self, _: ModelId, _: &str) -> Option<FieldId> {
        None
    }
    fn field_name(&self, _: ModelId, _: FieldId) -> Option<&str> {
        None
    }
    fn field_ty(&self, _: ModelId, _: FieldId) -> Option<&WireTy> {
        None
    }
    fn function_id(&self, name: &str) -> Option<FnId> {
        let at = FUNCTIONS.iter().position(|n| *n == name)?;
        FnId::try_from(at).ok()
    }
    fn function_name(&self, function: FnId) -> Option<&str> {
        FUNCTIONS.get(function as usize).copied()
    }
    fn function_params(&self, function: FnId) -> Option<&[WireTy]> {
        FUNCTIONS.get(function as usize).map(|_| &[][..])
    }
    fn function_result(&self, function: FnId) -> Option<&WireTy> {
        FUNCTIONS.get(function as usize).map(|_| &WireTy::Int)
    }
}

/// Every call of a test returns an `Int`, so every call id decodes as `bump`.
struct AnyCall;

impl Requests for AnyCall {
    fn subscription_model(&self, _: SubId) -> Option<ModelId> {
        None
    }
    fn call_function(&self, _: u32) -> Option<FnId> {
        Some(BUMP)
    }
}

fn int(v: u64) -> Value {
    Value::Int(SafeInt::new(i64::try_from(v).unwrap()).unwrap())
}

struct Fns;

impl ServerFns for Fns {
    fn is_locking(&self, function: FnId) -> bool {
        function == BUMP
    }

    fn admit<'a, 'c>(
        &'a self,
        _tx: &'a mut (dyn Transaction<'c> + 'c),
        _principal: Option<RowId>,
        _function: FnId,
        _args: &'a [Value],
    ) -> BoxFuture<'a, Result<(), CallError>> {
        Box::pin(async { Ok(()) })
    }

    fn invoke<'a, 'c>(
        &'a self,
        db: CallDb<'a, 'c>,
        _host: &'a mut dyn ServerHost,
        function: FnId,
        _args: &'a [Value],
        _limits: Limits,
    ) -> BoxFuture<'a, Result<Value, FnFault>> {
        Box::pin(async move {
            match (function, db) {
                (BUMP, CallDb::Locked(tx)) => {
                    let n = tx.next_in_sequence("bumps").await.unwrap();
                    Ok(int(n))
                }
                (COUNT, CallDb::Read(_)) => Ok(int(7)),
                (f, other) => panic!("fn {f} got {other:?}"),
            }
        })
    }
}

struct TestClock(i64);

impl Clock for TestClock {
    fn now_ms(&mut self) -> SafeInt {
        self.0 += 1;
        SafeInt::new(self.0).unwrap()
    }
}

/// A writer connection whose transactions hand out sequence numbers and never commit.
struct FailingWriter;

struct FailingTx;

fn unsupported<'a, T: 'a>() -> BoxFuture<'a, Result<T, DbError>> {
    Box::pin(async { Err(DbError::Unsupported("test writer")) })
}

impl Connection for FailingWriter {
    fn migrate<'a>(&'a mut self, _: &'a MigrationPlan) -> BoxFuture<'a, Result<(), DbError>> {
        unsupported()
    }
    fn query<'a>(&'a mut self, _: &'a Query) -> BoxFuture<'a, Result<Rows, DbError>> {
        unsupported()
    }
    fn applied_schema<'a>(&'a mut self) -> BoxFuture<'a, Result<Option<AppliedSchema>, DbError>> {
        unsupported()
    }
    fn begin<'c>(&'c mut self) -> BoxFuture<'c, Result<Box<dyn Transaction<'c> + 'c>, DbError>> {
        Box::pin(async { Ok(Box::new(FailingTx) as Box<dyn Transaction<'c> + 'c>) })
    }
}

impl<'c> Transaction<'c> for FailingTx {
    fn query<'a>(&'a mut self, _: &'a Query) -> BoxFuture<'a, Result<Rows, DbError>> {
        unsupported()
    }
    fn apply<'a>(&'a mut self, _: &'a [Write]) -> BoxFuture<'a, Result<(), DbError>> {
        unsupported()
    }
    fn append_ops<'a>(&'a mut self, _: &'a [NewOp]) -> BoxFuture<'a, Result<ServerSeq, DbError>> {
        unsupported()
    }
    fn ops_since<'a>(
        &'a mut self,
        _: ServerSeq,
        _: u32,
    ) -> BoxFuture<'a, Result<Vec<StoredOp>, DbError>> {
        unsupported()
    }
    fn record_reject<'a>(&'a mut self, _: OpId, _: &'a str) -> BoxFuture<'a, Result<(), DbError>> {
        unsupported()
    }
    fn last_seq<'a>(&'a mut self, _: ReplicaId) -> BoxFuture<'a, Result<u32, DbError>> {
        unsupported()
    }
    fn replay_outcome<'a>(
        &'a mut self,
        _: OpId,
    ) -> BoxFuture<'a, Result<Option<ReplayOutcome>, DbError>> {
        unsupported()
    }
    fn next_in_sequence<'a>(&'a mut self, _: &'a str) -> BoxFuture<'a, Result<u64, DbError>> {
        Box::pin(async { Ok(1) })
    }
    fn commit(self: Box<Self>) -> BoxFuture<'c, Result<(), DbError>> {
        Box::pin(async { Err(DbError::Backend("disk full".to_owned())) })
    }
    fn rollback(self: Box<Self>) -> BoxFuture<'c, Result<(), DbError>> {
        Box::pin(async { Ok(()) })
    }
}

// ---------------------------------------------------------------------------------------------
// Identity and replicas
// ---------------------------------------------------------------------------------------------

/// Tokens of the form `user:<n>:<kid>`; everything else is rejected.
struct Tokens;

impl SessionTokenVerifier for Tokens {
    fn verify_session(&self, token: &str) -> Result<SessionToken, TokenRejected> {
        let mut parts = token.split(':');
        match (parts.next(), parts.next(), parts.next(), parts.next()) {
            (Some("user"), Some(n), Some(kid), None) => {
                let n: u16 = n.parse().map_err(|_| TokenRejected("bad user".into()))?;
                let kid = KeyId::new(kid).ok_or_else(|| TokenRejected("bad kid".into()))?;
                Ok(SessionToken { user: user(n), kid })
            }
            _ => Err(TokenRejected("bad token".into())),
        }
    }
}

/// Every key is active; revocation goes through the board.
struct Keys;

impl KeyStatusSource for Keys {
    fn status(&self, _: RowId, _: &KeyId) -> KeyStatus {
        KeyStatus::Active
    }
}

#[derive(Default)]
struct Records {
    live: HashMap<ReplicaId, ReplicaRecord>,
    issued: HashSet<ReplicaId>,
}

impl ReplicaRecords for Records {
    fn find(&self, replica: ReplicaId) -> Option<&ReplicaRecord> {
        self.live.get(&replica)
    }
    fn was_issued(&self, replica: ReplicaId) -> bool {
        self.issued.contains(&replica)
    }
}

impl ReplicaStore for Records {
    fn store(&mut self, record: ReplicaRecord) -> Result<(), StoreFailed> {
        self.issued.insert(record.replica);
        self.live.insert(record.replica, record);
        Ok(())
    }
}

struct Counter(u64);

impl Entropy for Counter {
    fn next_u64(&mut self) -> u64 {
        self.0 += 1;
        self.0
    }
    fn secret(&mut self) -> [u8; SECRET_BYTES] {
        [self.0 as u8; SECRET_BYTES]
    }
}

fn user(n: u16) -> RowId {
    RowId::new(Hlc::new(1_000, n, ReplicaId(7)).unwrap())
}

const NOW: u64 = 1_700_000_000_000;

// ---------------------------------------------------------------------------------------------
// Server thread
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Auth,
    NoAuth,
}

/// Wraps the gate: records the peer address every `Hello` was decided for and, when asked,
/// ends the session from inside the decision, as a revocation between the registration on the
/// board and the `welcome` does.
struct Probe<A> {
    inner: A,
    peers: Arc<Mutex<Vec<IpAddr>>>,
    end_early: Option<ErrorCode>,
}

impl<A: Admission> Admission for Probe<A> {
    fn admit(
        &self,
        hello: &Hello,
        peer: IpAddr,
        now_ms: u64,
        end: Arc<dyn SessionEnd>,
    ) -> Result<Admitted, HelloRefused> {
        self.peers.lock().unwrap().push(peer);
        let admitted = self.inner.admit(hello, peer, now_ms, Arc::clone(&end));
        if let (Ok(_), Some(code)) = (&admitted, self.end_early) {
            end.end(code);
        }
        admitted
    }
}

struct Server {
    addr: SocketAddr,
    peers: Arc<Mutex<Vec<IpAddr>>>,
    clock: Arc<AtomicU64>,
    board: Arc<KeyStatusBoard<Keys>>,
    /// What `Socket::serve` returned, once it returned.
    done: mpsc::Receiver<String>,
}

struct Options {
    mode: Mode,
    limits: SessionLimits,
    end_early: Option<ErrorCode>,
    /// Serve every accepted connection as if it came from this address.
    peer: Option<IpAddr>,
    /// The writer connection fails every commit.
    fail_commits: bool,
    /// The hello timeout of the socket, when not the default.
    hello_timeout: Option<Duration>,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            mode: Mode::Auth,
            limits: SessionLimits::default(),
            end_early: None,
            peer: None,
            fail_commits: false,
            hello_timeout: None,
        }
    }
}

fn start(options: Options) -> Server {
    let peers = Arc::new(Mutex::new(Vec::new()));
    let clock = Arc::new(AtomicU64::new(NOW));
    let board = Arc::new(KeyStatusBoard::new(Keys));
    let (tx, rx) = mpsc::channel();
    let (done_tx, done) = mpsc::channel();
    {
        let peers = Arc::clone(&peers);
        let clock = Arc::clone(&clock);
        let board = Arc::clone(&board);
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let local = tokio::task::LocalSet::new();
            local.block_on(&rt, async move {
                let driver = MemoryDriver::new();
                let writer: Box<dyn Connection> = if options.fail_commits {
                    Box::new(FailingWriter)
                } else {
                    driver.connect("memory:").await.unwrap()
                };
                let reader = driver.connect("memory:").await.unwrap();
                let calls = CallServer::new(
                    Box::new(Fns),
                    writer,
                    reader,
                    Box::new(MemoryAuditLog::new()),
                    Box::new(TestClock(0)),
                    Limits::default(),
                );
                let hellos = Hellos::new(options.limits);
                let log_end = Box::new(|| ServerSeq(0));
                let gate: Box<dyn Admission> = match options.mode {
                    Mode::Auth => Box::new(Probe {
                        inner: HelloGate::auth(
                            hellos,
                            Tokens,
                            board,
                            Records::default(),
                            Counter(0),
                            log_end,
                        ),
                        peers,
                        end_early: options.end_early,
                    }),
                    Mode::NoAuth => Box::new(Probe {
                        inner: HelloGate::no_auth(hellos, Records::default(), Counter(0), log_end),
                        peers,
                        end_early: options.end_early,
                    }),
                };
                let now = Box::new(move || clock.load(Ordering::SeqCst));
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                tx.send(listener.local_addr().unwrap()).unwrap();
                let socket = Socket::new(Box::new(Schema), gate, calls, now);
                if let Some(timeout) = options.hello_timeout {
                    socket.set_hello_timeout(timeout);
                }
                if let Some(peer) = options.peer {
                    loop {
                        let (stream, from) = listener.accept().await.unwrap();
                        let socket = std::rc::Rc::clone(&socket);
                        let from = SocketAddr::new(peer, from.port());
                        tokio::task::spawn_local(socket.serve_stream(stream, from));
                    }
                }
                let failed = socket.serve(listener).await;
                let _ = done_tx.send(failed.to_string());
            });
        });
    }
    let addr = rx.recv_timeout(Duration::from_secs(10)).unwrap();
    Server {
        addr,
        peers,
        clock,
        board,
        done,
    }
}

// ---------------------------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------------------------

struct Client(tungstenite::WebSocket<TcpStream>);

impl Client {
    fn connect(addr: SocketAddr) -> Client {
        let stream = TcpStream::connect(addr).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let (ws, _) = tungstenite::client(format!("ws://{addr}/sync"), stream).unwrap();
        Client(ws)
    }

    fn send(&mut self, frame: &str) {
        self.0.send(Message::text(frame)).unwrap();
    }

    fn recv(&mut self) -> String {
        loop {
            match self.0.read() {
                Ok(Message::Text(text)) => return text.to_string(),
                Ok(Message::Close(_)) => panic!("closed while a frame was expected"),
                Ok(_) => {}
                Err(e) => panic!("read failed while a frame was expected: {e}"),
            }
        }
    }

    fn msg(&mut self) -> ServerMsg {
        let frame = self.recv();
        decode_server(frame.as_bytes(), &Schema, &AnyCall)
            .unwrap_or_else(|e| panic!("undecodable frame {frame}: {e:?}"))
    }

    /// The next frame is the fatal `error` with `code`, then the server closes.
    fn expect_fatal(&mut self, code: ErrorCode) {
        match self.msg() {
            ServerMsg::Error(e) => {
                assert_eq!(e.code, code);
                assert_eq!(e.sub, None);
            }
            other => panic!("expected Error {code:?}, got {other:?}"),
        }
        self.expect_closed();
    }

    /// The server closes without sending another frame.
    fn expect_closed(&mut self) {
        loop {
            match self.0.read() {
                Ok(Message::Text(text)) => panic!("frame after the end: {text}"),
                Ok(_) => {}
                Err(_) => return,
            }
        }
    }

    fn hello(&mut self, token: Option<&str>, replica: Option<ReplicaId>, secret: Option<&str>) {
        let msg = ClientMsg::Hello(Hello {
            token: token.map(str::to_owned),
            replica,
            replica_secret: secret.map(str::to_owned),
            last_server_seq: None,
        });
        self.send(&encode_client(&msg, &Schema).unwrap());
    }

    /// Opens a session; the `welcome` names the replica and, without `auth`, its secret.
    fn open(addr: SocketAddr, token: Option<&str>) -> (Client, ReplicaId, Option<String>) {
        let mut client = Client::connect(addr);
        client.hello(token, None, None);
        match client.msg() {
            ServerMsg::Welcome(w) => (client, w.replica, w.replica_secret),
            other => panic!("expected a welcome, got {other:?}"),
        }
    }

    fn call(&mut self, call: u32, function: &str) -> Result<Value, CallError> {
        self.send(&format!(
            r#"{{"v":0,"t":"call","call":{call},"function":"{function}","args":[]}}"#
        ));
        match self.msg() {
            ServerMsg::Return { call: got, result } => {
                assert_eq!(got, call);
                result
            }
            other => panic!("expected a return, got {other:?}"),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// SPEC 12.14 over the socket
// ---------------------------------------------------------------------------------------------

#[test]
fn ac_34_sessions_per_principal() {
    let server = start(Options::default());
    let mut open = Vec::new();
    for _ in 0..16 {
        open.push(Client::open(server.addr, Some("user:1:k1")));
    }

    // The 17th verified Hello of the same user.
    let mut extra = Client::connect(server.addr);
    extra.hello(Some("user:1:k1"), None, None);
    extra.expect_fatal(ErrorCode::TooManySessions);

    // An invalid token is refused for what it is, not for the limit.
    let mut forged = Client::connect(server.addr);
    forged.hello(Some("forged"), None, None);
    forged.expect_fatal(ErrorCode::Unauthorized);

    // Another user is not limited by the first one.
    let (_other, _, _) = Client::open(server.addr, Some("user:2:k1"));

    // A Hello that supersedes a session at 16 is accepted; the old session gets Superseded.
    let (mut first, replica, _) = open.remove(0);
    let mut again = Client::connect(server.addr);
    again.hello(Some("user:1:k1"), Some(replica), None);
    match again.msg() {
        ServerMsg::Welcome(w) => assert_eq!(w.replica, replica),
        other => panic!("expected a welcome, got {other:?}"),
    }
    first.expect_fatal(ErrorCode::Superseded);
    assert_eq!(again.call(1, "count"), Ok(int(7)));

    // Every other session is still served.
    for (n, (client, _, _)) in open.iter_mut().enumerate() {
        assert_eq!(client.call(1, "count"), Ok(int(7)), "session {n}");
    }
}

#[test]
fn ac_34_call_rate() {
    let server = start(Options::default());
    let (mut a, _, _) = Client::open(server.addr, Some("user:1:k1"));
    let (mut b, _, _) = Client::open(server.addr, Some("user:1:k2"));

    // The burst of 50 is shared by both sessions of the principal; the clock stands still.
    for n in 1..=50u32 {
        let client = if n % 2 == 0 { &mut a } else { &mut b };
        assert_eq!(client.call(n, "bump"), Ok(int(u64::from(n))), "call {n}");
    }
    assert_eq!(a.call(51, "bump"), Err(CallError::Limit));
    assert_eq!(b.call(52, "count"), Err(CallError::Limit));

    // Another principal has its own bucket.
    let (mut c, _, _) = Client::open(server.addr, Some("user:2:k1"));
    assert_eq!(c.call(1, "bump"), Ok(int(51)));

    // After one second 20 calls are back. The refused calls wrote nothing: the sequence goes
    // on at 52.
    server.clock.fetch_add(1_000, Ordering::SeqCst);
    assert_eq!(a.call(53, "bump"), Ok(int(52)));
}

#[test]
fn ac_57_unknown_replica() {
    let server = start(Options::default());
    let mut client = Client::connect(server.addr);
    client.hello(Some("user:1:k1"), Some(ReplicaId(0x1234_5678)), None);
    client.expect_fatal(ErrorCode::Unauthorized);

    let server = start(Options {
        mode: Mode::NoAuth,
        ..Options::default()
    });
    let mut client = Client::connect(server.addr);
    client.hello(None, Some(ReplicaId(0x1234_5678)), Some(&"ab".repeat(16)));
    client.expect_fatal(ErrorCode::Unauthorized);
}

#[test]
fn ac_57_superseded_refused_hello() {
    // Without auth: the replica of an open session with a wrong secret.
    let server = start(Options {
        mode: Mode::NoAuth,
        ..Options::default()
    });
    let (mut open, replica, secret) = Client::open(server.addr, None);
    let secret = secret.unwrap();
    let wrong: String = secret
        .chars()
        .map(|c| if c == '0' { '1' } else { '0' })
        .collect();
    let mut thief = Client::connect(server.addr);
    thief.hello(None, Some(replica), Some(&wrong));
    thief.expect_fatal(ErrorCode::Unauthorized);
    assert_eq!(open.call(1, "count"), Ok(int(7)));

    // With auth: the replica of user 1 named with a token of user 2, and with a forged token.
    let server = start(Options::default());
    let (mut open, replica, _) = Client::open(server.addr, Some("user:1:k1"));
    let mut other = Client::connect(server.addr);
    other.hello(Some("user:2:k1"), Some(replica), None);
    other.expect_fatal(ErrorCode::Unauthorized);
    let mut forged = Client::connect(server.addr);
    forged.hello(Some("forged"), Some(replica), None);
    forged.expect_fatal(ErrorCode::Unauthorized);
    assert_eq!(open.call(1, "count"), Ok(int(7)));
}

// ---------------------------------------------------------------------------------------------
// Transport rules (amir #2720)
// ---------------------------------------------------------------------------------------------

#[test]
fn an_end_before_the_welcome_is_sent_instead_of_the_welcome() {
    let server = start(Options {
        end_early: Some(ErrorCode::Unauthorized),
        ..Options::default()
    });
    let mut client = Client::connect(server.addr);
    client.hello(Some("user:1:k1"), None, None);
    client.expect_fatal(ErrorCode::Unauthorized);
}

#[test]
fn a_revoked_device_key_closes_its_open_session() {
    let server = start(Options::default());
    let (mut revoked, _, _) = Client::open(server.addr, Some("user:1:k1"));
    let (mut kept, _, _) = Client::open(server.addr, Some("user:1:k2"));
    assert_eq!(server.board.revoke(user(1), &KeyId::new("k1").unwrap()), 1);
    revoked.expect_fatal(ErrorCode::Unauthorized);
    assert_eq!(kept.call(1, "count"), Ok(int(7)));
    let mut again = Client::connect(server.addr);
    again.hello(Some("user:1:k1"), None, None);
    again.expect_fatal(ErrorCode::Unauthorized);
}

#[test]
fn an_ipv4_mapped_peer_counts_as_its_ipv4_address() {
    let mapped = IpAddr::V6(Ipv4Addr::new(10, 0, 0, 1).to_ipv6_mapped());
    let server = start(Options {
        mode: Mode::NoAuth,
        limits: SessionLimits::with_sessions_per_ip(1).unwrap(),
        peer: Some(mapped),
        ..Options::default()
    });
    let (_open, _, _) = Client::open(server.addr, None);
    let mut second = Client::connect(server.addr);
    second.hello(None, None, None);
    second.expect_fatal(ErrorCode::TooManySessions);
    let peers = server.peers.lock().unwrap().clone();
    assert_eq!(peers, vec![IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)); 2]);
}

#[test]
fn other_ipv6_peers_count_as_themselves() {
    let v6 = IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1));
    let server = start(Options {
        mode: Mode::NoAuth,
        peer: Some(v6),
        ..Options::default()
    });
    let (_open, _, _) = Client::open(server.addr, None);
    assert_eq!(server.peers.lock().unwrap().clone(), vec![v6]);
}

#[test]
fn a_failed_commit_closes_every_session_and_ends_the_server() {
    let server = start(Options {
        fail_commits: true,
        ..Options::default()
    });
    let (mut writer, _, _) = Client::open(server.addr, Some("user:1:k1"));
    let (mut other, _, _) = Client::open(server.addr, Some("user:2:k1"));
    writer.send(r#"{"v":0,"t":"call","call":1,"function":"bump","args":[]}"#);
    // Nothing of the failed call is answered (D159).
    writer.expect_closed();
    other.expect_closed();
    let failed = server.done.recv_timeout(Duration::from_secs(10)).unwrap();
    assert!(failed.contains("commit failed"), "{failed}");
}

#[test]
fn the_first_frame_must_be_a_hello() {
    let server = start(Options::default());
    let mut client = Client::connect(server.addr);
    client.send(r#"{"v":0,"t":"call","call":1,"function":"count","args":[]}"#);
    client.expect_fatal(ErrorCode::Protocol);
    assert!(server.peers.lock().unwrap().is_empty());
}

#[test]
fn a_message_over_the_frame_limit_is_fatal() {
    let server = start(Options::default());
    let (mut client, _, _) = Client::open(server.addr, Some("user:1:k1"));
    let big = format!(
        r#"{{"v":0,"t":"call","call":1,"function":"count","args":["{}"]}}"#,
        "x".repeat(1024 * 1024)
    );
    client.send(&big);
    client.expect_fatal(ErrorCode::FrameTooLarge);
}

#[test]
fn a_sync_message_is_refused_until_the_sync_engine_serves_it() {
    let server = start(Options::default());
    let (mut client, _, _) = Client::open(server.addr, Some("user:1:k1"));
    client.send(r#"{"v":0,"t":"unsubscribe","sub":1}"#);
    client.expect_fatal(ErrorCode::Protocol);
}

#[test]
fn calls_of_one_session_are_answered_and_a_refused_frame_keeps_the_session() {
    let server = start(Options::default());
    let (mut client, _, _) = Client::open(server.addr, Some("user:1:k1"));
    assert_eq!(client.call(1, "bump"), Ok(int(1)));
    assert_eq!(client.call(2, "nope"), Err(CallError::NotFound));
    assert_eq!(client.call(3, "bump"), Ok(int(2)));
    // A ping is answered by the protocol layer and changes nothing.
    client.0.send(Message::Ping(vec![1, 2, 3].into())).unwrap();
    assert_eq!(client.call(4, "count"), Ok(int(7)));
}

/// A hello timeout far above the waits of the shutdown tests, so only the shutdown can end
/// the connections they hold open.
const NO_HELLO_TIMEOUT: Duration = Duration::from_secs(120);

/// amir #2963: a connection that only opened TCP (still in the upgrade) does not keep a
/// failed commit from ending the server (D159).
#[test]
fn a_tcp_connection_in_the_upgrade_does_not_hold_up_the_shutdown() {
    let server = start(Options {
        fail_commits: true,
        hello_timeout: Some(NO_HELLO_TIMEOUT),
        ..Options::default()
    });
    let _idle = TcpStream::connect(server.addr).unwrap();
    std::thread::sleep(Duration::from_millis(200));
    let (mut writer, _, _) = Client::open(server.addr, Some("user:1:k1"));
    writer.send(r#"{"v":0,"t":"call","call":1,"function":"bump","args":[]}"#);
    writer.expect_closed();
    let r = server.done.recv_timeout(Duration::from_secs(15));
    assert!(
        r.is_ok(),
        "serve did not return within 15 s with an idle TCP connection open"
    );
}

/// amir #2963: an upgraded connection that sends no `hello` does not keep a failed commit from
/// ending the server (D159).
#[test]
fn an_upgraded_connection_without_a_hello_does_not_hold_up_the_shutdown() {
    let server = start(Options {
        fail_commits: true,
        hello_timeout: Some(NO_HELLO_TIMEOUT),
        ..Options::default()
    });
    let mut quiet = Client::connect(server.addr);
    let (mut writer, _, _) = Client::open(server.addr, Some("user:1:k1"));
    writer.send(r#"{"v":0,"t":"call","call":1,"function":"bump","args":[]}"#);
    writer.expect_closed();
    let r = server.done.recv_timeout(Duration::from_secs(15));
    assert!(
        r.is_ok(),
        "serve did not return within 15 s with a connection waiting for its hello"
    );
    // The waiting connection was closed by the server, not left open.
    let started = Instant::now();
    quiet.expect_closed();
    assert!(started.elapsed() < Duration::from_secs(5));
}

/// The upgrade and the `hello` must arrive within the hello timeout; a connection that sends
/// nothing, or nothing after the upgrade, is closed (amir #2963 (1)).
#[test]
fn a_connection_without_a_hello_is_closed_after_the_hello_timeout() {
    let server = start(Options {
        hello_timeout: Some(Duration::from_millis(300)),
        ..Options::default()
    });
    let started = Instant::now();
    let mut idle = TcpStream::connect(server.addr).unwrap();
    idle.set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut byte = [0u8; 1];
    match idle.read(&mut byte) {
        Ok(0) => {}
        Ok(_) => panic!("bytes on a connection that sent no upgrade"),
        Err(e) => assert!(
            !matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut),
            "the idle connection was not closed: {e}"
        ),
    }
    assert!(started.elapsed() < Duration::from_secs(5));

    let started = Instant::now();
    let mut quiet = Client::connect(server.addr);
    quiet.expect_closed();
    let waited = started.elapsed();
    assert!(
        waited >= Duration::from_millis(250),
        "closed after {waited:?}"
    );
    assert!(waited < Duration::from_secs(5), "closed after {waited:?}");

    // A client that sends its hello in time gets its session.
    let (mut client, _, _) = Client::open(server.addr, Some("user:1:k1"));
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(client.call(1, "count"), Ok(int(7)));
}
