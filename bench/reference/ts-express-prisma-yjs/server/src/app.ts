import { createHash, randomBytes, verify, type KeyObject } from 'node:crypto';
import { createServer, type IncomingMessage } from 'node:http';
import express, { type NextFunction, type Request, type Response } from 'express';
import WebSocket from 'ws';
import * as Y from 'yjs';
import * as encoding from 'lib0/encoding';
import * as decoding from 'lib0/decoding';
import * as sync from 'y-protocols/sync';
import { mintToken, publicKeyFromRaw, readToken } from './paseto.ts';
import { Bucket, LIMITS, Window } from './limits.ts';
import { checkUpdate, writersOf } from './messages.ts';
import { DuplicateHandle, type Store, type User } from './store.ts';

const MESSAGE_SYNC = 0;
const HANDLE = /^[a-z0-9_]{3,20}$/;

// WebSocket close codes sent to the client.
export const CLOSE = { unauthorized: 4401, forbidden: 4403, invalid: 4400, limit: 4429 };

type Room = { id: string; doc: Y.Doc; conns: Map<WebSocket, User>; queue: Promise<void> };
type Authed = Request & { user: User };

export function createApp(store: Store, serverKey: KeyObject, limits: typeof LIMITS = LIMITS) {
  const app = express();
  const challenges = new Map<string, number>();
  const challengeRate = new Window(limits.challengesPerMinute, 60_000);
  const registrationRate = new Window(limits.registrationsPerHour, 3_600_000);
  const opRate = new Map<string, Bucket>();
  const rowRate = new Window(limits.rowsPerHour, 3_600_000);
  const rooms = new Map<string, Promise<Room>>();

  app.use(express.json({ limit: limits.bodyBytes }));

  // Sign in: the client signs a fresh, single use challenge with its Ed25519 user key.
  app.post('/auth/challenge', (req, res) => {
    if (!challengeRate.allow(req.ip ?? '')) return void res.sendStatus(429);
    const challenge = randomBytes(32).toString('base64url');
    challenges.set(challenge, Date.now() + limits.challengeTtlMs);
    res.json({ challenge });
  });

  function provenKey(body: Record<string, unknown>): string | null {
    const { publicKey, challenge, signature } = body;
    if (typeof publicKey !== 'string' || typeof challenge !== 'string' || typeof signature !== 'string') return null;
    const expires = challenges.get(challenge);
    challenges.delete(challenge);
    if (!expires || expires < Date.now()) return null;
    try {
      const key = publicKeyFromRaw(Buffer.from(publicKey, 'base64url'));
      return verify(null, Buffer.from(challenge), key, Buffer.from(signature, 'base64url')) ? publicKey : null;
    } catch {
      return null;
    }
  }

  const session = (user: User) => ({
    token: mintToken(serverKey, user.id, createHash('sha256').update(user.publicKey).digest('base64url').slice(0, 16)),
    user: { id: user.id, handle: user.handle, name: user.name },
  });

  app.post('/auth/register', async (req, res) => {
    const handle = String(req.body?.handle ?? '').toLowerCase();
    const name = String(req.body?.name ?? handle).slice(0, 64);
    if (!HANDLE.test(handle)) return void res.status(400).json({ error: 'invalid handle' });
    const publicKey = provenKey(req.body ?? {});
    if (!publicKey) return void res.sendStatus(401);
    if (await store.userByKey(publicKey)) return void res.status(409).json({ error: 'key already registered' });
    if (!registrationRate.allow(req.ip ?? '')) return void res.sendStatus(429);
    try {
      res.status(201).json(session(await store.createUser({ handle, name, publicKey })));
    } catch (e) {
      if (e instanceof DuplicateHandle) return void res.status(409).json({ error: 'handle taken' });
      throw e;
    }
  });

  app.post('/auth/signin', async (req, res) => {
    const publicKey = provenKey(req.body ?? {});
    const user = publicKey && (await store.userByKey(publicKey));
    if (!user) return void res.sendStatus(401);
    res.json(session(user));
  });

  async function userFromToken(token: string | undefined) {
    const claims = token ? readToken(serverKey, token) : null;
    return claims && store.userById(claims.sub);
  }

  const api = express.Router();
  api.use(async (req: Request, res: Response, next: NextFunction) => {
    const user = await userFromToken(req.headers.authorization?.replace(/^Bearer /, ''));
    if (!user) return void res.sendStatus(401);
    (req as Authed).user = user;
    next();
  });

  api.get('/me', (req, res) => void res.json(session((req as Authed).user).user));

  api.patch('/me', async (req, res) => {
    const name = req.body?.name;
    if (typeof name !== 'string' || !name.trim() || name.length > 64) return void res.sendStatus(400);
    const user = await store.renameUser((req as Authed).user.id, name);
    res.json(session(user).user);
  });

  api.get('/rooms', async (_req, res) => void res.json(await store.rooms()));

  api.post('/rooms', async (req, res) => {
    const name = req.body?.name;
    if (typeof name !== 'string' || !name.trim() || name.length > 64) return void res.sendStatus(400);
    res.status(201).json(await store.createRoom(name.trim(), (req as Authed).user.id));
  });

  // Membership changes only ever concern the signed in user.
  api.put('/rooms/:id/members/me', async (req, res) => {
    if (!(await store.room(req.params.id))) return void res.sendStatus(404);
    await store.join(req.params.id, (req as Authed).user.id);
    res.json(await store.room(req.params.id));
  });

  api.delete('/rooms/:id/members/me', async (req, res) => {
    const user = (req as Authed).user;
    await store.leave(req.params.id, user.id);
    const room = await rooms.get(req.params.id);
    for (const [ws, u] of room?.conns ?? []) if (u.id === user.id) ws.close(CLOSE.forbidden, 'evicted');
    res.sendStatus(204);
  });

  app.use('/api', api);

  // Realtime: one Yjs document per room, spoken with the y-websocket protocol.
  function openRoom(id: string): Promise<Room> {
    let room = rooms.get(id);
    if (!room) {
      room = store.updates(id).then((updates) => {
        const doc = new Y.Doc({ gc: false });
        if (updates.length) Y.applyUpdate(doc, Y.mergeUpdates(updates));
        return { id, doc, conns: new Map(), queue: Promise.resolve() };
      });
      rooms.set(id, room);
    }
    return room;
  }

  function send(ws: WebSocket, write: (e: encoding.Encoder) => void) {
    const e = encoding.createEncoder();
    encoding.writeVarUint(e, MESSAGE_SYNC);
    write(e);
    if (ws.readyState === WebSocket.OPEN) ws.send(encoding.toUint8Array(e));
  }

  async function receive(room: Room, ws: WebSocket, user: User, update: Uint8Array) {
    if (ws.readyState !== WebSocket.OPEN) return; // a rejected connection sends nothing more
    if (!(await store.isMember(room.id, user.id))) return ws.close(CLOSE.forbidden, 'not a member');
    const owners = await store.clientOwners(writersOf(room.doc, update));
    const verdict = checkUpdate(room.doc, update, user.id, owners);
    if (!verdict.ok) return ws.close(CLOSE.invalid, verdict.reason);
    if (verdict.added.length === 0) return;
    const n = verdict.added.length;
    const bucket = opRate.get(user.id) ?? new Bucket(limits.opsPerSecond, limits.opsBurst);
    opRate.set(user.id, bucket);
    if (n > bucket.available() || n > rowRate.remaining(user.id)) return ws.close(CLOSE.limit, 'rate limit');
    bucket.take(n);
    rowRate.allow(user.id, n);
    await store.appendUpdate(room.id, update, user.id, verdict.clients);
    Y.applyUpdate(room.doc, update);
    for (const other of room.conns.keys()) if (other !== ws) send(other, (e) => sync.writeUpdate(e, update));
  }

  const wss = new WebSocket.Server({ noServer: true, maxPayload: limits.frameBytes });
  const server = createServer(app);

  server.on('upgrade', (req: IncomingMessage, socket, head) => {
    const url = new URL(req.url ?? '/', 'http://localhost');
    const match = /^\/sync\/([^/]+)$/.exec(url.pathname);
    if (!match) return void socket.destroy();
    wss.handleUpgrade(req, socket, head, (ws) => void connect(ws, decodeURIComponent(match[1]), url.searchParams.get('token') ?? undefined));
  });

  async function connect(ws: WebSocket, id: string, token: string | undefined) {
    ws.binaryType = 'arraybuffer';
    let handle: ((data: Uint8Array) => void) | null = null;
    const early: Uint8Array[] = [];
    ws.on('message', (data: ArrayBuffer) => {
      if (!handle) return void early.push(new Uint8Array(data));
      try {
        handle(new Uint8Array(data));
      } catch {
        ws.close(CLOSE.invalid, 'malformed message');
      }
    });
    const user = await userFromToken(token);
    if (!user) return ws.close(CLOSE.unauthorized, 'unauthorized');
    if (!(await store.isMember(id, user.id))) return ws.close(CLOSE.forbidden, 'not a member');
    const room = await openRoom(id);
    room.conns.set(ws, user);
    ws.on('close', () => room.conns.delete(ws));

    handle = (data: Uint8Array) => {
      const d = decoding.createDecoder(data);
      if (decoding.readVarUint(d) !== MESSAGE_SYNC) return; // awareness is not used by the chat
      const type = decoding.readVarUint(d);
      if (type === sync.messageYjsSyncStep1) {
        const sv = decoding.readVarUint8Array(d);
        send(ws, (e) => sync.writeSyncStep2(e, room.doc, sv));
      } else if (type === sync.messageYjsSyncStep2 || type === sync.messageYjsUpdate) {
        const update = decoding.readVarUint8Array(d);
        room.queue = room.queue.then(() => receive(room, ws, user, update)).catch(() => ws.close(CLOSE.invalid, 'rejected'));
      }
    };
    send(ws, (e) => sync.writeSyncStep1(e, room.doc));
    for (const data of early.splice(0)) ws.emit('message', data.buffer);
  }

  return server;
}
