// Public JavaScript API of the Ostrel programming language runtime (draft, freeze wave F2).
//
// This file is the contract between the parts of `runtime/js/`, the JS backend
// (`crates/ostrel_codegen_js`), the Node sidecar host (`runtime/node/`) and hand written
// modules behind `extern js` (ARCHITECTURE 4.2, 7.1, 7.2, 12, 13). It declares shapes only;
// the implementation lives in the modules named per section.
//
// Status: draft stub. Until the architect freezes F2, names may change by review; after the
// freeze only by an architect DECISION with an impact list.
//
// Sources: ARCHITECTURE 5.1 (type table), 5.3 (ids and wire widths), 5.4 (reject reasons),
// 5.10 (app shell), 6.2 (text order), 7.1 and 7.2 (bridge errors), 7.4 (integer range),
// 7.5 (rendering safety), 3.4 and 5.9 (runtime error kinds and limits).

// ---------------------------------------------------------------------------------------
// 1. Identifiers (ARCHITECTURE 5.3)
//
// Ids travel as lowercase hex strings of fixed width, never as JSON numbers. String order of
// two encodings equals the order of the values. The brands keep the kinds apart at compile
// time; at run time they are plain strings.

declare const brand: unique symbol;
type Hex<Kind extends string> = string & { readonly [brand]: Kind };

/** Replica id, 16 hex digits. One per (user, device key), shared by all tabs (D32). */
export type ReplicaId = Hex<"ReplicaId">;
/** Operation id, 24 hex digits: replica (16) then seq (8). */
export type OpId = Hex<"OpId">;
/** Row id, 32 hex digits: wall_ms (12), counter (4), replica (16). Created by the runtime only. */
export type RowId = Hex<"RowId">;
/** Hybrid logical clock stamp, 32 hex digits: wall_ms (12), counter (4), replica (16). */
export type Hlc = Hex<"Hlc">;
/** Position in the server op log, 16 hex digits. */
export type ServerSeq = Hex<"ServerSeq">;

/** Fixed hex widths of every id kind (ARCHITECTURE 5.3). */
export declare const HEX_WIDTH: {
  readonly ReplicaId: 16;
  readonly ServerSeq: 16;
  readonly OpId: 24;
  readonly RowId: 32;
  readonly Hlc: 32;
};

/**
 * Strict id parser: returns the id when `text` has exactly the width of `kind` and only
 * lowercase hex digits, otherwise throws `OstrelError` with kind `Invalid`.
 */
export declare function parseId<K extends keyof typeof HEX_WIDTH>(kind: K, text: string): Hex<K>;

// ---------------------------------------------------------------------------------------
// 2. Values (ARCHITECTURE 5.1, 7.4)
//
// The in memory form of every Ostrel value in JS. It equals the wire form of the type table
// except `Bytes`, which is a `Uint8Array` in memory and a base64url string on the wire.
// ASSUMPTION A1: `Set[T]` is a readonly array in canonical order and `Map[K, V]` a readonly
// array of `[key, value]` pairs in canonical key order, so equality is order independent and
// matches the canonical encoding. `docs/types.md` (architect) is authoritative once it exists.

/** `Int`: a safe integer, plus or minus (2^53 minus 1). Leaving the range is `IntOverflow`. */
export type Int = number;
/** `Float`: a finite number. NaN and infinities are rejected at every boundary. */
export type Float = number;
/** `Text`: a string of Unicode scalar values (an unpaired surrogate is `Invalid`). */
export type Text = string;
/** `Time`: milliseconds since the Unix epoch, a safe integer. */
export type Time = number;
/** `Rank`: a fractional index key; compare with `compareText`. */
export type Rank = string & { readonly [brand]: "Rank" };
/** Enum value: the variant name. Declaration order defines `<`. */
export type EnumValue<Variant extends string = string> = Variant;
/** Reference to a row: its id. The target model is known to the generated code. */
export type Ref = RowId;

