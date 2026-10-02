// Minimal in-memory stand in for the parts of IndexedDB the outbox uses, for Node tests.
//
// Covered: open with upgradeneeded, object stores with out of line string keys, get, put,
// delete, getAll with a key range and a count, transactions with complete and abort events.
// Semantics kept from IndexedDB, because the outbox relies on them:
//   * Transactions on one database run one after another, also across connections (tabs).
//     Real IndexedDB only serialises overlapping read/write scopes; serialising all of them is
//     stricter and still valid.
//   * A transaction commits when it has no pending requests left; until then nothing is
//     visible to other transactions. An abort discards all of its writes.
//   * A request error aborts the transaction. Values are stored as structured clones.
// The real browser behaviour is checked separately in Chromium (see ../browser/).

export class MemoryKeyRange {
  constructor(lower, upper) {
    this.lower = lower;
    this.upper = upper;
  }

  static bound(lower, upper) {
    return new MemoryKeyRange(checkKey(lower), checkKey(upper));
  }

  includes(key) {
    return key >= this.lower && key <= this.upper;
  }
}

export class MemoryIndexedDB {
  #databases = new Map();
  #faults = [];

  /**
   * Makes the next request that matches `predicate({ store, method, key })` fail, which aborts
   * its transaction. Used to prove that an append is all or nothing.
   */
  failNext(predicate) {
    this.#faults.push(predicate);
  }

  open(name, version) {
    const request = new Request();
    later(() => {
      let database = this.#databases.get(name);
      const upgrade = database === undefined || database.version < version;
      if (database === undefined) {
        database = new Database(version, this.#faults);
        this.#databases.set(name, database);
      }
      database.version = Math.max(database.version, version);
      const connection = new Connection(database);
      request.result = connection;
      if (upgrade) request.onupgradeneeded?.();
      request.onsuccess?.();
    });
    return request;
  }
}

class Database {
  constructor(version, faults) {
    this.version = version;
    this.faults = faults;
    this.stores = new Map();
    this.queue = [];
    this.running = false;
  }

  enqueue(transaction) {
    this.queue.push(transaction);
    this.#runNext();
  }

  finished() {
    this.running = false;
    this.#runNext();
  }

  #runNext() {
    if (this.running || this.queue.length === 0) return;
    this.running = true;
    const transaction = this.queue.shift();
    later(() => transaction.start());
  }
}

class Connection {
  #database;

  constructor(database) {
    this.#database = database;
    this.objectStoreNames = {
      contains: (name) => database.stores.has(name),
    };
  }

  createObjectStore(name) {
    this.#database.stores.set(name, new Map());
  }

  transaction(storeNames, mode) {
    for (const name of storeNames) {
      if (!this.#database.stores.has(name)) throw new Error(`NotFoundError: ${name}`);
    }
    const transaction = new Transaction(this.#database, storeNames, mode);
    this.#database.enqueue(transaction);
    return transaction;
  }

  close() {}
}

class Transaction {
  #database;
  #mode;
  #scope;
  #work = [];
  #pending = 0;
  #started = false;
  #finished = false;
  #copies = new Map();

  constructor(database, storeNames, mode) {
    this.#database = database;
    this.#scope = new Set(storeNames);
    this.#mode = mode;
    this.error = null;
    this.oncomplete = null;
    this.onabort = null;
  }

  objectStore(name) {
    if (!this.#scope.has(name)) throw new Error(`NotFoundError: ${name} not in scope`);
    return new ObjectStore(this, name);
  }

  abort() {
    if (this.#finished) throw new Error('InvalidStateError: transaction finished');
    this.#finish(new Error('AbortError'));
  }

  // Called by the database when it is this transaction's turn.
  start() {
    if (this.#finished) {
      // Aborted while waiting in the queue: nothing to run.
      this.#database.finished();
      return;
    }
    this.#started = true;
    for (const name of this.#scope) {
      this.#copies.set(name, new Map(this.#database.stores.get(name)));
    }
    const work = this.#work;
    this.#work = [];
    work.forEach((run) => later(run));
    this.#settleWhenIdle();
  }

  request(store, method, key, operation) {
    if (this.#finished) throw new Error('TransactionInactiveError');
    if (method !== 'get' && method !== 'getAll' && this.#mode !== 'readwrite') {
      throw new Error('ReadOnlyError');
    }
    const request = new Request();
    this.#pending += 1;
    const run = () => {
      if (this.#finished) return;
      this.#pending -= 1;
      const faultIndex = this.#database.faults.findIndex((match) =>
        match({ store, method, key }),
      );
      if (faultIndex >= 0) {
        this.#database.faults.splice(faultIndex, 1);
        request.error = new Error(`injected failure: ${method} ${store}`);
        request.onerror?.();
        this.#finish(request.error);
        return;
      }
      request.result = operation(this.#copies.get(store));
      request.onsuccess?.();
      this.#settleWhenIdle();
    };
    if (this.#started) later(run);
    else this.#work.push(run);
    return request;
  }

  #settleWhenIdle() {
    later(() => {
      if (!this.#finished && this.#pending === 0) this.#finish(null);
    });
  }

  #finish(error) {
    if (this.#finished) return;
    this.#finished = true;
    if (error === null) {
      for (const [name, copy] of this.#copies) this.#database.stores.set(name, copy);
    } else {
      this.error = error;
    }
    later(() => {
      if (error === null) this.oncomplete?.();
      else this.onabort?.();
      if (this.#started) this.#database.finished();
    });
  }
}

class ObjectStore {
  #transaction;
  #name;

  constructor(transaction, name) {
    this.#transaction = transaction;
    this.#name = name;
  }

  get(key) {
    checkKey(key);
    return this.#transaction.request(this.#name, 'get', key, (rows) =>
      rows.has(key) ? structuredClone(rows.get(key)) : undefined,
    );
  }

  put(value, key) {
    checkKey(key);
    const copy = structuredClone(value);
    return this.#transaction.request(this.#name, 'put', key, (rows) => {
      rows.set(key, copy);
      return key;
    });
  }

  delete(key) {
    checkKey(key);
    return this.#transaction.request(this.#name, 'delete', key, (rows) => {
      rows.delete(key);
      return undefined;
    });
  }

  getAll(range, count) {
    return this.#transaction.request(this.#name, 'getAll', undefined, (rows) => {
      const keys = [...rows.keys()].filter((key) => range === undefined || range.includes(key));
      keys.sort((a, b) => (a < b ? -1 : a > b ? 1 : 0));
      const limited = count === undefined ? keys : keys.slice(0, count);
      return limited.map((key) => structuredClone(rows.get(key)));
    });
  }
}

class Request {
  constructor() {
    this.result = undefined;
    this.error = null;
    this.onsuccess = null;
    this.onerror = null;
    this.onupgradeneeded = null;
  }
}

function checkKey(key) {
  if (typeof key !== 'string') throw new Error('DataError: this stand in only supports string keys');
  return key;
}

function later(callback) {
  setImmediate(callback);
}
