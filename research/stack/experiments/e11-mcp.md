# E11 MCP surface (rmcp)

Date: 2026-10-20. Rust 1.99.1 (rustc 58cf8f9c3, 2026-10-13). rmcp `=3.5.1`, tokio 1.54.0, serde 1.0.229, serde_json 1.0.154, schemars 1.2.3 (via rmcp `server`). Probe only: rmcp-macros 3.6.0 resolved beside rmcp 3.5.1; `src/` identical to rmcp-macros 3.5.1 (`diff -rq` empty). crates.io newest rmcp: 3.6.0 (crate updated 2026-10-20 05:53 UTC). Untested.
Commit: `e392d3b`, branch `spike/e11-mcp`, worktree `/Users/ryzhakar/pp/estafette-e11-mcp`, pushed. Packages `spikes/e11-mcp/` (server, driver, probe) and `spikes/e11-manifest/` (draft manifest; resolved, never built). Both excluded from workspace (`exclude = ["spikes"]`, root `Cargo.toml`). `target/` ignored.
Machine: Apple M2 Max, 12 cores, 32 GiB, macOS 15.3.1 (24D70), Darwin 24.3.0 arm64. Shutdown timings: release builds.
Spec: `spec/blueprint.md` D28, D30, 3.9, 3.10, 3.14, 3.15 (tests 12, 13, 14), 3.16.
Paths: `src/...` = `rmcp-3.5.1/src/...`; `rmcp-macros:` = `rmcp-macros-3.5.1/src/...`; `tokio:` = `tokio-1.54.0/src/...`; all under `~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/`. `main.rs` = `spikes/e11-mcp/src/main.rs`.

Built. `main.rs`: hand-written `ServerHandler`, no rmcp macros. Six static tools, stub bodies, 3.10 envelope, SIGTERM handling. `src/bin/driver.rs`: raw newline-delimited JSON-RPC client over child pipes; no rmcp client. `src/bin/probe.rs`: one tool on rmcp `#[tool]` macros; measures library defaults. Terms: naive = return from `main`, runtime dropped (`drop(rt)`). bg = `rt.shutdown_background()`.
Reproduce from `spikes/e11-mcp/`: `cargo build --release`; `./target/release/driver wire <protocol version>`; `./target/release/driver shutdown 10`. Probe: `cargo build --features probe --bin probe --bin driver`; `./target/debug/driver probe`. Toolchain: `rustup run 1.99.1 cargo`.

Answer. rmcp 3.5.1 carries D28, D30, 3.10 with hand-written `ServerHandler`: static list, `you` in every envelope, `bad_request` envelope, descriptions byte-identical to 3.10. Four defaults break spec. Macro `Parameters<T>` failures bypass envelope. Macro `get_info` names server `rmcp`. `waiting()` holds up to 5 s after stdin EOF with call in flight. Return from `main` after SIGTERM hangs on pending stdin read; `shutdown_background()` fixes it (measured). No measured fix for 5 s EOF drain.

## Results

