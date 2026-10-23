# Capabilities required by spec Part 3

Source: spec/blueprint.md lines 399-887 (Part 3, 3.1 to 3.16). Date: 2026-10-15.

Excluded: 3.12 bridge contract (bridge-side steps, Claude Code bridge, pi bridge) and 3.2 repository layout. Acceptance tests in 3.15 add no capability beyond the rows below; each test maps to rows already listed.

## By range

### transport

| # | Capability | Spec section | Exact parameters quoted |
|---|---|---|---|
| 1 | Start an iroh endpoint with the transport key, the configured relay, and local discovery | 3.6 | "an iroh endpoint with the transport key of 3.1, the relay from the network configuration, and local network discovery enabled" |
| 2 | Register one ALPN identifier | 3.6 | "One ALPN identifier, a fixed ASCII build constant that ends in `/1`" |
| 3 | Accept connections from any node id and authorize per request | 3.6 | "Accept a connection from any node id. Authorization is per request, by claim and mac, not per connection." |
| 4 | Serve one request per bidirectional stream | 3.6 | "The client writes one request structure and finishes its send side. The server writes one response structure and finishes. Both sides close the stream." |
| 5 | Open a direct stream to a node id for Message, Probe, or Introduce | 3.6, 3.8 | "Connect to `claim.node`"; "Send Probe to `claim.node`"; "Send Introduce directly to `claim.node`" |
| 6 | Validate a received Message, steps 1 to 4, in order | 3.6 | "1. `v` is 1, else close the stream. 2. The mac verifies, else close the stream without a response. 3. `from.sig` verifies, else respond `accepted: false, reason: \"refused\"`. 4. `from.node` equals the node id of the remote end of the connection, else respond `refused`." |
| 7 | Validate a received Message, steps 5 and 6 | 3.6 | "5. `to` equals the card of this process, and this process has a claim, else respond `refused`. 6. `body` is valid UTF-8 within the limit, else respond `too_large`." |
| 8 | Validate a received Probe and Introduce | 3.6 | "`v` is 1 and the mac verifies, else close the stream. For Introduce, the claim must verify and `claim.node` must equal the remote node id, else close the stream." |
| 9 | Reach a peer through the relay when no direct path exists | 3.15 | "Two machines, no direct path, relay only. Same result as test 1." |

### gossip

| # | Capability | Spec section | Exact parameters quoted |
|---|---|---|---|
| 1 | Subscribe to the topic id with configured bootstrap node ids plus locally discovered node ids | 3.7 | "Subscribe to the topic id with the bootstrap node ids from the configuration, plus node ids found by local discovery." |
| 2 | Subscribe and receive at process start, sending nothing before init | 3.8 | "Subscribe to the topic and receive; send no payload before init." |
| 3 | Publish each gossip payload as one postcard structure | 3.7 | "Every payload is one structure." |
| 4 | Drop a payload whose mac does not verify | 3.7 | "Drop any payload whose mac does not verify." |
| 5 | Drop a Presence or Unreachable whose claim does not verify | 3.7 | "Drop any Presence or Unreachable whose claim does not verify." |
| 6 | Bind claim.node to the card over gossip by claim signature only | 3.7 | "over gossip the claim's signature is the only binding of `claim.node` to the card, and the direct-stream check of 3.6 step 4 is where a node id is a fact." |
| 7 | Never send a Message over gossip | 3.16 | "Do not send a Message over gossip." |
| 8 | Never send a Presence on a timer | 3.16 | "Do not send a Presence on a timer." |

### crypto

