//! The WebSocket socket of the server (ARCHITECTURE 4.1, 5.4, 5.9, 11).
//!
//! [`Socket`] accepts TCP connections, performs the WebSocket upgrade and runs one session per
//! connection on the current thread: tokio `current_thread` with one `LocalSet` (4.1), so
//! every task of this module is spawned with [`tokio::task::spawn_local`]. The WebSocket
//! protocol state is the `tungstenite` of `tokio-tungstenite` (ARCHITECTURE 11); its reads and
//! writes go through an in memory [`Pipe`] that this module fills from and drains to the tokio
//! stream, so no other crate is needed to drive it.
//!
//! # One connection
//!
//! 1. Upgrade. Any request path is accepted; the HTTP side of the server (app shell, HTTP
//!    requests) is not served here. ASSUMPTION: the socket listens on a port of its own;
//!    whether it moves behind the HTTP upgrade of ARCHITECTURE 11 on a shared port is open
//!    with the architect (amir #2960, chief).
//! 2. The first message must be a `hello` ([`read_hello`]). The upgrade and this first message
//!    together must arrive within [`HELLO_TIMEOUT`] of the accept, otherwise the connection is
//!    closed; until then it counts against no session limit, so the bound keeps a client that
//!    opens connections and sends nothing from holding them (amir #2963). It is decided by the
//!    [`Admission`] of the process with the peer address after [`normalize_peer`]. A refusal
//!    gets its fatal `error` frame and the connection closes; a refusal without a code (a
//!    server defect or a failed store) closes without a frame.
//! 3. The [`SessionEnd`] of the session exists before the `Hello` is decided. A device key
//!    revocation can end the session after the board registered it but before the `welcome`
//!    is written (amir #2720 (a), `hello.rs` "open_session"). The transport then sends that
//!    fatal `error` instead of the `welcome`, never both.
//! 4. Every later frame goes to [`Session::receive`]. A `call` takes one unit from the call
//!    bucket of its principal first ([`SessionTicket::admit_call`]); over the rate it gets
//!    `CallError` `Limit` and the fn does not run. An admitted call runs in a task of its own,
//!    so a session can have several calls open and keeps reading while they run.
//! 5. A message over the frame limit of 5.9 is the fatal `Error FrameTooLarge`.
//!
//! # Shutdown
//!
//! A `commit` that fails ends the process (D159): the call that saw it is not answered, every
//! connection is closed, and [`Socket::serve`] returns the [`CommitFailed`] once all of them
//! are gone; the binary then exits with code 75. Every phase of a connection watches for this:
//! the upgrade, the wait for the `Hello`, the session and every frame it writes. The close
//! frame itself is written for at most [`CLOSE_TIMEOUT`], so a peer that does not read cannot
//! hold the process (amir #2963).
//!
//! The reads and writes of a connection are cancel safe: a frame already read waits in the
//! connection until it is taken, and bytes not yet written stay queued in order, so a session
//! that turns to another event while a write waits loses neither (amir #2963 (2)).
//!
//! # Addresses
//!
//! Sessions and calls without `auth` count per peer address. An IPv4 client reached over an
//! IPv6 socket arrives as an IPv4 mapped IPv6 address, so [`normalize_peer`] turns it into the
//! IPv4 address first; otherwise one client would have two counters (amir #2720 (b)). Every
//! other IPv6 address counts as itself (a /128).
//!
//! ASSUMPTION: until the sync engine serves them (T215), a `subscribe`, `unsubscribe`, `held`
//! or `ops` message is answered with the fatal `Error Protocol`: the server cannot serve it,
//! and an answer that never comes would leave the client waiting without a reason.
//!
//! ASSUMPTION: [`HELLO_TIMEOUT`] is 10 s until the architect fixes the value (amir #2960).
//!
//! ASSUMPTION: the send queue limit of 8 MiB (5.9) is wired by T191; until then a frame is
//! written before the connection reads on, so a client that does not read stops its own
//! session only.

