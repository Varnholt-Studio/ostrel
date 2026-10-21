// Server function calls of one sync session (ARCHITECTURE 5.4, 5.7, SPEC 12.7, D155).
//
// A `CallTable` belongs to one session, from its `Welcome` to its end. It
//
// * gives every call an id of its own: ids rise per session and are never used twice, also
//   not after the call settled (D155), so a late `return` can never be read against another
//   function;
// * writes the `call` frame with the arguments typed by the function's parameters (D132) and
//   hands it to `send`;
// * settles each call exactly once:
//   - with the `return` of the server: `{ ok: true, value }`, or `{ ok: false, kind }` with a
//     call error (`Denied`, `NotFound`, `Limit`, `Invalid`, `Unavailable`) or the kind of a
//     runtime error of the server;
//   - `Offline` when the frame never reached the socket: nothing was sent (SPEC 12.7);
//   - `OutcomeUnknown` when the frame was sent and no `return` came, because the session
//     ended or `call_timeout_ms` passed with the session open. The server may or may not have
//     run the call, so the client never sends it again (D81); a `return` that comes later is
//     dropped (D155).
//
// A call is never settled with a default value (AC-33 d); turning the outcome into an
// `OstrelError` is the generated code's part (`Settle`).
//
// `offlineCall()` is the outcome of a call made while no session is open.

import { DEFAULT_LIMITS, WireError, encodeClient } from './frame.mjs';

/** Default of `call_timeout_ms` (`ostrel_sync::protocol::LIMITS`), until `Welcome` names one. */
export const DEFAULT_CALL_TIMEOUT_MS = 30000;

/** The outcome of a call made while no session is open: nothing was sent (SPEC 12.7). */
export function offlineCall() {
  return Promise.resolve(failed('Offline'));
}

export class CallTable {
  /**
   * @param {object} deps
   * @param {import('./value.mjs').WireSchema} deps.schema
   * @param {(frame: string) => void} deps.send  hands a frame to the socket; throws when it
   *   cannot (a closed socket)
   * @param {object} [deps.limits]  the limits of `Welcome` (`call_timeout_ms`, `frame_bytes`)
   * @param {Function} [deps.setTimer]
   * @param {Function} [deps.clearTimer]
   */
  constructor({
    schema,
    send,
    limits = {},
    setTimer = setTimeout,
    clearTimer = clearTimeout,
  }) {
    this.schema = schema;
    this.send = send;
    this.timeoutMs = limits.call_timeout_ms ?? DEFAULT_CALL_TIMEOUT_MS;
    this.frameBytes = limits.frame_bytes ?? DEFAULT_LIMITS.frame_bytes;
    this.setTimer = setTimer;
    this.clearTimer = clearTimer;
    this.nextId = 0;
    // Calls sent and not settled: id -> { name, settle, timer }.
    this.open = new Map();
    this.closed = false;
  }

  /**
   * Calls the server function `name`.
   *
   * @param {string} name  the function's name in the schema
   * @param {readonly unknown[]} args  values in memory, one per parameter
   * @returns {Promise<{ok: true, value: unknown} | {ok: false, kind: string}>}
   * @throws {WireError} when the schema has no such function or an argument does not fit its
   *   parameter: a defect of the caller, nothing is sent and no id is used
   */
  call(name, args) {
    if (this.closed) return offlineCall();
    const id = this.nextId;
    let frame;
    try {
      frame = encodeClient(
        { t: 'call', call: id, function: name, args },
        { schema: this.schema, frameBytes: this.frameBytes },
      );
    } catch (error) {
      // The arguments are over the frame limit: the server could never read them, and nothing
      // was sent, so the call is refused here as the server refuses a value over a limit.
      if (error instanceof WireError && error.kind === 'FrameTooLarge') {
        this.nextId += 1;
        return Promise.resolve(failed('Limit'));
      }
      throw error;
    }
    this.nextId += 1;
    try {
      this.send(frame);
    } catch {
      return Promise.resolve(failed('Offline'));
    }
    return new Promise((resolve) => {
      const timer = this.setTimer(() => this.settle(id, failed('OutcomeUnknown')), this.timeoutMs);
      this.open.set(id, { name, resolve, timer });
    });
  }

  /** The function of a call that waits for its `return`, or null (for `decodeServer`). */
  functionOf(id) {
    return this.open.get(id)?.name ?? null;
  }

  /**
   * Settles a call with a decoded `return`. A `return` for a call that does not wait is
   * dropped; `decodeServer` already refuses it as `UnknownCall`.
   */
  receive({ call, result }) {
    this.settle(call, result);
  }

  /** The session ended: every call still waiting gets `OutcomeUnknown`; new calls `Offline`. */
  close() {
    this.closed = true;
    for (const id of [...this.open.keys()]) this.settle(id, failed('OutcomeUnknown'));
  }

  settle(id, outcome) {
    const entry = this.open.get(id);
    if (entry === undefined) return;
    this.open.delete(id);
    this.clearTimer(entry.timer);
    entry.resolve(outcome);
  }
}

function failed(kind) {
  return { ok: false, kind };
}