| # | Capability | Spec section | Exact parameters quoted |
|---|---|---|---|
| 1 | Derive the node id from the transport key | 3.1 | "The Ed25519 key pair of the iroh endpoint of one process. Its public half is the node id." |
| 2 | Encode a card public key | 3.1 | "32 bytes, Ed25519 public key, encoded as lowercase base32 without padding (52 characters)." |
| 3 | Encode and decode a card secret key in the sidecar | 3.1 | "32 bytes, Ed25519 seed, encoded as 64 lowercase hexadecimal characters and one newline, in the sidecar." |
| 4 | Generate a card key pair | 3.5 | "generate a key pair" |
| 5 | Generate and encode a network secret | 3.1 | "32 random bytes, encoded as lowercase base32 without padding." |
| 6 | Compute a fingerprint for internal use | 3.1 | "The first 4 bytes of a card public key, encoded as lowercase base32 without padding (7 characters). Internal." |
| 7 | Derive keys with BLAKE3 derive_key | 3.4 | "`derive(context, secret)` is BLAKE3 `derive_key` with the context string and the 32 secret bytes as key material." |
| 8 | Derive the topic id | 3.4 | "`derive(\"messaging topic v1\", secret)`" (32 bytes) |
| 9 | Derive the mac key | 3.4 | "`derive(\"messaging mac v1\", secret)`" (32 bytes) |
| 10 | Compute a mac over a wire structure | 3.4 | "encode the structure with its `mac` field set to 32 zero bytes, then compute the keyed hash of those bytes." |
| 11 | Verify a mac by recomputation and constant-time comparison | 3.4 | "Verify by recomputation and constant-time comparison." |
| 12 | Sign and verify a claim with Ed25519 | 3.4 | "Sign the postcard encoding of the claim with `sig` set to 64 zero bytes. Verify the same way." |
| 13 | Treat a claim as valid only when its signature verifies against the card | 3.4 | "A claim is valid when the signature verifies against `card`." |
| 14 | Derive an Ed25519 public key from a 32-byte seed for comparison | 3.5 | "the public key derived from the sidecar equals `public_key`." |
| 15 | Keep fingerprints and keys out of to and from fields | 3.16 | "Do not put fingerprints or keys in the `to` or `from` fields." |

### wire-encoding

| # | Capability | Spec section | Exact parameters quoted |
|---|---|---|---|
| 1 | Encode and decode all wire structures with postcard | 3.1 | "wire encoding  /  postcard." |
| 2 | Start every wire structure with v equal to 1 | 3.1 | "Every wire structure starts with a version field `v` equal to 1." |
| 3 | Encode the Claim struct | 3.4 | "v: 1; card: [u8; 32]; node: [u8; 32]; ts: u64; handle: String; sig: [u8; 64]" |
| 4 | Encode the Message struct | 3.6 | "v: 1; id: [u8; 16]; to: [u8; 32]; from: Claim; attached: bool; sent_at: u64; body: String; mac: [u8; 32]" |
| 5 | Encode the Probe struct | 3.6 | "v: 1; nonce: [u8; 16]; mac: [u8; 32]" |
| 6 | Encode the Introduce struct | 3.6 | "v: 1; claim: Claim; handle: String; public: Vec<u8>; attached: bool; mac: [u8; 32]" |
| 7 | Encode the MessageAck struct | 3.6 | "v: 1; id: [u8; 16]; accepted: bool; attached: bool; reason: Option<String>" |
| 8 | Encode the ProbeAck struct | 3.6 | "v: 1; nonce: [u8; 16]; claim: Option<Claim>; attached: bool; mac: [u8; 32]" |
| 9 | Encode the IntroduceAck struct | 3.6 | "v: 1; claim: Option<Claim>; handle: Option<String>; public: Vec<u8>; attached: bool; mac: [u8; 32]" |
| 10 | Encode the Presence struct | 3.7 | "v: 1; claim: Claim; handle: String; public: Vec<u8>; attached: bool; ts: u64; mac: [u8; 32]" |
| 11 | Encode the Departure struct | 3.7 | "v: 1; card: [u8; 32]; node: [u8; 32]; ts: u64; mac: [u8; 32]" |
| 12 | Encode the Unreachable struct | 3.7 | "v: 1; card: [u8; 32]; node: [u8; 32]; reporter: Claim; ts: u64; mac: [u8; 32]" |
| 13 | Encode the Request sum type | 3.6 | "Request = Message, Probe, Introduce (in that order)" |
| 14 | Encode the Response sum type | 3.6 | "Response = MessageAck, ProbeAck, IntroduceAck (in that order)" |
| 15 | Encode the Payload sum type | 3.7 | "Payload = Presence, Departure, Unreachable (in that order)" |
| 16 | Enforce the Message body limit | 3.6, 3.14 | "`body`: UTF-8, maximum 1,048,576 bytes"; "message body  /  1,048,576 bytes" |
| 17 | Enforce the wire structure size cap | 3.14 | "wire structure, any  /  1,114,112 bytes" |