use std::cell::{Cell, RefCell};
use std::fmt;
use std::future::Future;
use std::io::{self, Read, Write};
use std::net::{IpAddr, SocketAddr};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use ostrel_core::value::Value;
use ostrel_sync::protocol::{CallError, CallId, ErrorCode, Hello, LIMITS, ServerSeq};
use ostrel_sync::replica_id::Entropy;
use ostrel_sync::wire::WireSchema;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinSet;
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::handshake::HandshakeError;
use tokio_tungstenite::tungstenite::protocol::{WebSocket, WebSocketConfig};
use tokio_tungstenite::tungstenite::{self, Message};

use super::call::{CallServer, CommitFailed};
use super::hello::{
    Admitted, HelloInput, HelloRefused, Hellos, Identity, ReplicaStore, SessionEnd, SessionLimits,
    SessionTicket, fatal_frame, read_hello, welcome_frame,
};
use super::session::{Incoming, Session};
use crate::identity::TokenRejected;
use crate::key_status::{
    KeyId, KeyStatus, KeyStatusBoard, KeyStatusSource, SessionToken, SessionTokenVerifier,
};

/// Bytes read from the stream in one step.
const READ_CHUNK: usize = 16 * 1024;

/// The time a connection has from its accept until its `Hello` is read: the upgrade and the
/// first message together. [`Socket::set_hello_timeout`] changes it for one socket.
pub const HELLO_TIMEOUT: Duration = Duration::from_secs(10);

/// The time the close frame of a connection may take to be written.
pub const CLOSE_TIMEOUT: Duration = Duration::from_secs(1);

/// The pause after a failed `accept` (for example too many open files), so the accept loop
/// does not spin while the cause lasts.
const ACCEPT_PAUSE: Duration = Duration::from_millis(100);

/// The address a connection counts against: an IPv4 mapped IPv6 address is the IPv4 address
/// it carries; every other address is itself.
pub fn normalize_peer(peer: IpAddr) -> IpAddr {
    peer.to_canonical()
}

/// The session limits for the value of `--max-sessions-per-ip`. They are built only through
/// [`SessionLimits::with_sessions_per_ip`], so no option value gives a limit of zero
/// (amir #2720 (d)). `Err` is the reason for the usage error.
pub fn sessions_per_ip_option(value: &str) -> Result<SessionLimits, String> {
    let n: usize = value.parse().map_err(|_| {
        format!("--max-sessions-per-ip needs a positive whole number, got `{value}`")
    })?;
    SessionLimits::with_sessions_per_ip(n)
        .ok_or_else(|| "--max-sessions-per-ip must be at least 1".to_owned())
}

/// Decides the `Hello` of a connection. [`HelloGate`] is the implementation of the server; the
/// trait keeps the socket free of the types of the token verifier and the stores.
pub trait Admission {
    /// Decides `hello` from `peer` (already normalized) at `now_ms`. `end` closes the session
    /// later from outside; it may be called before this returns.
    fn admit(
        &self,
        hello: &Hello,
        peer: IpAddr,
        now_ms: u64,
        end: Arc<dyn SessionEnd>,
    ) -> Result<Admitted, HelloRefused>;
}

/// The token verifier of an app without `auth`: it has no value, so it is never asked.
#[derive(Clone, Copy, Debug)]
pub enum NoTokens {}

impl SessionTokenVerifier for NoTokens {
    fn verify_session(&self, _token: &str) -> Result<SessionToken, TokenRejected> {
        match *self {}
    }
}

impl KeyStatusSource for NoTokens {
    fn status(&self, _user: ostrel_core::ids::RowId, _kid: &KeyId) -> KeyStatus {
        match *self {}
    }
}

/// The admission of a server process: [`Hellos`] with the identity of the app, the replica
/// records, the entropy for new replicas and the end of the op log.
pub struct HelloGate<R, E, V = NoTokens, S = NoTokens> {
    hellos: Hellos,
    auth: Option<(V, Arc<KeyStatusBoard<S>>)>,
    records: RefCell<R>,
    entropy: RefCell<E>,
    log_end: Box<dyn Fn() -> ServerSeq>,
}

impl<R, E> HelloGate<R, E> {
    /// The admission of an app without `auth`.
    pub fn no_auth(
        hellos: Hellos,
        records: R,
        entropy: E,
        log_end: Box<dyn Fn() -> ServerSeq>,
    ) -> Self {
        HelloGate {
            hellos,
            auth: None,
            records: RefCell::new(records),
            entropy: RefCell::new(entropy),
            log_end,
        }
    }
}