export type Value =
  | null
  | boolean
  | number
  | string
  | Uint8Array
  | readonly Value[]
  | readonly (readonly [Value, Value])[]
  | Rec;

/** A record: a `data` row passed by value (G2, A2-6). */
export interface Rec {
  readonly [field: string]: Value;
}

/** Largest and smallest `Int` (ARCHITECTURE 7.4). */
export declare const INT_MAX: 9007199254740991;
export declare const INT_MIN: -9007199254740991;

/**
 * Compares two strings by Unicode code point, never by UTF 16 unit or locale
 * (ARCHITECTURE 6.2). Used for `Text` and `Rank` order on every replica.
 */
export declare function compareText(a: string, b: string): -1 | 0 | 1;

// ---------------------------------------------------------------------------------------
// 3. Errors
//
// Every error the runtime raises is an `OstrelError` with a stable `kind`. Messages are for
// people; tests compare kinds.

/** Runtime error kinds shared with the VM (SPEC AC-52, ARCHITECTURE 3.4, D44). */
export type RuntimeErrorKind =
  | "IntOverflow"
  | "DivisionByZero"
  | "CallDepth"
  | "StepLimit"
  | "HeapLimit"
  | "TextLimit";

/** Reason codes of a server `Reject` (ARCHITECTURE 5.4). `ClockSkew` does not exist (AC-57 g). */
export type RejectReason = "Denied" | "Conflict" | "Forged" | "Invalid" | "Limit";

/** Bridge errors: client side (7.1) and Node sidecar (7.2). */
export type ExternErrorKind =
  | "Type"
  | "Threw"
  | "Undefined"
  | "Busy"
  | "Crashed"
  | "Timeout";

/** Error of a server function call, as seen by the caller (5.7, 5.9). */
export type CallErrorKind = "Denied" | "NotFound" | "Limit" | "Invalid" | "Offline";

export type ErrorKind = RuntimeErrorKind | RejectReason | ExternErrorKind | CallErrorKind;

export declare class OstrelError<K extends ErrorKind = ErrorKind> extends Error {
  constructor(kind: K, message: string);
  readonly name: "OstrelError";
  readonly kind: K;
}

/** Raised by the bridge guards (AC-21). `kind` is one of `ExternErrorKind`. */
export declare class ExternError extends OstrelError<ExternErrorKind> {
  /** The `extern js` function that failed, as `<module spec>#<name>`. */
  readonly target: string;
}

// ---------------------------------------------------------------------------------------
// 4. Rows and the client store (ARCHITECTURE 5.1, 5.4, 5.6)
//
// The store is the client replica as generated code sees it. Its implementation spans
// `runtime/js/` (T3) and `runtime/js/sync/`, `runtime/js/crdt/` (T5); this interface is the
// seam between them. Writes are optimistic: applied, rendered, then queued in the outbox.

/** Implicit fields of every row (5.1). `author` exists only in apps with `auth`. */
export interface RowMeta {
  readonly id: RowId;
  readonly made: Hlc;
  readonly changed: Hlc;
  readonly author?: RowId;
  /** Client only: written locally, not yet acknowledged by the server. */
  readonly pending: boolean;
  /** Client only: the server rejected the write that produced this state. */
  readonly rejected: RejectReason | null;
}

export type Row<Fields extends Rec = Rec> = RowMeta & Readonly<Fields>;

/**
 * Query description produced by the JS backend from `where`, `sort` and `limit`. The filter
 * is query IR as JSON (ARCHITECTURE 6); the store treats it as opaque data.
 * ASSUMPTION A2: the query IR JSON form is fixed with `ostrel_db::api` in F1b and shared here.
 */
export interface Query {
  readonly model: string;
  readonly filter?: unknown;
  readonly sort?: readonly { readonly field: string; readonly desc?: boolean }[];
  readonly limit?: number;
}

export type Unsubscribe = () => void;

