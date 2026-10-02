# Node sidecar host

`host.mjs` runs the TS/JS modules named in `extern js server` blocks for the
API server (ARCHITECTURE 7.2). The server starts one host per server process
and talks to it over JSON-RPC 2.0 on stdin and stdout. The host uses only
`node:` modules and needs no package install. Node 22 is required.

The host is not a sandbox. Extern modules are trusted application code; the
Node permission model and the empty environment only limit accidents.

## Start

```
node --permission --allow-fs-read=<app>/extern runtime/node/host.mjs \
  --module <name>=<absolute path> [--module <name>=<absolute path> ...]
```

* The server passes an empty environment (no database URL, no keys).
* `<name>` matches `[A-Za-z_][A-Za-z0-9_]*` and is unique.
* If an argument is invalid or a module fails to load, the host writes the
  reason to stderr and exits with code 2 before announcing ready.
* When the server closes stdin, the host exits with code 0, also with calls
  still running.

## Framing

One JSON object per line, UTF-8, at most 1 MiB per line in each direction
(newline excluded). stdout carries protocol messages only. `console.log`,
`console.info`, `console.debug` and `process.stdout.write` inside extern
modules are routed to stderr, which the server forwards to its log.

A request line over 1 MiB is dropped up to its newline and answered with
`-32600` and id `null`; the host keeps serving.

## Messages

Host to server, once after all modules are loaded (a notification):

```
{"jsonrpc":"2.0","method":"ready","params":{"protocol":1,"modules":{"<name>":["<fn>", ...]}}}
```

Server to host:

```
{"jsonrpc":"2.0","id":1,"method":"ping"}
{"jsonrpc":"2.0","id":2,"method":"call","params":{"module":"<name>","fn":"<export>","args":[...]}}
{"jsonrpc":"2.0","method":"cancel","params":{"id":2}}
```

* `id` is a string or a safe integer. Requests without `id` are
  notifications and are never answered. The id of a running call cannot be
  reused until that call has been answered or cancelled (`-32600`).
* `ping` answers `"pong"`.
* `call` looks up `fn` among the own exports of the module, awaits the
  result and answers `{"jsonrpc":"2.0","id":2,"result":<value>}`.
* Calls run concurrently; answers come in completion order.
* A call holds one of the 64 slots until its promise settles or the server
  cancels it. The host has no clock of its own; the server owns the deadline
  of every call (5 s unless the extern block says otherwise).
* `cancel` is a notification: it frees the slot of the running call with
  that id and sends no answer, also not when the promise settles later.
  Unknown ids are ignored. The server sends it when its deadline for the
  call has passed, so a promise that never settles cannot block the host.

## Errors

| Code | Meaning | `id` of the answer | `data.kind` |
|---|---|---|---|
| -32700 | Line is not valid JSON | `null` | |
| -32600 | Not a JSON-RPC 2.0 request object (also `jsonrpc` missing or not `"2.0"`, `method` not a string), bad id, or request over 1 MiB | the id if the line is an object with a valid id, else `null` | |
| -32601 | Unknown method | request id | |
| -32602 | Bad params, unknown module, or unknown function | request id | |
| -32000 | The extern call failed | request id | `Threw`, `Undefined`, `Type`, `TooLarge`, `Busy` |

* `Threw`: the function threw or its promise rejected. The message is
  `<name>: <message>` of the error, cut to 256 characters.
* `Undefined`: the function returned `undefined`.
* `Type`: the result is not a plain JSON value: `undefined` inside a value,
  functions, symbols, `BigInt`, `NaN` or infinite numbers, strings or keys
  with a lone surrogate, sparse arrays, objects that are not plain (for
  example `Date` or `Map`), accessor properties, cycles, or nesting deeper
  than 256. Negative zero is accepted and arrives as `0`.
* `TooLarge`: the response is over 1 MiB, or the result has more than
  1 048 576 values counted per path (a value reached twice through shared
  references counts twice, as it does in the JSON text).
* `Busy`: 64 calls are already running.

The host does not check results against the declared Ostrel types. The type
guards on the server side do that, including the `Int` range (D24).

Timeouts and crashes are handled by the supervisor in `ostrel_server`
(ARCHITECTURE 7.2, D60): it never sends a 65th call; when a call passes its
deadline it fails it with `ExternError::Timeout`, sends `cancel` and a
`ping`; a host that does not answer the `ping` within 1 s (for example a
module stuck in an endless loop) is killed and restarted with backoff, and
its running calls fail with `ExternError::Crashed` or `Timeout`.