### local-encoding

| # | Capability | Spec section | Exact parameters quoted |
|---|---|---|---|
| 1 | Write JSON as one object per line, newline-terminated, on the inbox socket and in MCP results | 3.1 | "JSON, one object per line, terminated by a newline. Used on the inbox socket and in MCP results." |
| 2 | Enforce the serialized public mapping size limit | 3.5, 3.7, 3.14 | "The serialized `public` mapping has a maximum size of 4096 bytes. A larger mapping gives `card_invalid`."; "public mapping, serialized  /  4096 bytes" |
| 3 | Carry the card public mapping in presence verbatim and keep the rest local | 3.5 | "Its content travels in presence, verbatim. Everything else in the card stays local." |
| 4 | Carry the public mapping in Introduce and Presence as JSON bytes | 3.6, 3.7 | "the serialized public mapping of the card, as JSON bytes" |
| 5 | Read the inbox client first line | 3.11 | "Client first line: `{\"since\": N}`. `N` is the last `seq` the client saw, or 0." |
| 6 | Write an inbox frame as a JSON object | 3.11 | "{\"seq\": 12, \"id\": \"<message id>\", \"from\": \"<effective handle>\", \"body\": \"<text>\", \"sent_at\": 1700000000000, \"received_at\": 1700000000000}" |
| 7 | Write the MCP success envelope | 3.10 | "{ \"ok\": true, \"you\": { \"card\": \"<card name>\", \"handle\": \"<effective handle>\", \"status\": \"claimed\" }, \"result\": { } }" |
| 8 | Write the MCP error envelope | 3.10 | "{ \"ok\": false, \"you\": { \"card\": null, \"handle\": null, \"status\": \"unclaimed\" }, \"error\": { \"code\": \"not_claimed\", \"message\": \"Claim a card with init first.\" } }" |
| 9 | Emit the status field values | 3.10 | "`status` is one of `unclaimed`, `claimed`, `displaced`." |

### files-and-paths

| # | Capability | Spec section | Exact parameters quoted |
|---|---|---|---|
| 1 | Resolve the config base directory | 3.3 | "`$XDG_CONFIG_HOME`, else `$HOME/.config`, then one subdirectory for this program." |
| 2 | Resolve the data base directory | 3.3 | "`$XDG_DATA_HOME`, else `$HOME/.local/share`, then one subdirectory for this program." |
| 3 | Resolve and create the runtime base directory | 3.3 | "`$XDG_RUNTIME_DIR`, else `$TMPDIR` then `/tmp`, then one subdirectory for this program with the user id in its name, permissions 0700." |
| 4 | Write the network configuration file | 3.3 | "`<config>/networks/<network>.toml`. Network configuration. Permissions 0600." |
| 5 | Parse the network configuration keys | 3.3 | "`secret = \"<network secret>\"`; `relay = \"https://relay.example\"`; `bootstrap = [\"<node id>\", \"<node id>\"]`" |
| 6 | Locate the card file | 3.3 | "`<data>/cards/<network>/<card>.yaml`. A card. The file stem is the card name." |
| 7 | Locate and write the sidecar file | 3.3 | "`<data>/cards/<network>/<card>.key`. The sidecar of that card. Permissions 0600." |
| 8 | Locate the infrastructure key file | 3.3 | "`<data>/infra/<network>.key`. The transport key of the bootstrap member. Permissions 0600. Only on the infrastructure host." |
| 9 | Locate the inbox socket file | 3.3 | "`<runtime>/<pid>.sock`. The inbox socket of the process with that process id." |
| 10 | Override the cards directory by flag and environment variable | 3.3 | "A command-line flag and an environment variable override the cards directory. Nothing else is configurable by path." |
| 11 | Parse card keys and ignore all others | 3.5 | "The binary reads these keys and ignores every other key." (`handle`, `public_key`, `network`, `public`) |
| 12 | Default the card network to the process network | 3.5 | "`network`  /  no  /  A network name. If absent, the network given to the process is used." |
| 13 | Validate the card file at claim | 3.5, 3.13 table | "the file parses, `handle` matches the handle grammar, `public_key` decodes to 32 bytes"; error `card_invalid`: "The card file did not pass validation." |
| 14 | Validate the sidecar at claim | 3.5, 3.10 table | "the sidecar exists, the sidecar has permissions 0600, the sidecar decodes to 32 bytes"; error `sidecar_invalid`: "absent, unreadable, has wrong permissions, or does not match the card." |
| 15 | Create a card and sidecar without overwriting | 3.5 | "generate a key pair, write the sidecar with permissions 0600, write a card with `handle` and `public_key` only. Do not overwrite an existent card or sidecar." |
| 16 | Remove a stale socket file at start | 3.3 | "Remove a stale socket file at start if no process with that id exists." |
| 17 | Remove the own socket file at exit | 3.3 | "Remove your own socket file at exit." |
| 18 | Read the registry of cards for a network | 3.13 | "list cards  /  network name  /  Read the registry." |
| 19 | Claim the reserved card `operator`, creating it if absent | 3.13 | "Claim the reserved card `operator` from the registry, create it if absent, run the send algorithm, release." |
| 20 | Keep secrets, tokens, and sidecar paths out of the card | 3.16 | "Do not put a secret, a token, or a sidecar path in a card." |
| 21 | Write no file that announces the inbox socket | 3.16 | "Do not write a file that announces the inbox socket." |
| 22 | Add no lock file, pid file, or registry index | 3.16 | "Do not add a lock file, a pid file, or a registry index." |