impl<R, E, V, S> HelloGate<R, E, V, S> {
    /// The admission of an app with `auth`: tokens are verified by `tokens`, the status of
    /// their device key is checked on `board`, which the revocation path shares.
    pub fn auth(
        hellos: Hellos,
        tokens: V,
        board: Arc<KeyStatusBoard<S>>,
        records: R,
        entropy: E,
        log_end: Box<dyn Fn() -> ServerSeq>,
    ) -> Self {
        HelloGate {
            hellos,
            auth: Some((tokens, board)),
            records: RefCell::new(records),
            entropy: RefCell::new(entropy),
            log_end,
        }
    }

    /// The open sessions and limits.
    pub fn hellos(&self) -> &Hellos {
        &self.hellos
    }

    /// The replica records.
    pub fn records(&self) -> std::cell::Ref<'_, R> {
        self.records.borrow()
    }
}

impl<R, E, V, S> Admission for HelloGate<R, E, V, S>
where
    R: ReplicaStore,
    E: Entropy,
    V: SessionTokenVerifier,
    S: KeyStatusSource,
{
    fn admit(
        &self,
        hello: &Hello,
        peer: IpAddr,
        now_ms: u64,
        end: Arc<dyn SessionEnd>,
    ) -> Result<Admitted, HelloRefused> {
        let identity = match &self.auth {
            Some((tokens, board)) => Identity::Auth {
                tokens,
                board: board.as_ref(),
            },
            None => Identity::NoAuth,
        };
        // `admit` never awaits, so these borrows end before any other connection runs.
        let mut records = self.records.borrow_mut();
        let mut entropy = self.entropy.borrow_mut();
        self.hellos.admit(
            identity,
            HelloInput {
                hello,
                peer,
                records: &mut *records,
                entropy: &mut *entropy,
                now_ms,
                log_end: (self.log_end)(),
                end,
            },
        )
    }
}

impl<R, E, V, S> fmt::Debug for HelloGate<R, E, V, S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HelloGate")
            .field("hellos", &self.hellos)
            .field("auth", &self.auth.is_some())
            .finish_non_exhaustive()
    }
}

/// What every connection of one server process shares.
pub struct Socket {
    schema: Box<dyn WireSchema>,
    admission: Box<dyn Admission>,
    calls: CallServer,
    clock: Box<dyn Fn() -> u64>,
    failed: RefCell<Option<CommitFailed>>,
    shutdown: watch::Sender<bool>,
    hello_timeout: Cell<Duration>,
}

impl fmt::Debug for Socket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Socket")
            .field("calls", &self.calls)
            .field("failed", &self.failed.borrow().is_some())
            .finish_non_exhaustive()
    }
}

impl Socket {
    /// The socket of a server process. `clock` gives the server time in ms since the epoch
    /// for `Hello` and the call rate.
    pub fn new(
        schema: Box<dyn WireSchema>,
        admission: Box<dyn Admission>,
        calls: CallServer,
        clock: Box<dyn Fn() -> u64>,
    ) -> Rc<Self> {
        let (shutdown, _) = watch::channel(false);
        Rc::new(Socket {
            schema,
            admission,
            calls,
            clock,
            failed: RefCell::new(None),
            shutdown,
            hello_timeout: Cell::new(HELLO_TIMEOUT),
        })
    }

    /// Sets the time a connection has from its accept until its `Hello` is read, for the
    /// connections accepted from now on.
    pub fn set_hello_timeout(&self, timeout: Duration) {
        self.hello_timeout.set(timeout);
    }

    /// The call server of the process.
    pub fn calls(&self) -> &CallServer {
        &self.calls
    }

    /// Accepts connections on `listener` until a `commit` fails, then closes every connection
    /// and returns why. An error of `accept` itself (for example too many open files) skips
    /// that connection and keeps accepting after a pause.
    pub async fn serve(self: Rc<Self>, listener: TcpListener) -> CommitFailed {
        let mut shutdown = self.shutdown.subscribe();
        let mut connections = JoinSet::new();
        let failed = loop {
            let taken = self.failed.borrow_mut().take();
            if let Some(failed) = taken {
                break failed;
            }
            tokio::select! {
                accepted = listener.accept() => match accepted {
                    Ok((stream, peer)) => {
                        let socket = Rc::clone(&self);
                        connections.spawn_local(async move {
                            socket.serve_stream(stream, peer).await;
                        });
                    }
                    Err(_) => tokio::time::sleep(ACCEPT_PAUSE).await,
                },
                _ = shutdown.changed() => {}
                // Finished connections are collected here so the set does not grow.
                Some(_) = connections.join_next(), if !connections.is_empty() => {}
            }
        };
        drop(listener);
        while connections.join_next().await.is_some() {}
        failed
    }