| Question | Answer | Evidence |
|---|---|---|
| rmcp features for stdio server? | `server` + `transport-io`, `default-features = false`. Builds, serves. `macros`, `base64` unneeded. | rmcp `Cargo.toml:88` (default), `:118` (server), `:146` (transport-io); spike `Cargo.toml`. |
| 1. `tools/list` identical before and after successful `init` (test 12)? | Yes. Parsed `result` equal on 2024-11-05, 2025-03-26, 2025-06-18, 2025-11-25, 2026-07-28. Names: six, 3.10 order. | Driver `wire`, check "test 12", PASS x5. List: `Vec<Tool>` built once (`main.rs:117-129`), returned by `list_tools` (`main.rs:225-231`). |
| 1b. rmcp advertises `tools.listChanged`? | No. Wire: `"capabilities":{"tools":{}}` in `initialize` and `server/discover`; hand-written and macro-generated `get_info` alike. Wire sessions: no unsolicited server message. | `src/model/capabilities.rs:54-61` (`list_changed: Option<bool>`, omitted when `None`), `:313-324` (`enable_tools()` leaves `None`); `rmcp-macros:tool_handler.rs:136-139` (macro `get_info` calls `.enable_tools()` only). |
| 1c. Can it be turned off? | Nothing to turn off. Off unless `get_info` calls `enable_tool_list_changed()`. rmcp ships runtime mutation: `ToolRouter::add_route`, `remove_route`, `disable_route`, `enable_route`; notifier sends `notifications/tools/list_changed`. Spike uses none: no `ToolRouter`; `Vec<Tool>`, no interior mutability. | `capabilities.rs:386-391`; `src/handler/server/router/tool.rs:436-489, 508-529`; `src/service/server.rs:268`. |
| 2. Envelope on MCP tool result? | `CallToolResult::structured(env)` for `ok:true`; `structured_error(env)` for `ok:false`. Client gets `content[0].text` = envelope as JSON string; `structuredContent` = envelope object; `isError` = `!ok` (explicit `false` on success). 2026-07-28 adds `"resultType":"complete"`. Checks PASS x5: text parses to same JSON as `structuredContent`; `isError == !ok`. | `src/model.rs:4155-4163, 4182-4190`; `src/handler/server.rs:255-259` (`resultType` stripped for older peers). Exact lines below. |
| 2b. `you` in every response (test 13)? | Yes. 17 `tools/call` envelopes on wire; driver asserts 16. Before init: `unclaimed`, `card:null`, `handle:null`. After: `claimed`, `alice`, `alice`. Both `ok:true`, `ok:false`. `you` read after call; `init` reply already says `claimed`. Not covered: `displaced`; non-object `arguments` (row 3b). | Check "test 13", PASS x5. `main.rs:201-216`. |
| 3. rmcp on schema-failing input? | Default path (`Parameters<T>`): tool-level error, not JSON-RPC. `isError:true`; `content[0].text` = serde message, e.g. ``failed to deserialize parameters: missing field `body` ``. No `structuredContent`, `ok`, `you`, or code. Unknown tool: JSON-RPC `-32602 "tool not found"`. Left default: breaks test 13 and closed code list. | `src/handler/server/tool.rs:235-250`; `src/handler/server/router/tool.rs:144-156, 573-576` (conversion), `:564-571` (unknown tool). Probe lines below. |
| 3b. Return our envelope instead? | Yes, for every input reaching `call_tool`: missing field, wrong type, negative `since`, absent `arguments`, unknown tool. Method: no `Parameters<T>`; hand-written `call_tool` runs `serde_json::from_value`; failure becomes `bad_request` envelope. No, for non-object `arguments` (string, array): rmcp answers `-32601 "tools/call"` before our code runs. No envelope, no `you`. Cause: `ClientRequest` is untagged union ending in `CustomRequest`. `CustomRequest` takes any method. `on_custom_request` answers `METHOD_NOT_FOUND` with method name. `on_custom_request` override: untested. | `main.rs:113-115, 233-240`; `src/model.rs:4632-4652`; `src/model/serde_impl.rs:431-449`; `src/handler/server.rs:223-226, 552-564`. |
| 4. Stdin closed: time to exit? | Idle: median 1 ms (n=10). Before handshake: 1 ms, exit status 1 (`ConnectionClosed("initialize request")`). Call in flight: median 5003 ms (n=10), cooperative or not. rmcp drains in-flight responses up to 5 s after EOF before `waiting()` returns. `ctx.ct` stays uncancelled in drain; cooperation does not shorten it (eof-coop = eof-slow). | Shutdown table. `src/service.rs:1420-1428` (EOF to `Closed`), `:1762-1766`, `:1767-1803` (5 s / 2 s drain), `:1175-1186` (drop guard cancels after `waiting()` returns). |
| 4b. SIGTERM: time to exit? | naive: no exit within 4000 ms, 10/10, all four stages (before handshake, idle, call in flight, cooperative call in flight). Exits ~1 ms after driver closes stdin. bg medians: before handshake 1 ms, idle 1 ms, cooperative call 1 ms; call in flight 2003 ms (rmcp 2 s drain after cancel). | Shutdown table, timelines. `src/service.rs:1456-1459` (cancel to `Cancelled`), `:1762-1766`. |
| 4c. Pending stdin read hangs exit? | Yes, on signal with stdin open: `Runtime::drop` waits for blocking thread stuck in `read(0)`. Not on EOF: read returns. `rt.shutdown_background()` avoids it (measured). `std::process::exit` would too (untested); root `Cargo.toml` sets `exit = "deny"`. Lead's hypothesis confirmed. | `tokio:io/stdin.rs:15-22`; `tokio:runtime/runtime.rs:39-44, 457-494`; `src/transport/io.rs:4-6` (`stdio()` = `tokio::io::stdin()` + `stdout()`). |
| 5. LOC, server, stubs' bodies excluded | `main.rs`: 278 lines; 250 code (non-blank, non-comment, after `cargo fmt`). Stub region (`dispatch`, six tool bodies): 62. **Excluded: 188.** Of 188: tool table 52; imports and consts 29; arg structs 19; `parse`, struct, `new`, `claimed`, closing brace 18; `ServerHandler` impl 21; envelope 15; `main` + `run` 34 (9 measurement scaffolding: four `eprintln!`, `bg` branch). Driver: 374 code lines. Probe: 23. | `main.rs:135-199` = stub region (marker comments). Count: lines not blank, not starting `//`. |
| 5b. Tree, server package | 54 unique name+version, root included (53 names). Literal recipe: 63; `(*)`, `(proc-macro)` suffixes split one crate into several lines. | Tree counts. |
| 6. Draft manifest | 280 unique without `iroh-relay` `server`; 321 with. rmcp default features: 285, 326. | Tree counts. |

## Wire examples (exact JSON for success and failure)