### local-io

| # | Capability | Spec section | Exact parameters quoted |
|---|---|---|---|
| 1 | Listen on the inbox socket from process start, before init, with permissions 0600 | 3.11 | "Permissions 0600. The listener starts with the process, before init, so a bridge can connect early." |
| 2 | Send buffered frames above N, then live frames, to each client | 3.11 | "The server sends every buffered frame with `seq` greater than `N`, then live frames as they arrive." |
| 3 | Write only message frames on the inbox socket | 3.11, 3.16 | "The server writes nothing else on this socket."; "Do not push anything but message frames on the inbox socket." |
| 4 | Keep the inbox socket open in every state | 3.9 | "The inbox socket stays open in every state." |
| 5 | Begin stopping on stdin close or a signal | 3.9 | "any state --stdin closed or signal--> Stopping --Departure sent--> exit" |
| 6 | Test whether a process with a given id exists | 3.3 | "Remove a stale socket file at start if no process with that id exists." |
| 7 | Read no harness environment variable | 3.16 | "Do not read any harness environment variable in the binary." |
| 8 | Impose no rate limit in the binary | 3.16 | "Do not rate limit in the binary." |

### mcp

| # | Capability | Spec section | Exact parameters quoted |
|---|---|---|---|
| 1 | Serve MCP over standard input and output with a build-constant server name | 3.10 | "Transport: standard input and output. The server name is a build constant." |
| 2 | Fix the tool list at start, with six tools, never changed | 3.10, 3.16 | "The tool list is fixed at start and never changes."; "Do not add, remove, or hide a tool at runtime." |
| 3 | Implement `list_cards` | 3.10 | "`cards: [{ name, handle, held: bool }]`" (input: none) |
| 4 | Set `held` from the peer table and wait the observer wait on first call | 3.10 | "`held` is true when the peer table has an Online entry for that card public key. On the first call after start, wait the observer wait of 3.14 before answering." |
| 5 | Implement `init` input and result | 3.10 | "`card: string`, `description?: string`" in; "`handle: string`, `inbox_path: string`" out |
| 6 | Put the description under `public.description` for this session only | 3.10 | "The description, if given, is put under `public.description` in the presence of this session only." |
| 7 | Return `bad_request` for init in state Claimed | 3.9 | "Init in state Claimed returns `bad_request`. A process claims one card for its life." |
| 8 | Implement `peers`, excluding self | 3.10 | "`peers: [{ handle, status, attached, public: object, last_seen }]`. Excludes yourself." |
| 9 | Implement `send` input and result | 3.10 | "`to: string`, `body: string`" in; "`id: string`, `delivered: true`, `attached: bool`" out |
| 10 | Implement `broadcast` with concurrent sends | 3.10 | "`to: string[]`, `body: string`"; "`results: [{ to, id?, delivered: bool, attached?: bool, error?: { code, message } }]`. Sends run concurrently." |
| 11 | Implement `inbox` without removing frames | 3.10 | "`since?: integer`" in; "`messages: [{ seq, id, from, body, sent_at, received_at }]`, `last_seq: integer`. ... Does not remove frames." |
| 12 | Send step 1: refuse when not Claimed | 3.10 | "If the state is not Claimed, return `not_claimed`." |
| 13 | Send step 2: resolve the handle | 3.10 | "None: `unknown_handle`. More than one: `ambiguous_handle`, with the candidates in the message." |
| 14 | Send step 3: connect with a 5000 ms timeout | 3.10 | "Connect failure or a 5000 millisecond timeout: `unreachable`." |
| 15 | Send step 4: write Message and await MessageAck within 10000 ms | 3.10 | "Write a Message. Wait for MessageAck with a 10000 millisecond timeout. Timeout: `timeout`." |
| 16 | Send steps 5 and 6: return the reason code or delivered with attached | 3.10 | "`accepted` false: return the error with the code in `reason`."; "Return `delivered: true` with `attached` from the ack." |
| 17 | Use the closed error code list | 3.10 | "`not_claimed`, `card_not_found`, `card_held`, `card_invalid`, `sidecar_invalid`, `unknown_handle`, `ambiguous_handle`, `unreachable`, `timeout`, `refused`, `too_large`, `bad_request`. This list is closed." (messages per the 3.10 table) |
| 18 | Return `not_claimed` in Unclaimed and Displaced, except list_cards and init | 3.9 | "In Unclaimed and Displaced, every tool except list_cards and init returns `not_claimed`. In Displaced, the error message names the node that holds the card." |
| 19 | Include `you` in every tool response | 3.10, 3.15 | "Every tool response contains `you` with the current status, before and after init, and after displacement." |
| 20 | Return `bad_request` on input that breaks the tool schema | 3.10 | "The input did not match the tool schema." (any tool) |
| 21 | Use the six tool description strings exactly | 3.10 | "`list_cards`: List the identity cards on this machine and whether each is in use." ... "`inbox`: Read messages that arrived for you. Use `since` to read only newer ones." (six strings, exact) |
| 22 | Refuse no send because of a peer's recorded status | 3.16 | "Do not refuse a send because of the recorded status of a peer." |

