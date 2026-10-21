// The sync client of one tab: tab leader, outbox and socket wired together
// (ARCHITECTURE 5.4, D32).
//
// Every tab creates one client. The tabs of an origin elect one leader
// (`runtime/js/sync/leader/`); only the leader opens the WebSocket and drains the shared
// outbox (`runtime/js/sync/outbox/`). Any tab may write: it appends to the outbox and calls
// `changed()`, which wakes the leader's session, locally or over the BroadcastChannel. The
// leader reports every outcome to the other tabs, so each tab can roll back a rejected op in
// its own view.
//
// Sessions and reconnects. While a tab leads, it runs one session after another:
//
// * a closed socket or a faulty frame: reconnect after a delay that doubles from
//   `reconnectMs` up to `maxReconnectMs`, and starts again at `reconnectMs` after a `Welcome`;
// * a fatal `error` the server sends to every session that has to go away (`Protocol`,
//   `FrameTooLarge`, `TooManySessions`, `Overloaded`): reconnect the same way. After
//   `Overloaded` the client catches up with back pressure (5.4);
// * `Superseded`: a newer session of the same replica exists, so this tab stops leading and
//   opens no session until it leads again (D32, Q27 (b));
// * `Unauthorized`, `ReplicaExpired`, and an outbox whose oldest op cannot be sent: another
//   attempt would fail the same way. The tab keeps leading but stays offline until the app
//   calls `reconnect()` (for example after a new sign in or after dropping the replica as the
//   notice of 5.5 says). `onNotice` tells the app.

import { createTabLeader } from '../leader/leader.mjs';
import { EventQueue } from './events.mjs';
import { END, runSession } from './session.mjs';

export const DEFAULT_RECONNECT_MS = 1000;
export const DEFAULT_MAX_RECONNECT_MS = 30000;

// Fatal codes after which a new session can succeed.
const RETRY_CODES = new Set(['Protocol', 'FrameTooLarge', 'TooManySessions', 'Overloaded']);

// Messages between the tabs, in the `body` of the leader's `up` and `down` messages.
const CHANGED = 'changed';
const OUTCOME = 'outcome';

/**
 * Creates the sync client of one tab.
 *
 * @param {object} deps
 * @param {string} deps.tabId
 * @param {{request: Function}} deps.locks  `navigator.locks`
 * @param {(name: string) => BroadcastChannel} deps.openChannel
 * @param {() => WebSocket} deps.connect  opens a new WebSocket to the app's sync endpoint
 * @param {import('../outbox/outbox.mjs').Outbox} deps.outbox
 * @param {{load: Function, saveReplica: Function}} deps.identity  see `runSession`
 * @param {Iterable<string>} [deps.models]  model names of the compiled schema
 * @param {object} [deps.hooks]
 *   `onWelcome(info)` on the leader; `onAck(outcome)` and `onReject(outcome)` on every tab;
 *   `onNotice(notice)` on the leader for what the user must see; `onError(error)` for
 *   failures of the client itself
 * @param {number} [deps.reconnectMs]
 * @param {number} [deps.maxReconnectMs]
 * @param {object} [deps.leaderOptions]  passed to `createTabLeader` (lock and channel names,
 *   `retryMs`)
 */
export function createSyncClient({
  tabId,
  locks,
  openChannel,
  connect,
  outbox,
  identity,
  models = [],
  hooks = {},
  reconnectMs = DEFAULT_RECONNECT_MS,
  maxReconnectMs = DEFAULT_MAX_RECONNECT_MS,
  leaderOptions = {},
}) {
  if (typeof connect !== 'function') throw new TypeError('connect must be a function');
  const modelNames = [...models];
  // The event queue of the running session; null while this tab does not lead.
  let current = null;
  // Wakes a leader that waits offline or for its reconnect delay.
  let retryNow = null;

  const report = (error) => {
    try {
      hooks.onError?.(error);
    } catch {
      // A failing error sink must not stop the client.
    }
  };

  // Calls a hook and reports a failure; a hook can never stop the session.
  const hook = (name) => async (payload) => {
    try {
      await hooks[name]?.(payload);
    } catch (error) {
      report(error);
    }
  };

  const sessionHooks = {
    onWelcome: hook('onWelcome'),
    onNotice: hook('onNotice'),
    onAck: async (outcome) => {
      leader.broadcast({ kind: OUTCOME, outcome: { ...outcome, t: 'ack' } });
      await hook('onAck')(outcome);
    },
    onReject: async (outcome) => {
      leader.broadcast({ kind: OUTCOME, outcome: { ...outcome, t: 'reject' } });
      await hook('onReject')(outcome);
    },
  };

  // Waits `ms`, or less if `signal` aborts or `reconnect()` is called; `ms` null waits for
  // one of those two only.
  function pause(ms, signal) {
    return new Promise((resolve) => {
      const done = () => {
        clearTimeout(timer);
        signal.removeEventListener('abort', done);
        retryNow = null;
        resolve();
      };
      const timer = ms === null ? undefined : setTimeout(done, ms);
      retryNow = done;
      signal.addEventListener('abort', done, { once: true });
    });
  }

  async function onLead({ signal }) {
    let delay = reconnectMs;
    while (!signal.aborted) {
      current = new EventQueue();
      let result;
      try {
        result = await runSession({
          socket: connect(),
          outbox,
          identity,
          models: modelNames,
          signal,
          queue: current,
          hooks: sessionHooks,
        });
      } finally {
        current = null;
      }
      if (result.welcomed) delay = reconnectMs;
      if (result.end === END.ABORTED) return;
      if (result.end === END.FATAL && result.code === 'Superseded') {
        await hook('onNotice')({ kind: 'superseded' });
        return;
      }
      if (result.end === END.FATAL && !RETRY_CODES.has(result.code)) {
        await hook('onNotice')({ kind: 'session-refused', code: result.code });
        await pause(null, signal);
        continue;
      }
      if (result.end === END.STUCK) {
        await hook('onNotice')({ kind: 'outbox-stuck', reason: result.error.message });
        await pause(null, signal);
        continue;
      }
      if (result.end === END.FAULT) report(result.error);
      await pause(delay, signal);
      delay = Math.min(delay * 2, maxReconnectMs);
    }
  }

  function onMessage({ kind, body }) {
    if (body === null || typeof body !== 'object') return;
    if (kind === 'up' && body.kind === CHANGED) {
      current?.push({ type: 'wake' });
      return;
    }
    if (kind === 'down' && body.kind === OUTCOME) {
      const outcome = body.outcome;
      if (outcome === null || typeof outcome !== 'object') return;
      if (outcome.t === 'ack') void hook('onAck')(withoutKind(outcome));
      if (outcome.t === 'reject') void hook('onReject')(withoutKind(outcome));
    }
  }

  const leader = createTabLeader({
    ...leaderOptions,
    tabId,
    locks,
    openChannel,
    onLead,
    onMessage,
    onError: report,
  });

  return {
    /** Joins the leader election; the leader connects. */
    start() {
      leader.start();
    },
    /** Leaves the election; a leading tab closes its session first. */
    stop() {
      return leader.stop();
    },
    /** Call after appending to the outbox, so the leader sends the new ops. */
    changed() {
      if (current !== null) current.push({ type: 'wake' });
      else leader.post({ kind: CHANGED });
    },
    /** Ends a reconnect delay or an offline wait of the leader at once. */
    reconnect() {
      retryNow?.();
    },
    isLeader() {
      return leader.isLeader();
    },
  };
}

function withoutKind(outcome) {
  const { t: _t, ...rest } = outcome;
  return rest;
}