Server stdout, unedited, one JSON-RPC message per line. Keys alphabetical: default `serde_json` (Surprises 7). Protocol 2025-11-25 unless stated.

Handshake. `capabilities.tools` is `{}`: no `listChanged`.

```json
{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-11-25","capabilities":{"tools":{}},"serverInfo":{"name":"estafette","version":"0.0.0"}}}
```

Success, before init. `list_cards`, input `{}`. `content[0].text`: envelope as string. `structuredContent`: same envelope as object. `isError:false`.

```json
{"jsonrpc":"2.0","id":3,"result":{"content":[{"type":"text","text":"{\"ok\":true,\"result\":{\"cards\":[{\"handle\":\"alice\",\"held\":false,\"name\":\"alice\"},{\"handle\":\"taken\",\"held\":true,\"name\":\"taken\"}]},\"you\":{\"card\":null,\"handle\":null,\"status\":\"unclaimed\"}}"}],"structuredContent":{"ok":true,"result":{"cards":[{"handle":"alice","held":false,"name":"alice"},{"handle":"taken","held":true,"name":"taken"}]},"you":{"card":null,"handle":null,"status":"unclaimed"}},"isError":false}}
```

Failure `not_claimed`. `peers` before init. `isError:true`.

```json
{"jsonrpc":"2.0","id":4,"result":{"content":[{"type":"text","text":"{\"error\":{\"code\":\"not_claimed\",\"message\":\"Claim a card with init first.\"},\"ok\":false,\"you\":{\"card\":null,\"handle\":null,\"status\":\"unclaimed\"}}"}],"structuredContent":{"error":{"code":"not_claimed","message":"Claim a card with init first."},"ok":false,"you":{"card":null,"handle":null,"status":"unclaimed"}},"isError":true}}
```

Success after init. `init` with `{"card":"alice","description":"stub"}`. `you` now `claimed`.

```json
{"jsonrpc":"2.0","id":15,"result":{"content":[{"type":"text","text":"{\"ok\":true,\"result\":{\"handle\":\"alice\",\"inbox_path\":\"/tmp/e11-mcp.sock\"},\"you\":{\"card\":\"alice\",\"handle\":\"alice\",\"status\":\"claimed\"}}"}],"structuredContent":{"ok":true,"result":{"handle":"alice","inbox_path":"/tmp/e11-mcp.sock"},"you":{"card":"alice","handle":"alice","status":"claimed"}},"isError":false}}
```

Failure `bad_request`. `send` with `{"to":"bob"}` (`body` missing), before init. `bad_request` beats `not_claimed`.

```json
{"jsonrpc":"2.0","id":8,"result":{"content":[{"type":"text","text":"{\"error\":{\"code\":\"bad_request\",\"message\":\"The input did not match the tool schema.\"},\"ok\":false,\"you\":{\"card\":null,\"handle\":null,\"status\":\"unclaimed\"}}"}],"structuredContent":{"error":{"code":"bad_request","message":"The input did not match the tool schema."},"ok":false,"you":{"card":null,"handle":null,"status":"unclaimed"}},"isError":true}}
```

Failure `bad_request`, state Claimed (3.9). Second `init`, `{"card":"alice"}`. `you` stays `claimed`.

```json
{"jsonrpc":"2.0","id":16,"result":{"content":[{"type":"text","text":"{\"error\":{\"code\":\"bad_request\",\"message\":\"The input did not match the tool schema.\"},\"ok\":false,\"you\":{\"card\":\"alice\",\"handle\":\"alice\",\"status\":\"claimed\"}}"}],"structuredContent":{"error":{"code":"bad_request","message":"The input did not match the tool schema."},"ok":false,"you":{"card":"alice","handle":"alice","status":"claimed"}},"isError":true}}
```

Failure `bad_request`, bound. `inbox` with `{"since":-1}`: passes advertised schema (`integer`), fails `u64` parse.

```json
{"jsonrpc":"2.0","id":10,"result":{"content":[{"type":"text","text":"{\"error\":{\"code\":\"bad_request\",\"message\":\"The input did not match the tool schema.\"},\"ok\":false,\"you\":{\"card\":null,\"handle\":null,\"status\":\"unclaimed\"}}"}],"structuredContent":{"error":{"code":"bad_request","message":"The input did not match the tool schema."},"ok":false,"you":{"card":null,"handle":null,"status":"unclaimed"}},"isError":true}}
```

Test 14 shape. `broadcast` to `["a","ghost","b"]`: three results, two delivered, one `unknown_handle`. Call itself succeeded: `ok:true`.