### cli

| # | Capability | Spec section | Exact parameters quoted |
|---|---|---|---|
| 1 | `create network`: generate secret, write config, print invitation | 3.13 | "Generate a secret. Write the configuration file."; output "An invitation string that contains the name, the secret, the relay, and the bootstrap node ids." |
| 2 | `join network`: write config from invitation, print nothing | 3.13 | "Write the configuration file from the invitation." Output: "Nothing." |
| 3 | `create card`: take network, card name, handle; print card public key | 3.13 | "network name, card name, handle  /  See 3.5.  /  The card public key." |
| 4 | `list cards`: print one line per card | 3.13 | "One line per card: name, handle, public key." |
| 5 | `show peers`: observer node, wait 3000 ms, print peers, exit | 3.13 | "Start an observer node without a card. Subscribe. Wait 3000 milliseconds for Presence and Introduce. Exit." Output: "effective handle, status, attached, public fields" |
| 6 | `tail`: observer node, print each gossip payload until interrupted | 3.13 | "Same as show peers, but print each gossip payload as it arrives until interrupted." |
| 7 | `test send`: claim `operator`, send, release; print send result | 3.13 | "network name, handle, body"; "Claim the reserved card `operator` ... run the send algorithm, release." |
| 8 | `doctor`: run each check, print one line per check | 3.13 | "configuration file exists with permissions 0600, each card validates, each sidecar matches, the relay answers" ; "One line per check, pass or fail." |
| 9 | `doctor`: observer sees at least one peer within 3000 ms | 3.13 | "an observer node sees at least one peer within 3000 milliseconds." |
| 10 | Accept network name and bind address for `run infrastructure` | 3.13 | "run infrastructure  /  network name, bind address  /  See 3.14.  /  The ticket." |
| 11 | Exit with codes 0, 1, 2, 3 | 3.13 | "Exit codes: 0 success, 1 failure, 2 usage error, 3 doctor found at least one failed check." |

