// A small queue of events for one sync session: frames and the close of the socket, a wake up
// after a local write, the end of a `Defer` delay and the abort of the session all arrive
// here, so the session handles them one at a time in the order they happened.

export class EventQueue {
  #items = [];
  #waiting = null;

  /** Adds an event. Never blocks. */
  push(event) {
    if (this.#waiting !== null) {
      const resolve = this.#waiting;
      this.#waiting = null;
      resolve(event);
      return;
    }
    this.#items.push(event);
  }

  /** Resolves to the oldest event, waiting for one if the queue is empty. */
  next() {
    if (this.#items.length > 0) {
      return Promise.resolve(this.#items.shift());
    }
    return new Promise((resolve) => {
      this.#waiting = resolve;
    });
  }
}

/**
 * Connects the events of a WebSocket like object (`onopen`, `onmessage`, `onclose`,
 * `onerror`) to `queue`. A close and an error both end up as one `{ type: 'closed' }` event.
 */
export function watchSocket(socket, queue) {
  let closed = false;
  const close = () => {
    if (closed) return;
    closed = true;
    queue.push({ type: 'closed' });
  };
  socket.onopen = () => queue.push({ type: 'open' });
  socket.onmessage = (message) => queue.push({ type: 'frame', data: message.data });
  socket.onclose = close;
  socket.onerror = close;
}