    /// Serves one accepted connection from `peer`: upgrade, `Hello`, then the session until
    /// either side closes it. The upgrade and the `Hello` end early when the hello timeout
    /// passes or the process shuts down.
    pub async fn serve_stream<IO>(self: Rc<Self>, io: IO, peer: SocketAddr)
    where
        IO: AsyncRead + AsyncWrite + Unpin,
    {
        let mut shutdown = self.shutdown.subscribe();
        let deadline = Instant::now() + self.hello_timeout.get();
        // An upgrade cut short drops the stream, which closes it.
        let Some(Some(mut conn)) = before_hello(&mut shutdown, deadline, Conn::upgrade(io)).await
        else {
            return;
        };
        let hello = before_hello(&mut shutdown, deadline, self.hello(&mut conn, peer.ip())).await;
        let Some(Some((session, ticket, ends))) = hello else {
            conn.close().await;
            return;
        };
        self.run(conn, session, ticket, ends, shutdown).await;
    }

    /// Reads and decides the `Hello`. `None` means the connection is to be closed; every frame
    /// it needed has been sent.
    async fn hello<IO>(
        &self,
        conn: &mut Conn<IO>,
        peer: IpAddr,
    ) -> Option<(Session, SessionTicket, mpsc::UnboundedReceiver<ErrorCode>)>
    where
        IO: AsyncRead + AsyncWrite + Unpin,
    {
        let frame = match conn.next_frame().await {
            Frame::Data(frame) => frame,
            Frame::TooLarge => {
                let _ = conn
                    .send(fatal_frame(ErrorCode::FrameTooLarge, &*self.schema))
                    .await;
                return None;
            }
            Frame::Closed => return None,
        };
        let hello = match read_hello(&frame, &*self.schema) {
            Ok(hello) => hello,
            Err(refusal) => {
                let _ = conn.send(refusal).await;
                return None;
            }
        };
        let (tx, mut ends) = mpsc::unbounded_channel();
        let end: Arc<dyn SessionEnd> = Arc::new(ChannelEnd(tx));
        let admitted = self
            .admission
            .admit(&hello, normalize_peer(peer), (self.clock)(), end);
        let Admitted {
            welcome,
            principal,
            ticket,
        } = match admitted {
            Ok(admitted) => admitted,
            Err(HelloRefused::Error(code)) => {
                let _ = conn.send(fatal_frame(code, &*self.schema)).await;
                return None;
            }
            Err(HelloRefused::EntropyExhausted | HelloRefused::Store(_)) => return None,
        };
        // An end that came while the `Hello` was decided wins over the `welcome`.
        if let Ok(code) = ends.try_recv() {
            let _ = conn.send(fatal_frame(code, &*self.schema)).await;
            drop(ticket);
            return None;
        }
        conn.send(welcome_frame(&welcome, &*self.schema))
            .await
            .ok()?;
        Some((Session::new(principal), ticket, ends))
    }