### time-and-ids

| # | Capability | Spec section | Exact parameters quoted |
|---|---|---|---|
| 1 | Generate message ids as UUID version 7 | 3.1 | "A UUID version 7. 16 bytes on the wire. The canonical string form in JSON." |
| 2 | Stamp times as unsigned 64-bit milliseconds since the Unix epoch | 3.1 | "Milliseconds since the Unix epoch, as an unsigned 64-bit integer." (sent_at, received_at, claim ts, ts fields, last_seen) |
| 3 | Connect with a 5000 ms timeout | 3.14 | "connect timeout  /  5000 milliseconds" |
| 4 | Await MessageAck for 10000 ms | 3.14 | "ack timeout  /  10000 milliseconds" |
| 5 | Treat a probe failure after 3000 ms | 3.8, 3.14 | "probe timeout  /  3000 milliseconds" |
| 6 | Wait 3000 ms as the observer wait | 3.9, 3.13, 3.14 | "observer wait  /  3000 milliseconds"; init waits it before assigning its handle |
| 7 | Wait at most 1000 ms for Departure at exit | 3.8, 3.14 | "Send Departure. Wait at most 1000 milliseconds. Exit."; "departure wait at exit  /  1000 milliseconds" |
| 8 | Validate the handle grammar | 3.1 | "A string that matches `^[a-z][a-z0-9-]{0,31}$`." |
| 9 | Form effective handles as handle or handle-n | 3.1 | "A handle, or a handle followed by `-` and a decimal integer of at least 2." |
| 10 | Reuse the message id on retry | 3.15 | "A retried send with the same message id after a lost ack produces one inbox frame, not two." |

### state

