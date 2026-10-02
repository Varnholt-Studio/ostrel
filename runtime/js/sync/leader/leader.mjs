// Tab leader election for the sync client (ARCHITECTURE 5.4, D32).
//
// All tabs of one origin share one replica and one outbox. Exactly one tab, the leader, holds
// the Web Lock `ostrel-sync`, owns the WebSocket and drains the outbox. The other tabs queue a
// request for the same lock, so the browser hands it to one of them when the leader closes or
// crashes. Tabs talk over a BroadcastChannel:
//
//   { t: 'who',    from }        a tab asks the current leader to announce itself
//   { t: 'leader', from }        the sender holds the lock
//   { t: 'resign', from }        the sender gave the lock up
//   { t: 'up',     from, body }  follower to leader; only the tab holding the lock handles it
//   { t: 'down',   from, body }  leader to followers
//
// Routing does not depend on who a tab believes the leader is: an `up` message is handled by
// whichever tab holds the lock when it arrives. `leaderId()` is informational. A message sent
// while no tab holds the lock is lost; this is safe because ops live in the durable outbox and
// a new leader drains it when it takes over.
//
// Messages from the channel are treated as untrusted and malformed ones are ignored.

export const LOCK_NAME = 'ostrel-sync';
export const CHANNEL_NAME = 'ostrel-sync';
export const DEFAULT_RETRY_MS = 1000;

const KINDS = new Set(['who', 'leader', 'resign', 'up', 'down']);

// Returns the parsed message or null when it is malformed.
function parseMessage(data) {
  if (data === null || typeof data !== 'object' || Array.isArray(data)) return null;
  const { t, from } = data;
  if (typeof t !== 'string' || !KINDS.has(t)) return null;
  if (typeof from !== 'string' || from === '') return null;
  if ((t === 'up' || t === 'down') && !Object.hasOwn(data, 'body')) return null;
  return { t, from, body: data.body };
}

function requireFunction(value, name) {
  if (typeof value !== 'function') throw new TypeError(`${name} must be a function`);
}

/**
 * Creates the leader election for one tab.
 *
 * @param {object} deps
 * @param {string} deps.tabId           unique id of this tab (for example a random UUID)
 * @param {{request: Function}} deps.locks  `navigator.locks` or a compatible object
 * @param {(name: string) => BroadcastChannel} deps.openChannel  opens the channel
 * @param {(ctx: {signal: AbortSignal}) => Promise<void>} deps.onLead
 *   runs while this tab leads (open the socket, drain the outbox). It must settle promptly
 *   once `signal` aborts. If it settles on its own, the tab steps down and asks for the lock
 *   again after `retryMs`.
 * @param {(msg: {kind: 'up'|'down', from: string, body: unknown}) => void} [deps.onMessage]
 * @param {(err: unknown) => void} [deps.onError]
 * @param {string} [deps.lockName]
 * @param {string} [deps.channelName]
 * @param {number} [deps.retryMs]
 */
export function createTabLeader({
  tabId,
  locks,
  openChannel,
  onLead,
  onMessage = () => {},
  onError = () => {},
  lockName = LOCK_NAME,
  channelName = CHANNEL_NAME,
  retryMs = DEFAULT_RETRY_MS,
}) {
  if (typeof tabId !== 'string' || tabId === '') {
    throw new TypeError('tabId must be a non empty string');
  }
  if (locks === null || typeof locks !== 'object' || typeof locks.request !== 'function') {
    throw new TypeError('locks must provide request()');
  }
  requireFunction(openChannel, 'openChannel');
  requireFunction(onLead, 'onLead');
  requireFunction(onMessage, 'onMessage');
  requireFunction(onError, 'onError');

  let started = false;
  let stopped = false;
  let leading = false;
  let currentLeader = null;
  let channel = null;
  let requestCtrl = null; // aborts a queued lock request
  let leadCtrl = null; // aborts the running onLead
  let leadDone = null; // settles when the lock callback returns
  let retryTimer = null;

  function send(msg) {
    if (!channel) return;
    try {
      channel.postMessage(msg);
    } catch (err) {
      onError(err);
    }
  }

  function deliver(msg) {
    try {
      onMessage(msg);
    } catch (err) {
      onError(err);
    }
  }

  function handle(event) {
    const msg = parseMessage(event?.data);
    if (!msg || msg.from === tabId) return;
    switch (msg.t) {
      case 'who':
        if (leading) send({ t: 'leader', from: tabId });
        break;
      case 'leader':
        if (!leading) currentLeader = msg.from;
        break;
      case 'resign':
        if (currentLeader === msg.from) currentLeader = null;
        break;
      case 'up':
        if (leading) deliver({ kind: 'up', from: msg.from, body: msg.body });
        break;
      case 'down':
        if (!leading) {
          currentLeader = msg.from;
          deliver({ kind: 'down', from: msg.from, body: msg.body });
        }
        break;
    }
  }

  async function lead() {
    leading = true;
    currentLeader = tabId;
    leadCtrl = new AbortController();
    send({ t: 'leader', from: tabId });
    try {
      await onLead({ signal: leadCtrl.signal });
    } catch (err) {
      onError(err);
    }
    leading = false;
    leadCtrl = null;
    if (currentLeader === tabId) currentLeader = null;
    send({ t: 'resign', from: tabId });
  }

  function requestLock() {
    retryTimer = null;
    if (stopped) return;
    requestCtrl = new AbortController();
    let finish;
    leadDone = new Promise((resolve) => { finish = resolve; });
    const done = finish;
    let granted = false;
    let request;
    try {
      request = locks.request(lockName, { signal: requestCtrl.signal }, async () => {
        granted = true;
        requestCtrl = null;
        try {
          if (!stopped) await lead();
        } finally {
          done();
          if (!stopped) retryTimer = setTimeout(requestLock, retryMs);
        }
      });
    } catch (err) {
      done();
      onError(err);
      return;
    }
    Promise.resolve(request).catch((err) => {
      if (!granted) done();
      if (err?.name !== 'AbortError') onError(err);
    });
  }

  return {
    /** Joins the election. Throws once the tab has been stopped. */
    start() {
      if (stopped) throw new Error('tab leader is stopped');
      if (started) return;
      started = true;
      channel = openChannel(channelName);
      channel.onmessage = handle;
      send({ t: 'who', from: tabId });
      requestLock();
    },

    /** Leaves the election; resolves once leadership (if any) has been handed back. */
    async stop() {
      if (stopped) return;
      stopped = true;
      if (retryTimer !== null) {
        clearTimeout(retryTimer);
        retryTimer = null;
      }
      requestCtrl?.abort();
      leadCtrl?.abort();
      if (leadDone) await leadDone;
      channel?.close();
      channel = null;
      currentLeader = null;
    },

    /** Whether this tab holds the lock now. */
    isLeader() {
      return leading;
    },

    /** Id of the tab last known to lead, or null. Informational only. */
    leaderId() {
      return currentLeader;
    },

    channelName() {
      return channelName;
    },

    /**
     * Sends `body` to the leader. On the leader it is delivered locally (asynchronously).
     * Returns false when the tab is not running.
     */
    post(body) {
      if (!started || stopped) return false;
      if (leading) {
        queueMicrotask(() => {
          if (leading) deliver({ kind: 'up', from: tabId, body });
        });
      } else {
        send({ t: 'up', from: tabId, body });
      }
      return true;
    },

    /** Sends `body` to every follower. Returns false unless this tab leads. */
    broadcast(body) {
      if (!leading || stopped) return false;
      send({ t: 'down', from: tabId, body });
      return true;
    },
  };
}