/** A live query: rows update incrementally when the replica changes (5.8). */
export interface LiveQuery<R extends Row = Row> {
  /** Current rows in query order; a new array after every change. */
  readonly rows: readonly R[];
  /** True until the first `Snapshot` (or the offline copy) has been applied. */
  readonly loading: boolean;
  /** Calls `listener` after each change, at most once per animation frame. */
  subscribe(listener: (rows: readonly R[]) => void): Unsubscribe;
  close(): void;
}

/**
 * One field change. Set and map fields change by exactly one element or key per op (G12).
 */
export type FieldChange =
  | { readonly op: "set"; readonly field: string; readonly value: Value }
  | { readonly op: "add"; readonly field: string; readonly element: Value }
  | { readonly op: "remove"; readonly field: string; readonly element: Value }
  | { readonly op: "put"; readonly field: string; readonly key: Value; readonly value: Value }
  | { readonly op: "delete"; readonly field: string; readonly key: Value };

export interface Store {
  query<R extends Row = Row>(query: Query): LiveQuery<R>;
  /** Reads one row from the local replica; `null` if absent, evicted or not readable. */
  get<R extends Row = Row>(model: string, id: RowId): R | null;
  /** Creates a row; the runtime assigns the id (D20). Resolves when queued locally. */
  make(model: string, fields: Rec): Promise<RowId>;
  edit(model: string, id: RowId, changes: readonly FieldChange[]): Promise<void>;
  drop(model: string, id: RowId): Promise<void>;
  /** Connection state of the tab leader's WebSocket (D32). */
  readonly online: boolean;
  onStatus(listener: (status: StoreStatus) => void): Unsubscribe;
}

export interface StoreStatus {
  readonly online: boolean;
  /** Ops in the outbox that the server has not acknowledged. */
  readonly pendingOps: number;
  /** Set when an outbox of another user or an expired replica was discarded (AC-43). */
  readonly notice: "OutboxDiscarded" | "ReplicaExpired" | null;
}

// ---------------------------------------------------------------------------------------
// 5. Server functions (ARCHITECTURE 4.3, 5.7, G11)
//
// A client call to a `server fn` has type `Remote T`. In JS it is a promise that resolves with
// the result or rejects with `OstrelError` whose kind is a `CallErrorKind` or a runtime error
// kind raised on the server. A rejection is never turned into a default value (AC-33 d).

export type Remote<T extends Value> = Promise<T>;

/** Generated per `server fn`: `call("name", args)` is the only transport. */
export interface Calls {
  call<T extends Value>(name: string, args: readonly Value[]): Remote<T>;
}

// ---------------------------------------------------------------------------------------
// 6. Bridge (ARCHITECTURE 7.1, 7.2; SPEC AC-20, AC-21, AC-38)
//
// Bridge types: Int, Float, Text, Bool, T?, List[T] and records. Each `extern js` function
// gets a generated guard that checks arguments and results against the declared Ostrel type.
// Wrong type, `undefined`, NaN, infinities, a cyclic object or a thrown exception become an
// `ExternError`; nothing is coerced.

export type BridgeValue =
  | null
  | boolean
  | number
  | string
  | readonly BridgeValue[]
  | { readonly [field: string]: BridgeValue };

/** Shape a module behind `extern js client` or `extern js server` exports. */
export type ExternModule = Readonly<
  Record<string, (...args: BridgeValue[]) => BridgeValue | Promise<BridgeValue>>
>;

/**
 * An exported Ostrel function as JS sees it (AC-20, JS calls the language). Arguments are
 * checked by the same guards; a wrong argument throws `ExternError` with kind `Type`.
 */
export type ExportedFn<Args extends readonly BridgeValue[], R extends BridgeValue> = (
  ...args: Args
) => R;

// ---------------------------------------------------------------------------------------
// 7. View core (`runtime/js/view/core.js`, T3-1; ARCHITECTURE 7.5)

export type ViewErrorCode =
  | "TagName"
  | "TagBlocked"
  | "TextType"
  | "AttrType"
  | "EventName"
  | "EventHandler"
  | "ChildType"
  | "KeyType"
  | "MixedKeys"
  | "DuplicateKey"
  | "VnodeReused"
  | "Document"
  | "Sinks";