```json
{"jsonrpc":"2.0","id":19,"result":{"content":[{"type":"text","text":"{\"ok\":true,\"result\":{\"results\":[{\"attached\":false,\"delivered\":true,\"id\":\"stub\",\"to\":\"a\"},{\"delivered\":false,\"error\":{\"code\":\"unknown_handle\",\"message\":\"No present member has that handle.\"},\"to\":\"ghost\"},{\"attached\":false,\"delivered\":true,\"id\":\"stub\",\"to\":\"b\"}]},\"you\":{\"card\":\"alice\",\"handle\":\"alice\",\"status\":\"claimed\"}}"}],"structuredContent":{"ok":true,"result":{"results":[{"attached":false,"delivered":true,"id":"stub","to":"a"},{"delivered":false,"error":{"code":"unknown_handle","message":"No present member has that handle."},"to":"ghost"},{"attached":false,"delivered":true,"id":"stub","to":"b"}]},"you":{"card":"alice","handle":"alice","status":"claimed"}},"isError":false}}
```

Request never reaching handler. `arguments` is string `"oops"`. JSON-RPC error. No envelope, no `you`.

```json
{"jsonrpc":"2.0","id":13,"error":{"code":-32601,"message":"tools/call"}}
```

`tools/list`. Line after init differs only in `id`.

```json
{"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"list_cards","description":"List the identity cards on this machine and whether each is in use.","inputSchema":{"properties":{},"type":"object"}},{"name":"init","description":"Claim one card for this session. Returns the handle you must use. Call once.","inputSchema":{"properties":{"card":{"type":"string"},"description":{"type":"string"}},"required":["card"],"type":"object"}},{"name":"peers","description":"List the members that are present, with their handles and descriptions.","inputSchema":{"properties":{},"type":"object"}},{"name":"send","description":"Send a text message to one member by handle. Returns whether it was delivered.","inputSchema":{"properties":{"body":{"type":"string"},"to":{"type":"string"}},"required":["to","body"],"type":"object"}},{"name":"broadcast","description":"Send one text message to several members by handle. Returns one result per member.","inputSchema":{"properties":{"body":{"type":"string"},"to":{"items":{"type":"string"},"type":"array"}},"required":["to","body"],"type":"object"}},{"name":"inbox","description":"Read messages that arrived for you. Use `since` to read only newer ones.","inputSchema":{"properties":{"since":{"type":"integer"}},"type":"object"}}]}}
```

Protocol 2026-07-28. No `initialize`; every request carries `_meta`. Added fields: `resultType`, `ttlMs`, `cacheScope`.

```json
{"jsonrpc":"2.0","id":1,"result":{"resultType":"complete","supportedVersions":["2024-11-05","2025-03-26","2025-06-18","2025-11-25","2026-07-28"],"capabilities":{"tools":{}},"ttlMs":0,"cacheScope":"private","_meta":{"io.modelcontextprotocol/serverInfo":{"name":"estafette","version":"0.0.0"}}}}
{"jsonrpc":"2.0","id":2,"result":{"resultType":"complete","ttlMs":0,"cacheScope":"private","tools":[{"name":"list_cards","description":"List the identity cards on this machine and whether each is in use.","inputSchema":{"properties":{},"type":"object"}},{"name":"init","description":"Claim one card for this session. Returns the handle you must use. Call once.","inputSchema":{"properties":{"card":{"type":"string"},"description":{"type":"string"}},"required":["card"],"type":"object"}},{"name":"peers","description":"List the members that are present, with their handles and descriptions.","inputSchema":{"properties":{},"type":"object"}},{"name":"send","description":"Send a text message to one member by handle. Returns whether it was delivered.","inputSchema":{"properties":{"body":{"type":"string"},"to":{"type":"string"}},"required":["to","body"],"type":"object"}},{"name":"broadcast","description":"Send one text message to several members by handle. Returns one result per member.","inputSchema":{"properties":{"body":{"type":"string"},"to":{"items":{"type":"string"},"type":"array"}},"required":["to","body"],"type":"object"}},{"name":"inbox","description":"Read messages that arrived for you. Use `since` to read only newer ones.","inputSchema":{"properties":{"since":{"type":"integer"}},"type":"object"}}]}}
{"jsonrpc":"2.0","id":4,"result":{"resultType":"complete","content":[{"type":"text","text":"{\"error\":{\"code\":\"not_claimed\",\"message\":\"Claim a card with init first.\"},\"ok\":false,\"you\":{\"card\":null,\"handle\":null,\"status\":\"unclaimed\"}}"}],"structuredContent":{"error":{"code":"not_claimed","message":"Claim a card with init first."},"ok":false,"you":{"card":null,"handle":null,"status":"unclaimed"}},"isError":true}}
```

rmcp defaults, probe on `#[tool]` macros. `initialize` names server `rmcp` `3.5.1`. `tools/list` shows schemars schema: `$schema`, nullable `since`.

```json
{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-11-25","capabilities":{"tools":{}},"serverInfo":{"name":"rmcp","version":"3.5.1"}}}
{"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"send","description":"Probe.","inputSchema":{"$schema":"https://json-schema.org/draft/2020-12/schema","properties":{"body":{"type":"string"},"since":{"format":"uint64","minimum":0,"type":["integer","null"]},"to":{"type":"string"}},"required":["to","body"],"type":"object"}}]}}
```