    /// The session after its `welcome`.
    async fn run<IO>(
        self: Rc<Self>,
        mut conn: Conn<IO>,
        mut session: Session,
        ticket: SessionTicket,
        mut ends: mpsc::UnboundedReceiver<ErrorCode>,
        mut shutdown: watch::Receiver<bool>,
    ) where
        IO: AsyncRead + AsyncWrite + Unpin,
    {
        let (done_tx, mut done) = mpsc::unbounded_channel::<Finished>();
        let mut calls = JoinSet::new();
        // Every sender of `ends` may be dropped while the session lives on (the board and the
        // registry let go of it); a closed channel is then never polled again.
        let mut ends_open = true;
        loop {
            // Frames already read are handled before the connection waits again.
            let event = match conn.buffered() {
                Some(frame) => Event::Frame(frame),
                None => tokio::select! {
                    biased;
                    _ = shutdown.changed() => Event::Shutdown,
                    code = ends.recv(), if ends_open => match code {
                        Some(code) => Event::End(code),
                        None => {
                            ends_open = false;
                            continue;
                        }
                    },
                    finished = done.recv() => finished.map_or(Event::Shutdown, Event::Done),
                    frame = conn.next_frame() => Event::Frame(frame),
                },
            };
            let step = match event {
                Event::Shutdown => Step::Close,
                Event::End(code) => {
                    let _ = send(&mut conn, &mut shutdown, fatal_frame(code, &*self.schema)).await;
                    Step::Close
                }
                Event::Done((call, result)) => {
                    match session.finish(call, result, &*self.schema) {
                        Ok(frame) => send(&mut conn, &mut shutdown, frame).await,
                        // A result that does not fit its type is a server defect; the client
                        // could not trust the session any more.
                        Err(_) => Step::Close,
                    }
                }
                Event::Frame(Frame::Closed) => Step::Close,
                Event::Frame(Frame::TooLarge) => {
                    let frame = fatal_frame(ErrorCode::FrameTooLarge, &*self.schema);
                    let _ = send(&mut conn, &mut shutdown, frame).await;
                    Step::Close
                }
                Event::Frame(Frame::Data(frame)) => {
                    match session.receive(&frame, &*self.schema) {
                        Incoming::Reply(frame) => send(&mut conn, &mut shutdown, frame).await,
                        Incoming::Close(frame) => {
                            let _ = send(&mut conn, &mut shutdown, frame).await;
                            Step::Close
                        }
                        Incoming::Sync(_) => {
                            let frame = fatal_frame(ErrorCode::Protocol, &*self.schema);
                            let _ = send(&mut conn, &mut shutdown, frame).await;
                            Step::Close
                        }
                        Incoming::Call(job) => match ticket.admit_call((self.clock)()) {
                            Err(limit) => match session.finish(job.call, Err(limit), &*self.schema)
                            {
                                Ok(frame) => send(&mut conn, &mut shutdown, frame).await,
                                Err(_) => Step::Close,
                            },
                            Ok(()) => {
                                let socket = Rc::clone(&self);
                                let done_tx = done_tx.clone();
                                calls.spawn_local(async move {
                                    let result = socket
                                        .calls
                                        .call(job.principal, job.function, &job.args)
                                        .await;
                                    match result {
                                        Ok(result) => {
                                            // The session may be gone; then nobody waits.
                                            let _ = done_tx.send((job.call, result));
                                        }
                                        Err(failed) => socket.fail(failed),
                                    }
                                });
                                Step::Go
                            }
                        },
                    }
                }
            };
            // Finished call tasks are collected so the set does not grow with the session.
            while calls.try_join_next().is_some() {}
            if let Step::Close = step {
                break;
            }
        }
        conn.close().await;
        drop(ticket);
        // Calls still running finish in their tasks; their results have nobody to go to.
        calls.detach_all();
    }

    /// Records a failed `commit` and tells every connection to close (D159).
    fn fail(&self, failed: CommitFailed) {
        let mut slot = self.failed.borrow_mut();
        if slot.is_none() {
            *slot = Some(failed);
        }
        drop(slot);
        self.shutdown.send_replace(true);
    }
}

/// A call result for the session: the call id and its result.
type Finished = (CallId, Result<Value, CallError>);

enum Event {
    Shutdown,
    End(ErrorCode),
    Done(Finished),
    Frame(Frame),
}

enum Step {
    Go,
    Close,
}

/// Writes `frame` unless the process shuts down first; `Close` when the session is to end.
async fn send<IO>(conn: &mut Conn<IO>, shutdown: &mut watch::Receiver<bool>, frame: String) -> Step
where
    IO: AsyncRead + AsyncWrite + Unpin,
{
    tokio::select! {
        biased;
        _ = shutdown.wait_for(|stop| *stop) => Step::Close,
        sent = conn.send(frame) => match sent {
            Ok(()) => Step::Go,
            Err(_) => Step::Close,
        },
    }
}

