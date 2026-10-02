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
{"jsonrpc":"2.0","method":"ready","params":{"modules":{"<name>":["<fn>", ...]}}}
```

Server to host:

```
{"jsonrpc":"2.0","id":1,"method":"ping"}
{"jsonrpc":"2.0","id":2,"method":"call","params":{"module":"<name>","fn":"<export>","args":[...]}}
```

* `id` is a string or a safe integer. Requests without `id` are
  notifications and are never answered.
* `ping` answers `"pong"`.
* `call` looks up `fn` among the own exports of the module, awaits the
  result and answers `{"jsonrpc":"2.0","id":2,"result":<value>}`.
* Calls run concurrently; answers come in completion order.

## Errors

| Code | Meaning | `data.kind` |
|---|---|---|
| -32700 | Line is not valid JSON (id `null`) | |
| -32600 | Not a JSON-RPC 2.0 request object, bad id, or request over 1 MiB | |
| -32601 | Unknown method | |
| -32602 | Bad params, unknown module, or unknown function | |
| -32000 | The extern call failed | `Threw`, `Undefined`, `Type`, `Busy` |

* `Threw`: the function threw or its promise rejected. The message is
  `<name>: <message>` of the error, cut to 256 characters.
* `Undefined`: the function returned `undefined`.
* `Type`: the result is not a plain JSON value: `undefined` inside a value,
  functions, symbols, `BigInt`, `NaN` or infinite numbers, strings or keys
  with a lone surrogate, sparse arrays, objects that are not plain (for
  example `Date` or `Map`), accessor properties, cycles, nesting deeper than
  256, or a response over 1 MiB.
* `Busy`: 64 calls are already running.

The host does not check results against the declared Ostrel types. The type
guards on the server side do that, including the `Int` range (D24).

Timeouts, crash detection, restart with backoff and the mapping to
`ExternError::Crashed` and `Timeout` belong to the supervisor in
`ostrel_server`.