Probe `tools/call`. Inputs in order: `{"to":"a","body":"b"}`; `{"to":"a"}`; `{"to":5,"body":"b"}`; `{"to":"a","body":"b","since":-1}`; no `arguments`; unknown tool `nope`; `arguments` string `"oops"`.

```json
{"jsonrpc":"2.0","id":3,"result":{"content":[{"type":"text","text":"a b None"}],"isError":false}}
{"jsonrpc":"2.0","id":4,"result":{"content":[{"type":"text","text":"failed to deserialize parameters: missing field `body`"}],"isError":true}}
{"jsonrpc":"2.0","id":5,"result":{"content":[{"type":"text","text":"failed to deserialize parameters: invalid type: integer `5`, expected a string"}],"isError":true}}
{"jsonrpc":"2.0","id":6,"result":{"content":[{"type":"text","text":"failed to deserialize parameters: invalid value: integer `-1`, expected u64"}],"isError":true}}
{"jsonrpc":"2.0","id":7,"result":{"content":[{"type":"text","text":"failed to deserialize parameters: missing field `to`"}],"isError":true}}
{"jsonrpc":"2.0","id":8,"error":{"code":-32602,"message":"tool not found"}}
{"jsonrpc":"2.0","id":9,"error":{"code":-32601,"message":"tools/call"}}
```

## Shutdown timings

Milliseconds, trigger to process exit. Release build. n=10 per case.

| Case | min | median | max | n |
|---|---|---|---|---|
| stdin closed before handshake (exit status 1) | 1 | 1 | 2 | 10 |
| stdin closed, idle | 1 | 1 | 6 | 10 |
| stdin closed, `send` in flight | 5002 | 5003 | 5004 | 10 |
| stdin closed, cooperative `send` in flight | 5002 | 5003 | 5004 | 10 |
| SIGTERM before handshake, naive | hung | hung | hung | 10 |
| SIGTERM, idle, naive | hung | hung | hung | 10 |
| SIGTERM, `send` in flight, naive | hung | hung | hung | 10 |
| SIGTERM, cooperative `send` in flight, naive | hung | hung | hung | 10 |
| SIGTERM before handshake, bg | 1 | 1 | 3 | 10 |
| SIGTERM, idle, bg | 1 | 1 | 1 | 10 |
| SIGTERM, `send` in flight, bg | 2002 | 2003 | 2007 | 10 |
| SIGTERM, cooperative `send` in flight, bg | 1 | 1 | 6 | 10 |

Method. `driver shutdown 10`. Driver holds stdin pipe open unless trigger is EOF. Trigger fires 300 ms after spawn: call in flight or read pending. Clock starts when `drop(stdin)` or `kill(pid, SIGTERM)` returns; stops when `try_wait` (1 ms poll) sees exit. Cap: 12 s for EOF, 4 s for SIGTERM. "hung" = alive at cap, stdin open. After driver closed stdin, hung processes exited in median 1 ms. In-flight call: `send` with body `slow:30000` (30 s sleep) or `coop:30000` (same; returns on `ctx.ct` cancel). Earlier n=5 run agrees: medians within 1 ms; hung 5/5.

Timelines (server stderr, last run per case; process-relative; trigger at about 0.30 s):

- stdin closed, call in flight: `stopped` 5.307 s, `runtime dropped` 5.308 s. rmcp held `waiting()` 5.0 s.
- SIGTERM, call in flight, naive: `stopped` 2.306 s (2.0 s drain); `runtime dropped` 4.305 s (driver closed stdin at cap).
- SIGTERM, idle, naive: `runtime returned` 0.304 s; `runtime dropped` 4.305 s (same).
- SIGTERM, cooperative call, naive: `stopped` 0.307 s; `runtime dropped` 4.306 s (same).
- SIGTERM, any stage, bg: `runtime dropped` within 0.1 ms of `runtime returned`.

## Tree counts

Recipe = `cargo tree --prefix none -e normal | sort -u | wc -l`. Over-counts. Headline = unique name+version, root included; `(*)`, `(proc-macro)`, path suffixes stripped. Host target aarch64-apple-darwin. Lockfiles committed: `spikes/e11-mcp/Cargo.lock`, `spikes/e11-manifest/Cargo.lock`.

Server package `spikes/e11-mcp`:

| Variant | Recipe | Unique name+version | Unique names | Delta |
|---|---|---|---|---|
| As built: rmcp `server` + `transport-io`, no macros | 63 | 54 | 53 | none |
| + rmcp `macros` (`--features probe`) | 70 | 60 | 59 | +6: darling, darling_core, darling_macro, ident_case, rmcp-macros 3.6.0, strsim |
| `-e normal,build` | 64 | 55 | 54 | +1: autocfg 1.5.1 |
| `-e normal --target all` | 89 | 74 | 73 | +20: windows-* (7), wasm-bindgen-* (4), js-sys, bumpalo, iana-time-zone, iana-time-zone-haiku, android_system_properties, core-foundation-sys, log, r-efi, wasi |
| + `serde_json/preserve_order` (scratch copy) | 65 | 55 | 54 | +1: foldhash 0.2.0 |