/// Runs `phase` of a connection before its session: `None` when the process shuts down first
/// or `deadline` passes first.
async fn before_hello<F: Future>(
    shutdown: &mut watch::Receiver<bool>,
    deadline: Instant,
    phase: F,
) -> Option<F::Output> {
    tokio::select! {
        biased;
        _ = shutdown.wait_for(|stop| *stop) => None,
        done = tokio::time::timeout_at(deadline, phase) => done.ok(),
    }
}

/// Passes an end from outside to the task of the connection.
struct ChannelEnd(mpsc::UnboundedSender<ErrorCode>);

impl SessionEnd for ChannelEnd {
    fn end(&self, code: ErrorCode) {
        // A connection that is already gone needs no end.
        let _ = self.0.send(code);
    }
}

/// One received message.
#[derive(Debug)]
enum Frame {
    /// A text or binary message.
    Data(Vec<u8>),
    /// A message over the frame limit.
    TooLarge,
    /// The peer closed, the stream failed or the peer broke the WebSocket protocol.
    Closed,
}

/// The bytes between the WebSocket state and the tokio stream: reads take what the stream
/// gave so far and say `WouldBlock` when that is used up; writes collect what is to be sent.
#[derive(Default)]
struct Pipe {
    input: Vec<u8>,
    at: usize,
    output: Vec<u8>,
}

impl Read for Pipe {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let mut rest = self.input.get(self.at..).unwrap_or_default();
        if rest.is_empty() {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        let n = Read::read(&mut rest, buf)?;
        self.at += n;
        if self.at >= self.input.len() {
            self.input.clear();
            self.at = 0;
        }
        Ok(n)
    }
}

