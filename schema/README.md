# herdr2 API

`h2.openrpc.json` is the complete request/response API as an
[OpenRPC](https://open-rpc.org) document: every method with its params,
result, and errors in JSON Schema. It is generated from the Rust types (checked
by test `m0`), and `h2 schema` or `rpc.discover` returns the same document.

Status: preview (protocol generation 1).

## Connecting

On Unix, core listens on `$XDG_RUNTIME_DIR/herdr2/<session>.sock` (or fallback paths);
on Windows, a named pipe. See [security docs](../docs/security.md#state-and-endpoints).
`h2 connect` bridges stdin/stdout to the socket, enabling `ssh box herdr2 connect`.

A connection starts in **plain mode** (JSON-RPC 2.0, one message per line):

```
-> {"jsonrpc":"2.0","id":1,"method":"agent.list","params":{}}
<- {"jsonrpc":"2.0","id":1,"result":{"agents":[...],"hidden_finished":0,"machines":[]}}
```

Errors return a stable `data.kind` and `data.hint`:

```
{"code":-32000,"message":"no agent \"a9\"","data":{"kind":"not_found","hint":"..."}}
```

Calling `hello` with `{"framed": true}` upgrades the connection to **framed mode**
(`[channel u32 LE][kind u8][len u32 LE][payload]`, DATA=0, CREDIT=1, CLOSE=2) for
concurrent calls and flow-controlled event streams (`events.subscribe`).

## ACP Agent Lifecycle & Permissions

- Every launched agent connects to an ACP adapter over stdio NDJSON (`initialize` with `fs: false, terminal: false`, then `session/new` or `session/load`).
- By default (`inject_mcp: true`), `herdr2` injects its built-in MCP server (`h2 mcp`) into `mcpServers`.
- Incoming prompts from `agent.send` enqueue serially into the agent's inbox (`inbox_len`); `--steer` (`steer: true`) sends `session/cancel` and places the prompt at the front of the queue.
- Permission prompts from `session/request_permission` are evaluated against `permission_mode` (`approve-reads` default). Non-auto-resolved requests set `status.state = "blocked"` with `pending_permission` (`request_id`, `session_id`, `title`, `kind`, `options`) and resolve via `agent.approve` / `agent.deny`.
- Full transcripts persist in `transcripts/<addr>.jsonl` and render via `agent.read` (`last`, `lines`).

## Events

`events.subscribe` replies with a snapshot (`seq`, matching `agents`) and streams
ordered notifications:

```
{"jsonrpc":"2.0","method":"event","params":{"seq":42,"type":"status","agent":"a7","data":{...}}}
{"jsonrpc":"2.0","method":"heartbeat","params":{"seq":42}}
{"jsonrpc":"2.0","method":"missed","params":{"seq":57,"agents":[...]}}
```

| type | data | apply |
|---|---|---|
| `created` | full `Agent` record | insert record |
| `lost` | `{reason, exit_code, signal, finished_ms: null}` | kept agent stays `live` until `restarted`; clear `pid` |
| `restarted` | full `Agent` record | replace record after `session/load` recovery |
| `keep` | `{keep}` | update `keep` |
| `finished` | `{reason, exit_code, signal, finished_ms}` | mark `state: "finished"` |
| `removed` | `{}` | drop record |
| `status` | `{status, records, reported, pending_permission, inbox_len}` | update live status fields |
| `turn` | `{turn, state}` | update `turn` |
| `labels` | `{labels}` (plus `record` on newly matching filter) | replace `labels` |
| `doc` | `{name, version}` | document updated (fetch via `doc.get`) |

## Documents

`doc.get {name, on?}` and `doc.set {name, value, version, on?}` provide optimistic-concurrency
opaque JSON storage (`conflict` on stale version, persisted atomically in `docs.json`, 1 MiB max).