Draft manifest `spikes/e11-manifest`. Cells: recipe / unique name+version (unique names). Resolution only; never compiled.

| rmcp features | Without `iroh-relay` `server` | With `iroh-relay` 1.3.0 `server` |
|---|---|---|
| `server` + `transport-io`, default features off | 365 / 280 (275) | 422 / 321 (313) |
| defaults (`base64`, `macros`, `server`) + `transport-io` | 371 / 285 (280) | 428 / 326 (318) |

- Relay `server` adds 41. Includes asn1-rs (+derive, +impl), rcgen, x509-parser, yasna, tokio-rustls-acme, tracing-subscriber, dashmap, sha1, nom 7.1.3.
- rmcp defaults add 5: darling, darling_core, darling_macro, ident_case, rmcp-macros 3.6.0. strsim already in tree.
- Resolved: iroh 1.3.0, iroh-gossip 0.101.0, iroh-relay 1.3.0, rmcp 3.5.1, clap 4.6.7, serde 1.0.229, postcard 1.1.3, serde_json 1.0.154, blake3 1.8.7, data-encoding 2.11.1, uuid 1.28.0, tokio 1.54.0, toml 1.1.8+spec-1.1.0, yaml-rust2 0.13.0, libc 0.2.191. `toml`, `yaml-rust2` written `*`.
- Cross-check: `research/stack/ground/tree.md` counted 276 (iroh, iroh-gossip, rmcp defaults + `transport-io`, clap derive; same definition). This manifest: 285. Net +9. Extra here: postcard, serde_json, blake3, data-encoding, uuid v7, toml, yaml-rust2, libc, more tokio features. Lists not diffed.

## Against the spec

Verdicts: CONFIRMED, CONTRADICTED, GAP (spec silent; run forced choice), NOT EXERCISED. Row numbers refer to Results.