| # | Capability | Spec section | Exact parameters quoted |
|---|---|---|---|
| 1 | Keep the peer table keyed by card, one entry per card | 3.8 | "Keyed by `card`. One entry per card." with fields card, claim, effective_handle, public, attached, status, last_seen |
| 2 | Use the status values Online, Unreachable, Gone | 3.8 | "status: Online, Unreachable, Gone" |
| 3 | Start the endpoint, direct listener, and inbox listener at process start | 3.8 | "Start the endpoint, the direct protocol listener, and the inbox listener." |
| 4 | Send Presence on init success | 3.8 | "init succeeds  /  Send Presence." |
| 5 | Presence for an unknown card: insert Online, send Introduce, assign handle | 3.8 | "Insert the peer as Online. Send Introduce directly to `claim.node`. Apply handle assignment." |
| 6 | Presence from the held node: update and set Online | 3.8 | "Update `public`, `attached`, and `last_seen`. Set Online." |
| 7 | Presence from another node earlier in tie-break order: probe, then replace on match | 3.8 | "Send Probe to `claim.node`. On a ProbeAck carrying that claim, replace the held claim, set Online, apply handle assignment. On failure, ignore." |
| 8 | Presence from another node later in tie-break order: probe the held node, replace on failure | 3.8 | "Send Probe to the node of the held claim. On ProbeAck, ignore. On failure, replace the held claim, set Online, apply handle assignment." |
| 9 | Presence for own card from another node: apply tie-break and answer | 3.8 | "Apply the tie-break of 3.4. If you lose, send Probe to that node: on a ProbeAck carrying that claim, enter Displaced (3.9); on failure, send Presence. If you win, send Presence." |
| 10 | Treat Introduce and IntroduceAck as Presence | 3.8 | "Same as Presence received." |
| 11 | Treat a validated Message's from as a Presence from from.node | 3.8 | "Treat `from` as a Presence received from `from.node`, by the rows above." |
| 12 | Order claims by tie-break | 3.4 | "the claim with the lower `ts` wins. If `ts` is equal, the claim with the lower `node`, compared as 32 bytes, wins." |
| 13 | Never replace a held claim without a probe | 3.4 | "The tie-break orders claims; it never replaces a held claim by itself. Before a node replaces the claim it holds for a card, it probes." |
| 14 | On Departure, set the peer Gone | 3.8 | "Departure received  /  Set the peer Gone." |
| 15 | Remove a Gone entry after its retention | 3.8, 3.14 | "Keep the entry for 3600 seconds, then remove it."; "gone entry retention  /  3600 seconds" |
| 16 | On Unreachable about a peer, set Unreachable unless contact is more recent | 3.8 | "Set the peer Unreachable, unless a direct contact with that peer is more recent than `ts`." |
| 17 | On Unreachable about yourself, send Presence | 3.8 | "Unreachable received, about yourself  /  Send Presence." |
| 18 | On gossip neighbor up, send Introduce unless the node is an Online entry | 3.8 | "gossip reports a neighbor up  /  Send Introduce to that node, unless it is the node of an Online entry." |
| 19 | On gossip neighbor down, probe and mark Unreachable on failure | 3.8 | "Send Probe to that node. On ProbeAck, keep Online. On failure, set Unreachable and send Unreachable." |
| 20 | On send failure at connect or ack, mark Unreachable and send Unreachable | 3.8 | "send to a peer fails at connect or at ack  /  Set the peer Unreachable. Send Unreachable." |
| 21 | On any successful direct contact, mark Online | 3.8 | "any direct contact with a peer succeeds  /  Set the peer Online." |
| 22 | On network change or relay reconnect, send Presence | 3.8 | "endpoint reports a network change or a relay reconnect  /  Send Presence." |
| 23 | On inbox client connect or last disconnect, set attached and send Presence | 3.8 | "a client connects to the inbox socket, or the last client disconnects  /  Set your attached flag. Send Presence." |
| 24 | On process stop, send Departure, wait, exit | 3.8 | "process stops  /  Send Departure. Wait at most 1000 milliseconds. Exit." |
| 25 | Assign a new entry its effective handle | 3.8 | "the effective handle of a new entry is the handle it announces, `h`, if no entry seen earlier at this node, yourself included, holds effective handle `h`; otherwise it is `h-n` for the smallest integer `n` of at least 2 such that `h-n` is free at this node." |
| 26 | Keep an entry's effective handle when its claim is replaced | 3.8 | "An entry keeps its effective handle for as long as it exists; a later entry never displaces it, and a replaced claim keeps the entry's handle." |
| 27 | Set own effective handle at init by the same rule, then never change it | 3.8 | "Your own effective handle at init follows the same rule against the table after the observer wait, and never changes afterward." |
| 28 | Run the session state machine | 3.9 | "Unclaimed --init ok--> Claimed --lost tie-break to a live claimant--> Displaced --init ok--> Claimed; any state --stdin closed or signal--> Stopping --Departure sent--> exit" |
| 29 | Refuse Messages in Displaced with `refused` | 3.9 | "Messages that arrive in Displaced are refused with `refused`, because step 5 of 3.6 fails." |
| 30 | Keep a duplicate set of the last 10,000 message ids | 3.6, 3.14 | "If `id` is in the duplicate set, respond `accepted: true` and do not enqueue."; "duplicate set  /  last 10,000 message ids" |
| 31 | Insert an accepted id into the duplicate set after enqueue | 3.6 | "Enqueue the message in the inbox (3.11). Insert `id` into the duplicate set. Respond `accepted: true`." |
| 32 | Buffer the last 1000 inbox frames | 3.11, 3.14 | "The buffer holds the last 1000 frames."; "inbox buffer  /  1000 frames" |
| 33 | Assign seq from 1, plus 1 per message, for the process life | 3.11 | "`seq` starts at 1 and increases by 1 per message for the life of the process." |
| 34 | Set attached true while at least one client is connected | 3.11 | "Attached is true while at least one client is connected." |
| 35 | Persist no inbox | 3.16 | "Do not persist the inbox." |
| 36 | Keep no message for an absent member past process life | 3.16 | "Do not keep a message for an absent member, on disk or in memory, past the life of the process." |

### infra