impl Write for Pipe {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.output.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// The frame limit of ARCHITECTURE 5.9 for messages and frames of the WebSocket layer, so a
/// larger message is refused while it is read, before it is held in memory.
fn ws_config() -> WebSocketConfig {
    let limit = usize::try_from(LIMITS.frame_bytes).unwrap_or(usize::MAX);
    WebSocketConfig::default()
        .max_message_size(Some(limit))
        .max_frame_size(Some(limit))
}

/// A WebSocket connection over a tokio stream.
struct Conn<IO> {
    io: IO,
    ws: WebSocket<Pipe>,
    chunk: Box<[u8]>,
    /// A frame read from the pipe that was not handed out yet, because the write before it
    /// was cut short.
    pending: Option<Frame>,
}

impl<IO> Conn<IO>
where
    IO: AsyncRead + AsyncWrite + Unpin,
{
    fn new(io: IO, ws: WebSocket<Pipe>, chunk: Box<[u8]>) -> Self {
        Conn {
            io,
            ws,
            chunk,
            pending: None,
        }
    }

    /// The server side of the upgrade handshake. `None` when the peer did not complete it.
    async fn upgrade(mut io: IO) -> Option<Self> {
        let mut chunk = vec![0u8; READ_CHUNK].into_boxed_slice();
        let mut round = tungstenite::accept_with_config(Pipe::default(), Some(ws_config()));
        loop {
            match round {
                Ok(mut ws) => {
                    let out = std::mem::take(&mut ws.get_mut().output);
                    io.write_all(&out).await.ok()?;
                    io.flush().await.ok()?;
                    return Some(Conn::new(io, ws, chunk));
                }
                Err(HandshakeError::Interrupted(mut mid)) => {
                    let pipe = mid.get_mut().get_mut();
                    let out = std::mem::take(&mut pipe.output);
                    io.write_all(&out).await.ok()?;
                    io.flush().await.ok()?;
                    let n = io.read(&mut chunk).await.ok()?;
                    let read = chunk.get(..n).filter(|r| !r.is_empty())?;
                    mid.get_mut().get_mut().input.extend_from_slice(read);
                    round = mid.handshake();
                }
                Err(HandshakeError::Failure(_)) => return None,
            }
        }
    }

    /// A message that is already read completely, without waiting. Control frames before it
    /// are answered by the protocol state (a `pong` for a `ping`, queued in the pipe) and
    /// skipped.
    fn buffered(&mut self) -> Option<Frame> {
        if let Some(frame) = self.pending.take() {
            return Some(frame);
        }
        loop {
            match self.ws.read() {
                Ok(message) => {
                    if let Some(frame) = data(message) {
                        return Some(frame);
                    }
                }
                Err(tungstenite::Error::Io(e)) if e.kind() == io::ErrorKind::WouldBlock => {
                    return None;
                }
                Err(e) => return Some(failed(&e)),
            }
        }
    }

    /// The next text or binary message. Cancel safe: a frame read before an answer to a
    /// control frame could be written is kept in `pending` for the next call.
    async fn next_frame(&mut self) -> Frame {
        loop {
            if self.pending.is_none() {
                self.pending = self.buffered();
            }
            // Answers to control frames wait in the pipe until written here.
            if self.write_out().await.is_err() {
                self.pending = None;
                return Frame::Closed;
            }
            if let Some(frame) = self.pending.take() {
                return frame;
            }
            let n = match self.io.read(&mut self.chunk).await {
                Ok(n) => n,
                Err(_) => return Frame::Closed,
            };
            match self.chunk.get(..n) {
                Some(read) if !read.is_empty() => {
                    self.ws.get_mut().input.extend_from_slice(read);
                }
                _ => return Frame::Closed,
            }
        }
    }

    /// Writes one text frame.
    async fn send(&mut self, frame: String) -> io::Result<()> {
        self.ws
            .send(Message::text(frame))
            .map_err(|e| io::Error::other(e.to_string()))?;
        self.write_out().await
    }

    /// Writes what the pipe holds. Cancel safe: bytes leave the pipe only once the stream
    /// took them, so a cut write goes on where it stopped.
    async fn write_out(&mut self) -> io::Result<()> {
        let pipe = self.ws.get_mut();
        if pipe.output.is_empty() {
            return Ok(());
        }
        while !pipe.output.is_empty() {
            let n = self.io.write(&pipe.output).await?;
            if n == 0 {
                return Err(io::ErrorKind::WriteZero.into());
            }
            pipe.output.drain(..n.min(pipe.output.len()));
        }
        self.io.flush().await
    }

    /// Sends a close frame and shuts the stream down, for at most [`CLOSE_TIMEOUT`]. The
    /// answer of the peer is not awaited.
    async fn close(mut self) {
        let _ = self.ws.close(None);
        let _ = self.ws.flush();
        let _ = tokio::time::timeout(CLOSE_TIMEOUT, async {
            let _ = self.write_out().await;
            let _ = self.io.shutdown().await;
        })
        .await;
    }
}

fn data(message: Message) -> Option<Frame> {
    match message {
        Message::Text(_) | Message::Binary(_) => Some(Frame::Data(message.into_data().to_vec())),
        Message::Close(_) => Some(Frame::Closed),
        Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => None,
    }
}

fn failed(error: &tungstenite::Error) -> Frame {
    match error {
        tungstenite::Error::Capacity(_) => Frame::TooLarge,
        _ => Frame::Closed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    #[test]
    fn an_ipv4_mapped_address_counts_as_its_ipv4_address() {
        let v4 = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));
        let mapped = IpAddr::V6(Ipv4Addr::new(10, 0, 0, 1).to_ipv6_mapped());
        assert_eq!(normalize_peer(mapped), v4);
        assert_eq!(normalize_peer(v4), v4);
    }