| Spec sentence | Verdict | Evidence |
|---|---|---|
| D28: "exposes exactly these tools at all times: list_cards, init, peers, send, broadcast, inbox." | CONFIRMED. Six names, 3.10 order, before and after init, five protocol versions. List hand-built. rmcp macro router sorts by name (`src/handler/server/router/tool.rs:588`): order would differ. | Row 1; check "names are the six". |
| D28: "Before a successful init, every tool except list_cards and init returns the not_claimed error." | CONFIRMED for `peers`, `send`, `broadcast`, `inbox` with valid input. GAP: invalid input before init returns `bad_request`, not `not_claimed` (spike parses before state check). Spec silent on precedence. | Check "D28: peers, send, broadcast, inbox return not_claimed"; wire example `send before init, body missing`. |
| D28 reason: "A static tool list depends on nothing in the harness." | CONSISTENT. No `listChanged`. No `list_changed` notification. | Row 1b. |
| D30: "Each response contains the card, the effective handle, and the claim status of the caller." | CONFIRMED for `unclaimed`, `claimed`: 17 of 17 envelopes. NOT EXERCISED: `displaced`. CONTRADICTED for request never reaching handler (non-object `arguments`): JSON-RPC error, no `you`. GAP: spec silent on `you` read before or after call; spike reads after. | Rows 2b, 3b. |
| 3.9: "In Unclaimed and Displaced, every tool except list_cards and init returns not_claimed." | CONFIRMED for Unclaimed. Displaced NOT EXERCISED. | Row 2b. |
| 3.9: "Init in state Claimed returns bad_request." | CONFIRMED. `you` stays `claimed`. | Wire example `init again`. |
| 3.9, 3.14, 3.15 test 4: "any state --stdin closed or signal--> Stopping --Departure sent--> exit"; departure wait at exit 1000 ms; peers see Gone within 1000 ms. | CONTRADICTED for binary that sends Departure after `waiting()` returns. Stdin closed with call in flight: `waiting()` held 5.0 s. SIGTERM with call in flight: held 2.0 s. Naive return from `main` after SIGTERM: hangs. Real `send` runs up to 15 s (5000 connect + 10000 ack, 3.10). Idle cases: 1 ms. Departure not built. | Rows 4, 4b, 4c. |
| 3.10: "Transport: standard input and output." | CONFIRMED. Driver parsed every stdout line in wire sessions. Logs go to stderr. | Wire sessions. |
| 3.10: "The server name is a build constant." | CONFIRMED for hand-written `get_info` (`main.rs:28, 220-223`). CONTRADICTED by rmcp macro default: `serverInfo` = `rmcp` `3.5.1`. | `src/model.rs:1556-1565`; `rmcp-macros:tool_handler.rs:143-152`; probe `initialize`. |
| 3.10: "The tool list is fixed at start and never changes." | CONFIRMED. | Rows 1, 1b. |
| 3.10: envelope `{ok, you, result}` and `{ok:false, you, error}`, "for every tool, success or failure". | CONFIRMED in content: 17 of 17 envelopes. Wire key order alphabetical; 3.10 shows `ok`, `you`, `result` or `error`. `serde_json` `preserve_order` restores 3.10 order (+1 crate). GAP: spec silent on envelope-to-MCP-result mapping; spike chose text + `structuredContent` + `isError`. | Row 2; Surprises 7. |
| 3.10: "`status` is one of `unclaimed`, `claimed`, `displaced`." | Two of three observed. | Row 2b. |
| 3.10 tools table, inputs: `init` `card: string`, `description?: string`; `send` `to`, `body`; `broadcast` `to: string[]`; `inbox` `since?: integer`. | CONFIRMED as advertised schemas (`tools/list`). GAP: `since?: integer` unbounded; spike parses `u64`; `since:-1` passes advertised schema, fails with `bad_request`. | Wire examples `tools/list`, `inbox, since negative`. |
| 3.10 tools table, results (`cards`, `handle`, `inbox_path`, `peers`, `id`, `delivered`, `attached`, `results`, `messages`, `last_seq`). | CONFIRMED: envelope carries these field names from stubs. Semantics NOT EXERCISED. | Wire sessions. |
| 3.10 `list_cards`: "On the first call after start, wait the observer wait of 3.14 before answering." | NOT EXERCISED. rmcp spawns one task per request; waiting call does not block others (source; unmeasured). | `src/service.rs:1603-1618`. |
| 3.10 `broadcast`: "Sends run concurrently." | NOT EXERCISED. | none |
| 3.10: "Error codes. This list is closed." | CONFIRMED for stub: codes emitted `not_claimed`, `bad_request`, `card_held`, `unknown_handle`, `timeout`; messages byte-equal to table (checked against `blueprint.md:734-745`). rmcp defaults outside list: untyped text errors, `-32602`, `-32601` (rows 3, 3b). GAP: unknown tool name; spike answers `bad_request`. | Wire sessions; `main.rs:30-35`. |
| 3.10: `bad_request`, "The input did not match the tool schema.", any tool. | CONFIRMED through hand-written parse. CONTRADICTED by rmcp default (row 3) and non-object `arguments` (row 3b). | Rows 3, 3b. |
| 3.10: "Tool descriptions, as given to the harness. Keep them exactly this short." | CONFIRMED byte-identical: `cmp` of six wire descriptions against six 3.10 bullets. | Driver `tools/list`; `cmp` run. |
| 3.15 test 12. | CONFIRMED. | Row 1. |
| 3.15 test 13. | CONFIRMED before and after init. After displacement NOT EXERCISED. | Row 2b. |
| 3.15 test 14. | CONFIRMED on stub: three results, two delivered, one `unknown_handle`; `isError:false`. | Wire example `broadcast`. |
| 3.16: "Do not add, remove, or hide a tool at runtime." | CONFIRMED in spike. rmcp ships the means (`ToolRouter` mutators, notifier); avoided by construction. | Row 1c. |
| 3.16: "Do not read any harness environment variable in the binary." | CONFIRMED for spike (reads argv only) and rmcp `src/` (no `env::var`, `std::env::`, `option_env`; only compile-time `env!` at `model.rs:1558, 1560`). Other crates in tree unchecked. | grep of rmcp 3.5.1 `src/`. |
| 3.16, other lines. | NOT EXERCISED (not MCP surface). | none |

## Surprises