export declare class ViewError extends Error {
  constructor(code: ViewErrorCode, message: string);
  readonly name: "ViewError";
  readonly code: ViewErrorCode;
}

export interface TextNode {
  readonly kind: "text";
  readonly text: string;
}

export interface ElementNode {
  readonly kind: "el";
  readonly tag: string;
  readonly key: string | number | undefined;
  readonly attrs: Readonly<Record<string, string>>;
  readonly on: Readonly<Record<string, (event: Event) => void>>;
  readonly children: readonly VNode[];
}

export type VNode = TextNode | ElementNode;
export type Child = VNode | string | number | null | undefined | false;

export interface ElementProps {
  readonly key?: string | number;
  readonly attrs?: Readonly<Record<string, string>>;
  readonly on?: Readonly<Record<string, (event: Event) => void>>;
}

export declare function text(value: string | number): TextNode;
export declare function el(
  tag: string,
  props?: ElementProps | null,
  children?: readonly Child[],
): ElementNode;

/** The only way attributes reach the DOM; production sinks come from `view/safe/` (T3-1b). */
export interface AttrSinks {
  setAttr(element: Element, name: string, value: string): void;
  removeAttr(element: Element, name: string): void;
}

export interface Renderer {
  mount(parent: Node, vnode: VNode): void;
  patch(parent: Node, oldVnode: VNode, newVnode: VNode): void;
  unmount(parent: Node, vnode: VNode): void;
}

export declare function createRenderer(doc: Document, sinks: AttrSinks): Renderer;

// ---------------------------------------------------------------------------------------
// 8. Offline app shell (`runtime/js/shell/`, T3-2; ARCHITECTURE 5.10, SPEC AC-62)

export interface ShellManifest {
  /** Build hash; changes whenever any shell file changes. */
  readonly build: string;
  /** HTML entry, relative to the scope. */
  readonly entry: string;
  readonly files: readonly string[];
  /** Navigation routes answered with the cached entry. */
  readonly routes?: readonly string[];
}

export type ShellStatus = "unsupported" | "ready" | "reloading" | "update-stuck" | "failed";

/** Page side: registers `sw.js` and activates a waiting build on the next load. */
export declare function startShell(
  host: Window,
  swUrl?: string,
): Promise<{ readonly status: ShellStatus; readonly error?: unknown }>;

/** The part of the service worker global scope that `attachShell` uses. */
export interface ShellWorkerScope extends EventTarget {
  readonly caches: CacheStorage;
  readonly registration: { readonly scope: string };
  fetch(input: RequestInfo, init?: RequestInit): Promise<Response>;
  skipWaiting(): Promise<void>;
}

/** Worker side: called once by the generated `sw.js`. */
export declare function attachShell(self: ShellWorkerScope, manifest: ShellManifest): void;

// ---------------------------------------------------------------------------------------
// 9. App entry (generated client)
//
// ASSUMPTION A3: the generated client module calls `startApp` once from the HTML entry after
// `startShell`. Auth (`std/auth`, T4) plugs in through `options.auth` and is absent in apps
// without `auth`, so those bundles contain no auth code (AC-46).

export interface AppOptions {
  /** WebSocket endpoint, relative to the origin. */
  readonly endpoint: string;
  /** Root element the app view is mounted into. */
  readonly root: Element;
  readonly auth?: AuthProvider;
}

/** Supplies the PASETO token for `Hello` (ARCHITECTURE 8). Implemented by std/auth (T4). */
export interface AuthProvider {
  /** Resolves with a `v4.public` token, or `null` when no user is signed in. */
  token(): Promise<string | null>;
  /** User id of the signed in user, or `null`. */
  readonly user: RowId | null;
}

export interface App {
  readonly store: Store;
  readonly calls: Calls;
  stop(): void;
}

export declare function startApp(options: AppOptions): Promise<App>;