| # | Capability | Spec section | Exact parameters quoted |
|---|---|---|---|
| 1 | Run an embedded relay bound to the given address | 3.14 | "runs, in one process, an embedded relay bound to the given address" |
| 2 | Run one iroh endpoint with the infrastructure key, creating the key on first run | 3.14, 3.3 | "one iroh endpoint with the key from `<data>/infra/<network>.key`. ... Create the key file on first run." |
| 3 | Subscribe to the topic id and send no payload | 3.14 | "The endpoint subscribes to the topic id, sends no payload" |
| 4 | Answer Probe and Introduce with claim None | 3.14 | "answers Probe and Introduce with `claim: None`" |
| 5 | Accept no Message and refuse each with `refused` | 3.14 | "It accepts no Message, and refuses one with `refused`." |
| 6 | Print a ticket with node id and relay URL at start | 3.14 | "It prints a ticket with its node id and relay URL at start." |
| 7 | Hold no card and forward no message on the bootstrap member | 3.16 | "Do not let the bootstrap member hold a card or forward a message." |

## Counts

| Range | Capabilities |
|---|---|
| transport | 9 |
| gossip | 8 |
| crypto | 15 |
| wire-encoding | 17 |
| local-encoding | 9 |
| files-and-paths | 22 |
| local-io | 8 |
| mcp | 22 |
| cli | 11 |
| time-and-ids | 10 |
| state | 36 |
| infra | 7 |
| Total | 174 |

## Ambiguities

The spec leaves these parameters unfixed. Each needs a decision before a library choice is made.

- **Base32 alphabet.** "Lowercase base32 without padding" does not name the alphabet. Node ids appear in the network configuration and the ticket, so their text form is also unfixed.
- **Sidecar hex.** The spec does not say whether uppercase hex is accepted on read, or whether the trailing newline is required.
- **Field widths.** The width and encoding of `v` and of the sum discriminants are not stated. The sum order is given only in the text.
- **Canonical JSON.** The byte form of the public mapping (key order, whitespace, escaping) is not fixed. The mac covers those bytes, so two implementations can disagree.
- **Public size measure.** The 4096-byte limit does not say whether it counts the compact or pretty JSON bytes.
- **YAML to JSON.** Conversion of the card `public` mapping from YAML to JSON is not defined, including non-string keys, dates, and duplicate keys.
- **Probe nonce.** The source of the 16-byte nonce is not named. The spec does not say whether ProbeAck's nonce must match the one sent.
- **Probe failure.** "Failure" is not defined. The spec does not say whether a bad mac, a nonce mismatch, or a connect error counts.
- **Local discovery.** The discovery mechanism is not named.
- **Relay implementation.** The relay protocol and the embedded relay implementation are not named.
- **Oversize stream.** The 1,114,112-byte cap does not say what happens when a stream exceeds it.
- **Stopping signals.** The signals that trigger Stopping are not listed.
- **Stale socket check.** The method for "no process with that id exists" is not specified. PID reuse is not addressed.
- **Build constants.** The program subdirectory name, the MCP server name, and the ALPN string are build constants, but no values are given. The source of the user id is not named.
- **Override names.** The command-line flag name and the environment variable name for the cards directory are not given.
- **Directory modes.** Permissions for the config and data directories are not stated, only for files.
- **Name grammars.** No grammar exists for network names or card names. The card name is a file stem in a path, so it needs a constraint.
- **Effective handle length.** The `-n` suffix can push an effective handle past 32 characters. The maximum is not stated, and no upper bound for `n` is given.
- **Gone entries and suffixes.** The spec does not say whether a Gone entry still "holds" a handle when a suffix is chosen.
- **Duplicate-set eviction.** The spec does not say whether the duplicate set evicts first in, first out or least recently used.
- **Replay beyond buffer.** The spec does not say what happens when a client's `since` is older than the 1000-frame buffer.
- **Concurrent inbox clients.** The spec does not say whether each client receives the full replay.
- **Single-claim race.** Init must refuse a second local claim, but the spec forbids a lock file and a registry index. Acceptance test 6 needs two processes within 100 ms to yield exactly one Claimed. The mechanism is not specified.
- **Unreachable and Departure channel.** The spec says "send Unreachable" and "send Departure" without naming the channel. The gossip topic is implied.
- **Transport key persistence.** The spec does not say whether an ordinary process generates its transport key per run or stores it. Observer nodes without a card are not addressed.
- **Output formats.** The invitation string, the ticket, and the per-line formats of list cards, show peers, and doctor are not specified.
- **Broadcast concurrency.** "Sends run concurrently" does not bound the number of concurrent sends.
- **Timers.** The mechanism for the Gone retention timer and the observer wait is not specified.