1. Macro `get_info` names server `rmcp`, version `3.5.1`. `Implementation::from_build_env()` expands `env!` inside rmcp, not application (`src/model.rs:1556-1565`; `rmcp-macros:tool_handler.rs:151`). Fix: pass `name`, `version` to `#[tool_handler]`, or write `get_info`.
2. `rmcp =3.5.1` leaves `rmcp-macros` unpinned. rmcp declares caret `version = "3.5.1"` (`Cargo.toml:809-810`). With `macros`, cargo resolved rmcp-macros 3.6.0. Sources identical; no behavior change. Lockfile is only pin.
3. Malformed `arguments` (string, array) gets `-32601`, message `tools/call`: "method not found" for existing method. Untagged `ClientRequest` falls through to `CustomRequest` (row 3b).
4. EOF with call in flight: `waiting()` holds 5.0 s; cancel holds 2.0 s. Both rmcp constants (`src/service.rs:1762-1766`). `ctx.ct` fires only when `ct` is cancelled; drop guard cancels after `waiting()` returns. Cooperative handler learns of EOF too late. No rmcp API reports EOF earlier. `Peer::is_transport_closed` is `tx.is_closed()` (`src/service.rs:1053-1055`). Receiver lives in serve task until task ends (`src/service.rs:1350, 1382, 1429`; ends `:1811`).
5. SIGTERM with stdin open hangs `Runtime::drop` on stdin read, every stage including before handshake. First smoke run (n=2, no settle): `term-idle` exited in 1 ms; read not yet pending. Hang needs read in flight (item 6 under "What could make this wrong").
6. Literal recipe over-counts: 63 vs 54 (server); 365 vs 280 (manifest, base). Suffix variants of one crate count as separate lines.
7. Wire key order alphabetical: `serde_json` `Map` is `BTreeMap` without `preserve_order`. 3.10 shows `ok`, `you`, `result` or `error`. Verified in scratch copy: with `preserve_order`, wire matches 3.10 order; `inputSchema` keeps declared order. Cost +1 crate (foldhash 0.2.0).
8. 2026-07-28 `tools/list` carries `ttlMs:0`, `cacheScope:"private"`. rmcp fills defaults (`src/model.rs:4852-4853`). Static list gets no client caching unless `list_tools` sets TTL.
9. `since?: integer` advertised unbounded; parsed `u64`; `since:-1` returns `bad_request` (3.10 gap).
10. schemars schema for `Option<u64>`: `{"format":"uint64","minimum":0,"type":["integer","null"]}`, plus `$schema` key (probe `tools/list`). 3.10 says `since?: integer`. Hand-written schemas avoid nullable union.
11. rmcp 3.5.1 serves five protocol versions. 2026-07-28 has no `initialize`: first request carries `_meta` with version, client capabilities (`src/service/server.rs:578-687`; `src/model.rs:181-183`). One server handled both. Real harness versions unknown.
12. Macro path sorts tools by name per call (`src/handler/server/router/tool.rs:581-590`). 3.10 order is not alphabetical.
13. EOF before handshake: `serve()` returns `ConnectionClosed`; with `?`, exit status 1. 3.9: stdin closed in any state goes to Stopping, then exit.
14. Handshake phase needs own shutdown path. `serve()` returns only after first request. SIGTERM before then works: `select!` drops `serve()` future (bg: 1 ms). `serve_with_ct` takes token for this (`src/service/server.rs:446-462`); untested.

## What could make this wrong

1. One machine, one OS: macOS 15.3.1 arm64. Linux untested. tokio docs say stdin is blocking read; hang and timings unmeasured elsewhere.
2. rmcp 3.5.1 only. 3.6.0 exists; untested. rmcp moves fast: five protocol versions; `ServerInfo` now deprecated alias of `ServerConfig`.
3. Driver = own raw JSON-RPC client, not harness. Harnesses differ: protocol version, pipe closing, signal choice, rendering of `content` + `structuredContent` + `isError`. Untested: any harness rendering of envelope; harness rejection of `structuredContent` without `outputSchema`; whether `ok:false` as `isError:true` hides or prefixes text.
4. Stub bodies. No network, no gossip. Not exercised: `list_cards` 3000 ms wait, `broadcast` concurrency, `displaced`, real `send` latency (up to 15 s), non-empty `peers`.
5. In-flight call is a sleep. Real call holds sockets, timers. rmcp drain ignores work; never run with real work.
6. "hung" = no exit within 4 s, stdin open. Exit follows within 1 ms of stdin closing: hang lasts while pipe stays open; demonstrated to cap only. Hang needs read pending (Surprises 5). SIGTERM within microseconds of handshake can escape.
7. Timing resolution ~2 ms: 1 ms poll; clock starts after `kill(2)` or `drop(stdin)` returns. Release build only. n=10 per case. 5 s and 2 s equal rmcp constants: low spread expected.
8. Runtime flavor: current-thread (`Builder::new_current_thread`), matching draft's tokio features (no `rt-multi-thread`). Multi-thread untested.
9. `shutdown_background` abandons blocking pool at exit. Response mid-write at SIGTERM may truncate; unchecked. Output flush at exit checked in idle cases only.
10. No measured fix for 5 s EOF drain. Candidate, unbuilt, untested: wrap `stdin()` in `AsyncRead` that records `Ok(0)`; application learns of EOF before `waiting()` returns.
11. Tree counts: host target only. `-e normal` omits build-only crates (+1) and other targets (+20). Versions floated to latest on 2026-10-20; lockfiles pin. `syn` appears twice (v2, v3); counted twice. Manifest never compiled: build-time feature unification unexercised. `toml`, `yaml-rust2` written `*`. iroh default features assumed. rmcp default-features ambiguity covered by 2x2 table.
12. LOC depends on rustfmt layout: `json!` blocks expand. "Stubs' bodies" = `dispatch` between marker comments. Also holds parse, claim-check lines real server keeps (about 10).
13. Wire examples use default `serde_json` (alphabetical keys). With `preserve_order` bytes differ; content does not.
14. Probe ran rmcp-macros 3.6.0 against rmcp 3.5.1. Macro `src/` identical to 3.5.1; rmcp-side behavior (router, extractor) is 3.5.1 code.
15. Not tested: `ping`, `resources/*`, `prompts/*`, client `notifications/cancelled`, SIGINT, SIGHUP, SIGKILL, parent death, Windows.
16. Spike never ran under rmcp's own client or any real MCP client.