    #[test]
    fn other_ipv6_addresses_count_as_themselves() {
        let v6 = IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1));
        assert_eq!(normalize_peer(v6), v6);
        // The IPv4 compatible form (deprecated) is not a mapped address.
        let compat = IpAddr::V6(Ipv4Addr::new(10, 0, 0, 1).to_ipv6_compatible());
        assert_eq!(normalize_peer(compat), compat);
    }

    #[test]
    fn the_option_never_gives_a_limit_of_zero() {
        assert!(sessions_per_ip_option("0").is_err());
        assert!(sessions_per_ip_option("-1").is_err());
        assert!(sessions_per_ip_option("many").is_err());
        assert!(sessions_per_ip_option("").is_err());
        let limits = sessions_per_ip_option("3");
        assert_eq!(limits.map(|l| l.per_ip), Ok(3));
        assert_eq!(
            sessions_per_ip_option("3").map(|l| l.per_principal),
            Ok(SessionLimits::default().per_principal)
        );
    }

    #[test]
    fn the_pipe_says_would_block_when_empty_and_keeps_order() {
        let mut pipe = Pipe::default();
        let mut buf = [0u8; 4];
        assert_eq!(
            pipe.read(&mut buf).map_err(|e| e.kind()),
            Err(io::ErrorKind::WouldBlock)
        );
        pipe.input.extend_from_slice(b"abcdef");
        assert_eq!(pipe.read(&mut buf).ok(), Some(4));
        assert_eq!(&buf, b"abcd");
        assert_eq!(pipe.read(&mut buf).ok(), Some(2));
        assert_eq!(buf.get(..2), Some(&b"ef"[..]));
        assert!(pipe.input.is_empty());
        assert_eq!(pipe.write(b"xy").ok(), Some(2));
        assert_eq!(pipe.output, b"xy");
    }

    use tokio::io::DuplexStream;
    use tokio_tungstenite::tungstenite::protocol::Role;

    /// A masked client frame (mask key 0, so the payload stays readable).
    fn client_frame(opcode: u8, payload: &[u8]) -> Vec<u8> {
        let len = u8::try_from(payload.len()).unwrap_or(u8::MAX);
        assert!(len < 126, "short payloads only");
        let mut frame = vec![0x80 | opcode, 0x80 | len, 0, 0, 0, 0];
        frame.extend_from_slice(payload);
        frame
    }

    /// A server connection over a duplex stream that holds at most 64 bytes in flight.
    fn duplex_conn() -> (Conn<DuplexStream>, DuplexStream) {
        let (server, peer) = tokio::io::duplex(64);
        let ws = WebSocket::from_raw_socket(Pipe::default(), Role::Server, Some(ws_config()));
        let chunk = vec![0u8; READ_CHUNK].into_boxed_slice();
        (Conn::new(server, ws, chunk), peer)
    }

    fn block_on<F: Future<Output = ()>>(f: F) {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build();
        assert!(rt.is_ok(), "no runtime");
        if let Ok(rt) = rt {
            rt.block_on(f);
        }
    }

    #[test]
    fn a_frame_after_a_ping_is_not_hidden_by_the_ping() {
        let (mut conn, _peer) = duplex_conn();
        let input = &mut conn.ws.get_mut().input;
        input.extend(client_frame(0x9, b"p"));
        input.extend(client_frame(0x1, b"hi"));
        assert!(matches!(conn.buffered(), Some(Frame::Data(d)) if d == b"hi"));
        // The pong for the ping waits in the pipe.
        assert_eq!(conn.ws.get_mut().output.first(), Some(&0x8A));
    }

    #[test]
    fn a_frame_read_while_a_write_waits_is_kept_and_no_byte_is_lost() {
        block_on(async {
            let (mut conn, mut peer) = duplex_conn();
            // More than the stream holds while the peer does not read, as a queued pong.
            let queued: Vec<u8> = (0..4096u32).map(|i| (i % 251) as u8).collect();
            conn.ws.get_mut().output = queued.clone();
            conn.ws.get_mut().input.extend(client_frame(0x1, b"hi"));
            // The session turns to another event while the write waits.
            let cut = tokio::time::timeout(Duration::from_millis(50), conn.next_frame()).await;
            assert!(cut.is_err(), "the write must wait for the peer");
            let drain = async {
                let mut got = vec![0u8; queued.len()];
                peer.read_exact(&mut got).await.map(|_| got)
            };
            let both = async { tokio::join!(drain, conn.next_frame()) };
            let joined = tokio::time::timeout(Duration::from_secs(5), both).await;
            assert!(joined.is_ok(), "the frame read before the cut was lost");
            if let Ok((got, frame)) = joined {
                assert_eq!(got.ok(), Some(queued));
                assert!(matches!(frame, Frame::Data(d) if d == b"hi"));
            }
        });
    }

    #[test]
    fn a_frame_kept_after_a_cut_write_is_the_next_buffered_frame() {
        block_on(async {
            let (mut conn, _peer) = duplex_conn();
            conn.ws.get_mut().output = vec![0u8; 4096];
            conn.ws.get_mut().input.extend(client_frame(0x1, b"hi"));
            let cut = tokio::time::timeout(Duration::from_millis(50), conn.next_frame()).await;
            assert!(cut.is_err(), "the write must wait for the peer");
            // The session loop asks for a buffered frame first.
            assert!(matches!(conn.buffered(), Some(Frame::Data(d)) if d == b"hi"));
            assert!(conn.buffered().is_none());
        });
    }
}
