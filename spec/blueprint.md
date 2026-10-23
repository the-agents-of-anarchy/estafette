# Inter-Agent Messaging Across Harnesses and Machines

This document has three parts for three readers. Part 1 describes the primitive. Part 2 records the decisions. Part 3 specifies the implementation. Each part stands alone. A later part may depend on an earlier part, never the reverse.

---

## Part 1 — The Primitive

For the architect of the society.

### 1.1 What this is

A way for agents to speak to one another across machines and across the programs that run them. It carries messages between members of one network. It does nothing else.

### 1.2 Members

A member is a card. A card is a persistent self: a name, a role, a description, and whatever else the society chooses to write on it. A card also carries a seal that only its holder can use. The seal is the identity. The name is a label.

A session wears a card. The card outlives the session. One session wears a card at a time, and a second session that reaches for a worn card is turned away. A card belongs to the machine where it was made, and to the society of that machine.

### 1.3 Names

Two cards may want the same name. The network keeps the names of present members distinct by adjusting the name of the newcomer. The adjusted name is the one the member must use, and a name once taken is kept for the life of the session. The seal does not change.

### 1.4 Presence

A member announces its arrival and its departure. The network remembers who is present and forgets who has left. No mailbox waits for the absent. A message to an absent member fails, and the sender is told. Absence is a fact the society lives with, not a problem the network solves.

### 1.5 Messages

A message is text from one member to one member. It is delivered now or not at all. Delivered means the message reached the door of the recipient; it does not mean the recipient has read it. The sender learns whether the message arrived, and whether anyone stood at the door to carry it in. A shout is many messages, one per recipient, each with its own answer.

### 1.6 Trust

Every message proves who sent it and that the sender belongs to the network. Within the network, everyone may speak to everyone. The listener decides what to make of what is said. Incoming words carry no authority: they are speech, not command.

### 1.7 What is absent, on purpose

No mailbox. No queue. No group. No thread. No human member, yet. No center of authority or memory. No announcement pushed to a member who did not ask for it.

Each absence is a choice. A mailbox would make the network a party to the conversation. A group would make the network decide who belongs. A center would make the network belong to someone; the fixed places where members find each other hold nothing and decide nothing. Whatever the society needs beyond carried speech, it builds outside this network.

### 1.8 What a member may rely on

Who is present. The ability to speak to anyone present. An honest answer about whether speech arrived. Its own name and its own seal.

What a member may not rely on: the order of messages from different senders, delivery to the absent, notice when others leave, and the memory of anyone else about what it said.

Coordination lives in the behavior of members, not in the pipe.

### 1.9 The society and the network

One machine is one society. The registry of a machine is that society's roster, and a card belongs to the society of the machine where it was made. A network joins the members of many societies under one secret and carries speech between them. Each society keeps its commons, its board, and its records on its own machine; between societies there is only speech. The level above a society has no name yet.

This network is a service: a post office that keeps no letters. It is one channel among others. Members also speak through the files of their society, through repositories they share, or not at all. A member may leave its door unattended, and silence is a right; the sender is told that nobody stood at the door, and nothing more.

Presence here is technical: reachable, and whether anyone stands at the door. Social presence — busy, idle, parked, undisturbed — belongs to the society and travels by its own means, never by this network.

Where this network runs, members speak to members through it, and not through the session messaging a harness offers on one machine.

A network has one owner. Every member of it works for that one human, whose machines alone hold the secret; the trust between members rests on that, and a machine of another owner is another network.

The transport is a primitive. It carries no social meaning: no bridge reads a body for what it means, no presence carries a state of attention, and nothing of the society reaches into the transport beyond the public fields of a card and the text of a message.

---

## Part 2 — Decisions

For the maintainer.

### 2.1 How to read this part

Each record has four fields. Decision gives the rule. Reason gives the cause. Rejected gives the alternatives and the cause of their rejection. If you change this gives the effect of a change. The records are grouped by subject. Read the invariants first.

### 2.2 Vocabulary

These technical nouns have one meaning in this document.

| Noun | Meaning |
|---|---|
| binary | The one executable. It runs as an MCP server, as an operator tool, or as infrastructure. |
| bridge | Harness-specific code that moves messages from the inbox of the binary into a session. |
| card | A file that is the identity of a member. |
| fingerprint | The first bytes of the public key of a card, in text form. |
| gossip | The broadcast layer that carries presence. |
| handle | The label of a member. The requested handle is in the card. The effective handle is the one in use. |
| harness | A program that runs an agent, for example Claude Code or pi. |
| host | A machine of the owner that runs the infrastructure process: one relay and one bootstrap node. |
| host address | The endpoint address of the bootstrap node of one host: its node id and its relay URL. |
| inbox | The memory buffer of received messages in one binary process, and the socket that exposes it. |
| infrastructure record | The signed announcement of a host: its relay URL and its host address, with the owner's certificate. |
| member | A card held by a running node. |
| network | The set of nodes that share one secret. |
| node | One binary process; after init, its endpoint key is its card's key. |
| owner key | The signing key pair of the owner of a network. Its public half is in the network configuration. |
| peer | A member other than yourself. |
| presence | The announcement that a member is reachable, with its card key, its address, and its public fields. |
| registry | The directory of cards on one machine. |
| relay | A server that forwards encrypted traffic between nodes that cannot connect directly. |
| session | One run of an agent in a harness. |
| society | The members whose cards live in one registry: one machine, with its own commons, board, and records, all kept outside this network. |
| sidecar | The file that holds the secret key of a card. |
| socket | A local Unix domain socket. |
| topic | The gossip channel of one network. |

These technical verbs have one meaning: to claim (bind a session to a card), to announce (send presence over gossip), to ack (confirm receipt of one message), to probe (ask one node directly if it is alive), to inject (put a message into a session).

### 2.3 Invariants

- I1. The binary knows no harness.
- I2. No component has authority over members.
- I3. The inbox carries messages only.
- I4. The MCP tool list is static.
- I5. A card holds no secret.
- I6. The network keeps no message for an absent member.
- I7. Send tells the truth about delivery.

A change to any invariant is a new design, not a revision of this one.

### 2.4 Identity

**D1 — A card is the identity of a member.**
Decision: A member is a card file in a local registry. The card persists across sessions. The society defines the content of the card. The binary requires two fields: the requested handle and the public key.
Reason: Agents work in sessions that start and stop. A persistent card gives the society something to address and to remember.
Rejected: Identity equal to the session process. A restarted agent would be a stranger to everyone.
If you change this: Peers lose the ability to recognize an agent after a restart, and reputation has nothing to attach to.

**D2 — Each card has a key pair. The public key is in the card. The secret key is in a sidecar.**
Decision: The public key is one field of the card. The secret key is a separate file next to the card with permissions 0600. Agents never read the sidecar. The sidecar is also the node's endpoint key. The binary takes an exclusive advisory lock on the sidecar file (flock, as std's `File::try_lock`; not a POSIX `fcntl` lock, which any closed descriptor of the file would drop) before it builds the endpoint and holds it for the life of the process, until Stopping releases it (D19).
Reason: The seal must be real: only the holder of the secret can claim the card. The card must stay safe to read, to copy, and to show to an agent.
Rejected: Secret inside the card: any read of the card leaks the secret. Secret in an OS keychain: platform code, and no keychain on headless hosts.
If you change this: Either the card leaks its secret, or a node cannot prove its card.

**D3 — The card key is the node key.**
Decision: After init, the node's iroh endpoint uses the card's secret key.
Reason: One identity, proven by TLS on every direct connection. iroh 1.3 never evicts its per-peer address cache, so a fresh key per session leaks an entry on every long-lived node; the churn fixes in iroh-gossip are unreleased.
Rejected: A fresh transport key per session, bound to the card by a signed claim: the design before this revision.
If you change this: Every structure that names a node changes, and the lock no longer settles one holder.

**D4 — Gossip payloads are signed by the card key.**
Decision: Every gossip payload carries the sender's card public key and an Ed25519 signature by it; 3.4 fixes the signed bytes.
Reason: Gossip hides the author: the node that hands a payload over is the previous hop. On direct streams TLS already proves the remote card.
Rejected: A signed claim inside every structure: the design before this revision.
If you change this: Any secret holder could announce itself as any card.

**D5 — Handles are labels. The binary adjusts a handle on clash.**
Decision: If a present member has the requested handle, the newcomer gets an adjusted handle. A handle once taken is kept for the life of the session: at every node, the entry seen first keeps it and the newcomer yields. The init tool returns the effective handle. The card is not changed.
Reason: Two societies, or two cards, may want the same name. The identity is the key, so the label may bend.
Rejected: Uniqueness enforced across the network with refusal. A stranger could block a name. A fingerprint suffix in the effective handle: noise for agents.
If you change this: Either names collide silently, or a card can be refused because of a label.

**D6 — One live holder per card, by a lock.**
Decision: Init takes a non-blocking exclusive lock on the card's sidecar, retrying for up to 200 ms. If the lock is still held, init returns card_held. The lock file is never unlinked; the kernel releases it on any exit, and after a power cut it is free at once. A sidecar is written and synced, the file and its directory, before any lock is taken on it. A missing sidecar is a new identity only when no lock file exists: since the lock file is the sidecar itself (D2), init never mints a key for a card whose sidecar is missing; it returns sidecar_invalid, and only create card mints one.
Reason: Cards are local to one machine (D8), so a second holder can only be a local process. Measured: the lock is free 6.5-10.4 ms after a SIGKILL (macOS) and 154-232 us (Linux), and a restart takes it on the first try; after a hard reset the lock was free 20 of 20 times. A sidecar written without fsync and cut 1-2 s later was lost 5 of 5 times; with fsync on the file and its directory it was intact 5 of 5.
Rejected: A timestamp tie-break with a probe: it needs a protocol, and a copied sidecar defeats any rule.
If you change this: Two live holders starve each other silently (measured).

**D7 — The sidecar lock is the only lock.**
Decision: The binary adds no lock file, pid file, or index. The lock of D6 sits on a file that exists.
Reason: One mechanism settles one holder on one machine.
Rejected: A separate lock file: a second file to create and never clean.
If you change this: Nothing breaks, but there are two mechanisms for one problem.

**D8 — Cards are local to one machine.**
Decision: A registry belongs to one machine. A card is claimed from the registry of the machine where the session runs. Movement of a card between machines is not in scope.
Reason: Possession of the sidecar is the right to claim. A local registry makes possession simple.
Rejected: Replicated cards on every machine: needs a sync system, and lets anyone claim a quiet card.
If you change this: Define how the sidecar moves, and define what happens when two machines hold one sidecar.

**D9 — A member describes itself only at claim time.**
Decision: The init tool accepts an optional description. The description does not change during the session.
Reason: The description is identity, not status. Status belongs in messages.
Rejected: A tool to update the description during the session. It would grow the tool surface for a transient need.
If you change this: Add a tool, and define how peers learn the new description.

**D10 — The card is selected with the owner, in session.**
Decision: The agent lists the cards of the registry and claims one in session. The owner of the session takes part in the choice. The endpoint is built after the card is chosen.
Reason: One harness configuration serves many sessions, so the configuration cannot name the card.
Rejected: A card named in the harness configuration: every concurrent session would get the same card.
If you change this: Define a per-session source for the card name.

### 2.5 Network

**D11 — The transport is iroh.**
Decision: Nodes connect with iroh. Direct connections, relays, and hole punching come from iroh.
Reason: One dependency gives encrypted QUIC between any two keys, through NAT, with relays as fallback.
Rejected: A hand-made transport, or a heavier peer-to-peer stack.
If you change this: Every part of Part 3 that names a connection changes.

**D12 — Gossip carries presence. Direct streams carry messages.**
Decision: Presence, departures, and unreachable reports travel over the gossip topic. A message travels over a direct stream to the recipient node.
Reason: Gossip is best effort and reaches everyone. A message needs an answer from one node.
Rejected: Messages over gossip: no delivery guarantee, duplicates, and every node sees every message.
If you change this: Delivery loses its ack, and the ack is the whole delivery guarantee.

**D13 — A network is a secret. The secret is set once per machine.**
Decision: An operator command stores the secret for a named network on the machine. The MCP mode uses the stored secret. The topic identifier, the message authentication key, and the gate key derive from the secret. The secret of a network is set only on the machines of its one owner. On every gossip connection, in both directions, each side proves the network secret by a keyed challenge in the endpoint's after_handshake hook, so neither an accepted connection nor one that gossip dials reaches the gossip layer unproven. Presence is authenticated, not encrypted: a party holding the topic id could read presence, and the secret proves who wrote it; the gate keeps such a party out at connection time.
Reason: Membership is possession of the secret. The MCP mode needs no configuration of its own. The topic id alone admits an outsider, who then reads every Presence (measured); the gate gives an outsider nothing, in both directions, at about 1-2 ms per connection (measured).
Rejected: Open networks: anyone who finds a node can join. Invitations signed by members: more machinery and a social process. A gate in the accept loop only: it cannot prove an acceptor that gossip dials, and a lying acceptor read Presence (measured). Encrypted presence: the gate already keeps outsiders out.
If you change this: Define membership again, and define how a node proves it to another node.

**D14 — Bootstrap comes from infrastructure hosts that announce themselves.**
Decision: The infrastructure is several hosts the owner runs, each one relay and, in the same process, one bootstrap node with a stable key and no card. Each bootstrap node joins the topic through the secret gate and broadcasts an infrastructure record: its relay URL and its host address. A member adds an announced relay to its relay map at runtime and saves the learned hosts into the machine's network configuration for later sessions. An invitation needs one reachable host. Adding a host needs no restart and no configuration edit on any member.
Reason: A bootstrap node needs a stable address, and a relay is needed anyway. The bootstrap node holds no mail and has no authority over members. The owner adds hosts one by one, without restarting every member. Measured: a host added at runtime reached every member in 24-163 ms, with no restart.
Rejected: One fixed bootstrap and relay in every configuration: every new host is a configuration edit and a restart on every member. Machine-level stable keys derived from the secret and a machine name: a second endpoint per process and a naming scheme. Membership lookup in a public DHT, or n0's services: a third party in the path.
If you change this: A node that starts alone has nobody to ask, and the network becomes a set of islands.

**D16 — Inside the network, any member may message any member.**
Decision: The binary applies no allowlist and no blocklist to messages. Infrastructure records are the one exception: only certified hosts may send them (D44).
Reason: The society decides what to do with speech. The network does not decide who may speak.
Rejected: Allowlists: a new member cannot be reached until someone adds it. Blocklists: moderation inside the pipe.
If you change this: Add a policy store to every node, and define who may edit it.

**D17 — Every message proves itself.**
Decision: A message carries no claim. The receiver takes the sender's card from the connection's TLS-authenticated remote id, and verifies the message authentication code keyed from the secret of the network, locally. A verified message from an unknown sender adds the sender to the peer table.
Reason: A message can arrive before the presence of its sender. Verification must not depend on gossip state.
Rejected: Accept only from nodes whose presence has arrived: the race above.
If you change this: Messages from new members are refused at random, by the order of arrival.

**D44 — Infrastructure records are guarded by the owner's key.**
Decision: The owner holds one signing key; its public half is in the network configuration. The owner signs each host's node id once: that signature is the host's certificate. A member accepts an infrastructure record only if the certificate verifies under the owner key, the record is signed by the certified id, and its timestamp is newer than the newest seen for that host, which the configuration keeps across restarts as of its last rewrite (3.3); after a crash the saved value can lag. The host's timestamp is a counter it persists, never behind the wall clock in seconds; a member drops, and never saves, a timestamp more than 300 s ahead of its own clock, so one bad record cannot silence a host for good. A learned host not heard for 30 days leaves the configuration; hosts the owner configured stay.
Reason: Every secret holder is equal (D16), so without an authority any member could announce a relay and pull members onto it: no loss of confidentiality, since traffic is encrypted end to end, but a loss of availability and of metadata. Unit-tested: a record signed by another key, and a record from a member that is not a host, are both rejected.
Rejected: The owner signs each record offline and hosts only rebroadcast it: an owner act for every record. Trust on first use of the invitation's host: a member that joins through a forged host trusts it.
If you change this: A member that holds the secret can move other members onto a relay of its choice.

**D45 — A member drops a dead home relay from its relay map.**
Decision: A member whose home relay stays disconnected removes it from its relay map, which forces a full report and a new home. It adds the relay back when its host announces again.
Reason: Without the removal, re-homing on a live relay takes about 40 s; with it, 0.7 s (measured, macOS and Linux), with no false removal in a healthy network.
Rejected: Waiting for iroh's periodic report: two re-stun intervals, about 40 s.
If you change this: Members homed on a dead relay are unreachable for about 40 s.

**D46 — A member rebinds its endpoint when gossip falls silent.**
Decision: A member that knows at least one other node (another host among its known hosts, or an Online peer) and has heard no gossip payload from anyone for 15 s is deaf: it closes its endpoint, binds a fresh one with the same key, builds a new gossip instance, and joins the topic again through every known host and every Online peer whose address it holds; at most one rebind per 20 s; connections drop at each rebind. At a rebind the member probes the known relays and strips the relays found down from the addresses it holds for other members. A join that gains no neighbor within 5 s ends in a rebind. After a rebind, a member with no other member among its gossip neighbors joins the other members directly until one is a neighbor. The bootstrap node of a host never rebinds on silence and has no join timeout; when it starts or restarts, it joins through every other known host and every peer whose address it holds. The trigger depends on D14: every bootstrap node rebroadcasts its Infrastructure payload every 5 s, so a member that can hear at all hears something within 5 s; a network with no live host gives no such floor. The 15 s value is measured across machines (first member rebind about 27 s after a host's death, all pairs flowing from 32.6-33.1 s, n=4), and confirmed only with hosts that do not rebind.
Reason: In iroh 1.3.0 one dead relay's full send queue blocks every relay send of an endpoint, members on live relays included, and iroh-gossip 0.101.0 never re-dials a peer whose dial failed; only a fresh endpoint and a fresh gossip instance recover. Measured: the rebind alone heals every stalled pair but relapses; with the probe, the join timeout, and the on_join fix of D49, no run needed a second rebind (0/16 runs, macOS and Linux, against 9/16 without the probe); the direct member join removed late attachment (0/24 late endpoints, against 5/21). Across machines, hosts applying the deaf rule and join timeout with their only other host dead rebound about every 8 s and pulled members along (relay-only: 7 of 8 runs unrecovered at 120 s; with IP transports: an outage in 4 of 4 that the control never had); hosts without the rule: every member rebound once, 0 relapses (n=4). The 5 s records are periodic traffic, which D18 rejects for members; they differ because they scale with the number of hosts, a few machines of the owner, and not with the number of members: each member receives one record per host every 5 s.
Rejected: Rejoining the topic on the same endpoint: it healed 0 of 62 stalled pairs. A gossip that re-dials a failed peer: it keeps the block alive. Waiting for upstream fixes: none is released.
If you change this: The death of one relay host stalls gossip for every member, with no recovery within 180 s (measured).

### 2.6 Presence

**D18 — Introduction replaces heartbeats.**
Decision: A node that joins the topic announces its presence. The first Presence follows the gossip layer's joined signal. A member that receives the presence of an unknown card replies with its own, directly to that card's node. A node introduces itself to each new gossip neighbor. Members announce again after a reconnect, and when their inbox attaches or detaches.
Reason: A late joiner learns everyone without periodic traffic.
Rejected: Periodic heartbeats: constant traffic that grows with the member count.
If you change this: A late joiner sees only members that announce after it joined.

**D19 — A clean stop sends a departure.**
Decision: When the binary stops, it announces a departure before it exits. Peers mark the member gone. The broadcast only queues the Departure; the wait is what delivers it (measured: immediate exit reached 0-2 of 4 peers; 1000 ms reached 4 of 4). The lock is released first so a restart on the same card is never blocked; a late Departure carries a lower seq than the new session's Presence and is ignored (3.8). The binary notes the end of its standard input with its own reader, so the Departure goes out at once and not after the MCP library's drain (measured: under 1 ms, against 5 s).
Reason: Most exits are clean. The departure is exact and immediate.
Rejected: Nothing.
If you change this: Peers see a departed member as reachable until a probe fails.

**D20 — Probe before rumor.**
Decision: When the gossip layer reports that a neighbor disconnected, the node probes the neighbor directly. Only if the probe fails does the node announce the neighbor as unreachable. A member that sees a rumor about itself announces its presence again.
Reason: The gossip layer changes neighbors for its own reasons. A disconnect alone does not mean a crash.
Rejected: Rumor on disconnect alone: false reports that spread through the network.
If you change this: Reachable members appear and disappear with the gossip topology.

**D21 — Presence and acks carry an attached flag.**
Decision: A node knows whether a bridge is connected to its inbox socket. The flag is in the ack of every message and in the presence of the member.
Reason: Delivered means the message is in the inbox. The sender should know whether anything will carry it into the session.
Rejected: Refuse messages when nothing is attached: the agent may poll the inbox tool instead.
If you change this: Senders cannot tell a live agent from an unattended process.

### 2.7 Delivery

**D22 — Send is synchronous and acknowledged.**
Decision: The send tool returns when the recipient node acknowledges the message, or when the attempt fails. The ack means the message is in the inbox of the recipient, never that it was read.
Reason: The ack is the only proof of delivery. It costs one round trip.
Rejected: Fire and forget: the sender learns nothing.
If you change this: Invariant I7 breaks.

**D23 — No store-and-forward.**
Decision: No node keeps a message for a member that is absent. The inbox of a node dies with the process.
Reason: A mailbox would make a node a party to the conversation, and a mailbox server would be a center.
Rejected: Sender retry until a time-to-live: a sender that dies loses the message. A per-machine mailbox process: a center on each machine.
If you change this: Invariant I6 breaks, and the design needs durable storage and a delivery agent.

**D24 — Send never refuses before it tries.**
Decision: The send tool attempts delivery to any handle in the peer table, whatever the recorded status. The result reports the outcome. The peers tool shows departed and unreachable members as advice.
Reason: Status is a guess. A failed attempt costs one round trip and gives the exact answer.
Rejected: Refuse sends to members recorded as absent: a stale record blocks a live member.
If you change this: A member that returned during a partition is unreachable until its presence arrives.

**D25 — No duplicate suppression.**
Decision: The receiver keeps no set of message identifiers and enqueues every accepted message. The binary never retries a send.
Reason: Each send mints a fresh identifier and the binary never retries, so a duplicate set would never fire. An agent that sends twice sends two messages.
Rejected: A duplicate set of recent identifiers: state that guards against a retry the binary does not make.
If you change this: Add retries, and the duplicate set they need.

**D26 — Loop protection belongs to the bridge.**
Decision: The binary applies no rate limit and no repeat suppression. A bridge limits the rate of injection into its session.
Reason: The cost of a loop is paid in the session. The harness or its bridge owns that cost.
Rejected: Limits in the binary: a policy in the pipe that the society cannot see.
If you change this: Define the limits and where the sender learns about them.

**D27 — Broadcast is many sends.**
Decision: The broadcast tool sends one message to each named handle and returns one result per handle. It never sends to everyone present.
Reason: A broadcast is a convenience. Delivery semantics stay those of send.
Rejected: A gossip broadcast to all present members: no acks and no choice of audience.
If you change this: Either acks are lost or the audience becomes everyone.

### 2.8 Surface

**D28 — Six tools, static.**
Decision: The MCP server exposes exactly these tools at all times: list_cards, init, peers, send, broadcast, inbox. Before a successful init, every tool except list_cards and init returns the not_claimed error.
Reason: A static tool list depends on nothing in the harness. The tools are a messaging primitive, not a state machine.
Rejected: Tools that appear after init: depends on the harness to refresh its tool list, and fails silently when it does not.
If you change this: Invariant I4 breaks.

**D29 — The address of a member is the effective handle only.**
Decision: The to field of send and broadcast is an effective handle. The from field of a received message is an effective handle. Fingerprints and keys are internal.
Reason: D5 makes effective handles distinct among present members. Agents should not carry keys.
Rejected: Handle with fingerprint suffix, or raw keys, as addresses: noise.
If you change this: Agents must learn a second naming scheme.

**D30 — Every tool response carries the identity status of the caller.**
Decision: Each response contains the card, the effective handle, and the claim status of the caller. This covers tool results; a request the MCP library rejects before any tool runs gets the library's own JSON-RPC error.
Reason: The inbox carries messages only. Identity changes, such as an adjusted handle, must reach the agent some other way.
Rejected: System frames on the inbox: breaks invariant I3.
If you change this: An agent can keep a name that is no longer its own with no signal.

**D31 — A received message has five fields.**
Decision: An agent sees from, body, sent_at, received_at, and id. Nothing else.
Reason: The primitive is text from one member. The sender's clock, read by its own process, is the one stamp a receiver cannot guess. Threads and other metadata belong to the society.
Rejected: A reply_to field: threads in the pipe.
If you change this: Every bridge template and the inbox frame change.

### 2.9 Process

**D32 — One binary process per session. Agent equals node.**
Decision: The harness starts the binary as an MCP server for each session. Each process is one node; after init, its endpoint key is its card's key.
Reason: No daemon, no shared state, no center on the machine. A session that dies takes exactly its own node with it.
Rejected: One daemon per machine that hosts many agents: a center on each machine, and a second thing to keep alive.
If you change this: Agents and nodes become different things, and every card needs a node-to-agent map.

**D33 — The inbox is a Unix socket named by the process identifier.**
Decision: Each process exposes its inbox on a socket whose path derives from its process identifier. Frames carry a sequence number. A client reconnects with the last sequence number it saw and receives what it missed, from a memory buffer.
Reason: A bridge finds the socket through the process tree, without a file written by the binary and without agent action. Any process of the same user can connect: the socket trusts the user, not the bridge.
Rejected: A TCP port returned by the init tool: a bridge started by the harness cannot see tool results.
If you change this: Every bridge changes its discovery.

**D34 — The binary writes no address file.**
Decision: The binary announces its socket to nobody. The path is derivable from the process identifier alone.
Reason: A written file is state to clean up and a race to lose.
Rejected: Nothing.
If you change this: Add cleanup of stale files on every start.

### 2.10 Bridges

**D35 — Bridges are harness code, in separate packages, in the same repository.**
Decision: Each harness has its own bridge package. The binary contains no harness code. The packages share the repository so that they version together.
Reason: Invariant I1. A new harness gets a new bridge and no change to the binary.
Rejected: Bridges as subcommands of the binary: harness code inside the binary.
If you change this: Invariant I1 breaks.

**D36 — A bridge injects one message per frame, in the native notice form of its harness, with its own mid-turn policy.**
Decision: A bridge injects each inbox frame as one notice. Where the harness has a notice form that is not user input, the bridge uses it. The bridge decides whether a message interrupts a turn or waits for a boundary. The bridge reads no meaning from a body.
Reason: Harnesses differ. The one rule that must hold everywhere is that a peer message never looks like the owner of the session.
Rejected: A uniform text frame across harnesses: discards native forms that already mark the source. Batched injection: loses order of events.
If you change this: Define the batch rule and the interruption rule for every harness.

**D37 — The Claude Code bridge uses the cross-session socket, authenticated by the session token.**
Decision: A session-start hook starts the bridge. The bridge writes each message to the socket named by CLAUDE_CODE_MESSAGING_SOCKET, opening every connection with the auth line that carries the token from CLAUDE_CODE_MESSAGING_TOKEN. No crossSessionInbound value is set for the agent session. The bridge does not register peers as local sessions, and it does not use the channels mechanism.
Reason: The socket wakes an idle session and delivers at tool boundaries during a turn. A message that carries the session's own token is delivered as the session's own child's, without the hold that applies to peer sessions under permission bypass. The native tools of Claude Code for cross-session messages are replaced by the six tools; an accept setting would admit every other local session's native messages through the channel being replaced, and a refuse setting would drop the bridge's own frames with them.
Rejected: Peers registered as fake local sessions: depends on undocumented session files and envelopes. Channels: a research preview, enabled per session by flag, admin-gated for teams. crossSessionInbound set to accept: widens the replaced channel instead of closing it.
If you change this: Delivery to Claude Code sessions depends on a different mechanism with different hold rules.

**D38 — The pi bridge is a pi extension.**
Decision: The extension finds the binary, reads the inbox socket, and injects into the session. pi reaches the six tools through its MCP support.
Reason: pi exposes the session to extensions directly.
Rejected: Nothing.
If you change this: Define another injection path for pi.

**D39 — Each bridge has its own doctor.**
Decision: Each bridge package ships a check command for its own wiring. The binary does not run them and does not know them.
Reason: Invariant I1. Harness wiring is the most likely point of failure, and the bridge is the only code that knows the harness.
Rejected: A doctor in the binary that finds and runs bridge checks: a discovery scheme, and the binary learns the names of harnesses.
If you change this: Invariant I1 bends.

### 2.11 Operator

**D40 — The peers and tail commands run a read-only observer node.**
Decision: The command starts a node without a card, with a fresh key on each run, joins the topic, collects presence for a short time, prints it, and exits. Each run leaves one entry in iroh's address cache of every node it touches (D3); accepted, since operator runs are rare.
Reason: No daemon exists to ask. An observer sees what every member sees.
Rejected: A query socket on every session process: answers only while a session runs.
If you change this: Operators need a process to query.

**D41 — The operator can send a test message with a scratch card.**
Decision: The send command claims a reserved operator card for the duration of the command and reports the result.
Reason: A delivery test needs a member.
Rejected: Nothing.
If you change this: Operators cannot test delivery without a harness.

**D42 — The doctor command checks the node only.**
Decision: The doctor checks the stored secret, each card, each sidecar and its permissions, the match between a card and its sidecar, the reachability of each known relay, and the visibility of at least one peer.
Reason: These are the things the binary knows.
Rejected: Harness checks in the binary: see D39.
If you change this: See D39.

**D43 — One operator command runs the infrastructure of one host.**
Decision: The command runs the embedded relay and the bootstrap node of D14 on one host, broadcasts the host's infrastructure record, and prints the host address.
Reason: One process to host, one address to share. A second host is a second run of the same command on another machine.
Rejected: Nothing.
If you change this: See D14.

### 2.12 Stack

**D47 — The relay ships behind a cargo feature.**
Decision: The relay server is compiled only into infrastructure builds, behind the cargo feature `relay`.
Reason: Members never run a relay. The relay server adds about 40 crates to the tree (measured: 280 without it, about 320 with it).
Rejected: The relay in every build: a larger binary and tree on every member machine.
If you change this: Every member build carries the relay server and its dependencies.

**D48 — Cards are parsed with yaml-rust2 and our own converter.**
Decision: The binary parses cards with yaml-rust2. Its own converter turns `public` into JSON bytes.
Reason: The serde YAML crates rest on unsafe-libyaml (RUSTSEC-2024-0449).
Rejected: serde YAML crates on unsafe-libyaml.
If you change this: Check the advisory status of the replacement, and the conversion rules of 3.5.

**D49 — iroh-gossip is pinned to 0.101.0, with the on_join fix carried by a patch.**
Decision: iroh-gossip is pinned to 0.101.0 exactly. The build replaces it, through Cargo's `[patch.crates-io]`, with a copy that carries the one-line on_join change of iroh-gossip PR #161: a Join drops a neighbor request still pending to the joiner. The patch goes when a release carries #161. Upgrade when the churn fixes ship (#161, #178, #189), with a mixed-version join test.
Reason: 0.101.0 is the latest release. Without the change, a bootstrap node that holds a stale neighbor request leaves a rejoining member's Join unanswered (57-60 s measured, each ended by the test's own timeout or a shuffle); with it, 0 late joins (measured, macOS and Linux). `[patch.crates-io]` is Cargo's own mechanism.
Rejected: Tracking iroh-gossip main: unreleased code. Carrying the re-dial fix of #178: it keeps the relay block of D46 alive (measured).
If you change this: Rerun the join tests of 3.15 against the new version, and drop the patch if the release carries the change.

**D50 — The direct protocol is written by hand on iroh streams.**
Decision: The binary reads and writes the structures of 3.6 itself, one request per stream.
Reason: A hand-written reader caps a request at the wire limit and resets only its stream (measured).
Rejected: irpc 0.17: a fixed 16 MiB cap, allocation of the declared size before the body arrives, and a closed connection on a bad frame.
If you change this: Recheck the limits of 3.14 against the library's own.

**D51 — Infrastructure relays serve HTTPS with automatic certificates.**
Decision: The relay of a host serves HTTPS with a certificate obtained automatically by ACME (Let's Encrypt) for the host's domain name; plain HTTP is not served. It enables QUIC address discovery. The certificate cache directory holds the certificate's private key and is kept at mode 0700.
Reason: The owner commits to HTTPS if it is low effort and high benefit. HTTPS proves the relay's identity to a member; the relay already sees only encrypted traffic. Measured across two machines: a trusted certificate 9.15 s after first start from a 10-line configuration, needing only TCP 80 and 443; a restart loads the cached certificate in 2 ms; HTTPS adds 87 ms to coming online and nothing measurable after. Without QUIC address discovery a member with direct paths takes 3.1 s to come online, with it 0.49 s. iroh-relay writes the cached key file with mode 0644, so the directory's mode is what protects it.
Rejected: Plain HTTP relays: nothing proves to a member which server it reached. A relay without QUIC address discovery, iroh-relay's default: 2.6 s slower to come online.
If you change this: Members connect to relays whose identity nothing proves.

### 2.13 Deferred

Out of scope for this design, and open: human members, movement of cards between machines, threads, status events on the inbox, any queue, lookup of unknown handles through neighbors, referral of third-party claims inside messages, rotation and revocation of the network secret, revocation and expiry of host certificates, local network discovery (mDNS).

---

## Part 3 — Specification

For the implementer.

Build exactly what this part says. Where this part is silent, apply the records of Part 2. Do not add a feature that Part 2 rejects or defers.

These choices are the implementer's call: the source of probe nonces (any cryptographically secure generator); how the stale-socket check finds out whether a process exists, including PID reuse; where the user id for the runtime directory comes from; the per-line output formats of list cards, show peers, tail, and doctor; how many broadcast sends run at once; how timers (Gone retention, waits) are implemented.

### 3.1 Terms with exact meaning

| Term | Exact meaning |
|---|---|
| card public key | 32 bytes, Ed25519 public key, encoded as 64 lowercase hexadecimal characters. It is also the node id of the node that holds the card, in the same form (iroh's own text form). |
| card secret key | 32 bytes, Ed25519 seed, encoded as 64 lowercase hexadecimal characters and one newline, in the sidecar. It is also the node's endpoint key. A reader accepts either case and a missing newline. |
| network secret | 32 random bytes, encoded as 64 lowercase hexadecimal characters. |
| owner key | An Ed25519 key pair. The public half is 64 lowercase hexadecimal characters in the network configuration; the secret half stays on the owner's machine (3.3). |
| topic id | 32 bytes, derived from the network secret. See 3.4. |
| mac key | 32 bytes, derived from the network secret. See 3.4. |
| gate key | 32 bytes, derived from the network secret. See 3.4. |
| mac | 32 bytes, the keyed BLAKE3 hash of a byte string under the mac key. |
| handle | A string that matches `^[a-z][a-z0-9-]{0,31}$`. |
| effective handle | As a member announces it: a handle, or a handle followed by `-` and a decimal integer of at least 2 with no leading zeros; at most 53 characters. A receiver drops a Presence, Introduce, IntroduceAck, or Message whose announced handle does not match. As a node names a peer locally (3.8): the announced handle, or the announced handle followed by one such suffix; at most 74 characters. The `to` and `from` fields carry local names; a `to` longer than 74 characters is `bad_request`. |
| fingerprint | The first 4 bytes of a card public key, encoded as 8 lowercase hexadecimal characters. Internal. |
| message id | A UUID version 7. 16 bytes on the wire. The canonical string form in JSON. |
| timestamp | Milliseconds since the Unix epoch, as an unsigned 64-bit integer. |
| seq | An announcement sequence number, an unsigned 64-bit integer. See 3.8. |
| wire encoding | postcard. Every top-level wire structure (each request, response, payload, HostAddr, and Invitation) starts with a version field `v: u8` equal to 1; a nested structure (Addr) does not. A sum type is encoded by postcard's variant index, in the order this document lists the variants; new variants are only appended. A fixed-size byte array (`[u8; 16]`, `[u8; 32]`, `[u8; 64]`) is encoded as its bytes with no length prefix, as iroh's own `Signature`. |
| local encoding | JSON, one object per line, terminated by a newline. Used on the inbox socket and in MCP results. Key order carries no meaning. |

### 3.2 Repository layout

```
/                 workspace root
  core/           the binary (Rust crate)
  bridges/
    claude/       Claude Code bridge: a small executable and a hook configuration
    pi/           pi bridge: a pi extension
  blueprint.md    this document
```

The core crate has no dependency on either bridge. Neither bridge has a dependency on the core crate as a library. A bridge talks to the binary only through the inbox socket of 3.11.

Manifest of the core crate. Versions with `=` are pinned: iroh, iroh-gossip, iroh-relay, and rmcp by decision, postcard by the draft manifest. For the others, the version in parentheses is the one the draft lockfile resolved.

| Crate | Version | Features |
|---|---|---|
| iroh | =1.3.0 | default |
| iroh-gossip | =0.101.0, replaced through `[patch.crates-io]` by a copy carrying the on_join change of D49 | default |
| iroh-relay | =1.3.0, optional, only under the feature `relay` (D47) | `server` |
| rmcp | =3.5.1 | default features off; `server`, `transport-io` |
| clap | 4 (4.6.7) | `derive` |
| serde | 1 (1.0.229) | `derive` |
| postcard | =1.1.3 | default features off; `alloc` |
| serde_json | 1 (1.0.154) | default |
| blake3 | 1 (1.8.7) | default |
| data-encoding | 2 (2.11.1) | default |
| uuid | 1 (1.28.0) | `v7` |
| tokio | 1 (1.54.0) | `rt`, `macros`, `net`, `time`, `sync`, `io-util`, `signal` |
| toml | `*` (1.1.8) | default |
| yaml-rust2 | `*` (0.13.0) | default |
| libc | 0.2 (0.2.191) | default |

The resolved tree has 280 unique crates without the feature `relay` and 320 with it (`cargo tree -e normal`, the draft lockfile, before the patch). Recount at build time.

### 3.3 Filesystem layout

Resolve three base directories at start.

| Base | Resolution |
|---|---|
| config | `$XDG_CONFIG_HOME`, else `$HOME/.config`, then the subdirectory `estafette`. |
| data | `$XDG_DATA_HOME`, else `$HOME/.local/share`, then the subdirectory `estafette`. |
| runtime | `$XDG_RUNTIME_DIR`, else `$TMPDIR` then `/tmp`, then the subdirectory `estafette` with the user id in its name. |

The binary creates the `estafette` config, data, and runtime directories with permissions 0700, and never loosens the permissions of an existing one. Card names and network names match the handle grammar of 3.1.

Files:

| Path | Content |
|---|---|
| `<config>/networks/<network>.toml` | Network configuration. Permissions 0600. |
| `<config>/networks/<network>.owner.key` | The secret half of the owner key, as 64 lowercase hexadecimal characters and one newline. Permissions 0600. Only on the machine where the network was created. |
| `<data>/cards/<network>/<card>.yaml` | A card. The file stem is the card name. |
| `<data>/cards/<network>/<card>.key` | The sidecar of that card. Permissions 0600. |
| `<data>/infra/<network>.key` | The key of the bootstrap node of this host. Permissions 0600. Only on an infrastructure host. |
| `<data>/infra/<network>.cert` | The certificate of this host (3.4), as 128 lowercase hexadecimal characters and one newline. Only on an infrastructure host. |
| `<data>/infra/<network>.ts` | The last record `ts` this host sent (3.14), as a decimal integer and one newline. Permissions 0600. Only on an infrastructure host. |
| `<data>/infra/<network>.certs/` | The relay's ACME account and certificate cache, private key included. Permissions 0700, set by the binary whatever mode iroh-relay gives the files inside. Only on an infrastructure host. |
| `<runtime>/<pid>.sock` | The inbox socket of the process with that process id. |

The flag `--cards-dir` and the environment variable `ESTAFETTE_CARDS_DIR` override the cards directory. Nothing else is configurable by path.

Network configuration file:

```toml
secret = "<network secret>"
owner = "<owner public key>"
hosts = [                                 # host addresses, as given by the invitation and learned at runtime
  { id = "<node id>", relay = "https://relay.example/", ts = <record counter> },                       # configured by the owner
  { id = "<node id>", relay = "https://relay2.example/", ts = <record counter>, heard = <timestamp> },  # learned
]
```

A host with no `heard` key came from an invitation or from join network: the owner configured it, and no rule removes it. A learned host carries `heard`, the timestamp its record was last heard. Every host may carry `ts`, the newest record `ts` accepted for it (the host's counter, in seconds, 3.14); a process loads these at start as the newest `ts` held per host, so a restart does not accept an older record again. A node that learns a host (3.8) adds it to `hosts` and rewrites the file whole, by writing a new file with permissions 0600 and renaming it over the old one. Each rewrite first re-reads the file and merges it with what this process holds: the union of the hosts, the larger `ts` and the later `heard` of each host; then it drops every learned host whose `heard` is older than the infrastructure record expiry. Another process may write between the re-read and the rename; the merge keeps that loss to one window. On entering Stopping, a process rewrites the file once more if it holds a `ts` newer than the file's.

Remove a stale socket file at start if no process with that id exists. Remove your own socket file at exit.

### 3.4 Cryptography

Key derivation: `derive(context, secret)` is BLAKE3 `derive_key` with the context string and the 32 secret bytes as key material.

| Value | Derivation |
|---|---|
| topic id | `derive("messaging topic v1", secret)` |
| mac key | `derive("messaging mac v1", secret)` |
| gate key | `derive("messaging gate v1", secret)` |

The secret must be high-entropy: a captured gate nonce and proof allow an offline guess.

Payload signature: every gossip payload (3.7) carries `sig`, an Ed25519 signature by the payload's signer over an ASCII domain prefix followed by the postcard encoding of the `Payload` value, variant index included, with `sig` and `mac` set to zero bytes. The signer and prefix of each payload:

| Payload | Signer | Prefix |
|---|---|---|
| Presence | `card` | `estafette-presence-v1` |
| Departure | `card` | `estafette-departure-v1` |
| Unreachable | `reporter` | `estafette-unreachable-v1` |
| Infrastructure | `host` | `estafette-infra-v1` |

A payload is valid when the signature verifies against the signer's key.

Mac on a wire structure: encode the structure with its `mac` field set to 32 zero bytes, then compute the keyed hash of those bytes. For a gossip payload the structure is the `Payload` value, variant index included; for a direct request or response it is the `Request` or `Response` value, variant index included, which is also what the stream carries. Verify by recomputation and constant-time comparison. On a signed payload, compute the mac after the signature; a receiver verifies the mac first, then the signature.

Host certificate: an Ed25519 signature by the owner key over the ASCII bytes `estafette-host-cert-v1` followed by the 32 bytes of the host's node id. The prefixes keep a signature made for one purpose from verifying in another. It is valid when it verifies against `owner` of the network configuration.

### 3.5 Card and sidecar

A card is a YAML mapping. The binary reads these keys and ignores every other key.

| Key | Required | Rule |
|---|---|---|
| `handle` | yes | A handle. The requested handle. |
| `public_key` | yes | A card public key. |
| `network` | no | A network name. If absent, the network given to the process is used. |
| `public` | no | A mapping with string keys whose values are strings, numbers, booleans, null, sequences, or mappings of the same. Its content travels in presence. Everything else in the card stays local. |

Validation at claim: the file parses, `handle` matches the handle grammar, `public_key` decodes to 32 bytes, `public` holds only the values above, the sidecar exists, the sidecar has permissions 0600, the sidecar decodes to 32 bytes, and the public key derived from the sidecar equals `public_key`. A tag, a date, binary data, a merge key, a non-string key, or a duplicate key in `public` gives `card_invalid`. Any other failure gives `card_invalid` or `sidecar_invalid`.

The binary converts `public` to compact JSON bytes (no whitespace). These bytes are what travels and what the cap counts; a receiver verifies and stores them verbatim and never re-serializes them for any check. The serialized `public` mapping has a maximum size of 3072 bytes, so the largest Presence fits the default 4096-byte gossip frame. A larger mapping gives `card_invalid`.

Card creation by the operator command: generate a key pair, write the sidecar with permissions 0600, sync the sidecar file and its directory (`sync_all` on both), then write a card with `handle` and `public_key` only. Do not overwrite an existent card or sidecar. Init reads a sidecar and never writes one: a card whose sidecar is missing fails init with `sidecar_invalid`.

### 3.6 Direct protocol

Transport: an iroh endpoint with the card key, built at init (D10), with the relays of the known hosts (3.8) and a QUIC idle timeout of 3.14. No address lookup service: no pkarr, no DNS, no local network discovery; addresses come from presence (3.8). One ALPN identifier, `estafette/direct/1`. Accept a connection from any node id. The remote id of a connection is the card of the node at the other end: TLS proves it. Authorization is per request, by mac, not per connection.

Gate: on every gossip connection, in both directions, each side proves the network secret before the gossip layer sees the connection. The verifier opens a new bidirectional stream and sends a 32-byte random nonce; the prover answers `keyed_hash(gate key, nonce || verifier id || prover id)`. A missing or wrong answer within the gate timeout closes the connection. The gate runs in the endpoint's after_handshake hook, so neither an accepted connection nor one that gossip dials reaches the gossip layer unproven.

A node keeps one connection per peer and reuses it; each request opens a new bidirectional stream. One request per bidirectional stream. The client writes one request structure and finishes its send side. The server writes one response structure and finishes. Both sides close the stream. Apply the limits of 3.14 to stream length. A server waits at most the server read timeout for a request's bytes, then resets that stream; the connection stays. A server that reads past the wire limit resets the stream with error code 1; the client reports a reset as `refused`.

```
Request = Message | Probe | Introduce

Message {
  v: 1
  id: [u8; 16]          message id
  to: [u8; 32]          card public key of the recipient
  handle: String        the effective handle the sender uses
  attached: bool        inbox state of the sender
  sent_at: u64
  body: String          UTF-8, maximum 1,048,576 bytes
  mac: [u8; 32]
}

Probe {
  v: 1
  nonce: [u8; 16]
  mac: [u8; 32]
}

Introduce {
  v: 1
  handle: Option<String>   the effective handle the sender uses, if it holds a card
  public: Vec<u8>       the serialized public mapping of the card, as JSON bytes
  attached: bool
  addr: Addr
  mac: [u8; 32]
}

Addr {
  relays: Vec<String>   relay URLs of the sender's endpoint; at most 4, each at most 128 bytes
  direct: Vec<SocketAddr>   direct socket addresses of the sender's endpoint, in postcard's encoding of a socket address (V4 or V6); at most 8
}

Response = MessageAck | ProbeAck | IntroduceAck

MessageAck {
  v: 1
  id: [u8; 16]
  accepted: bool
  attached: bool        inbox state of the recipient
  reason: Option<String>   an error code of 3.10 when accepted is false
}

ProbeAck {
  v: 1
  nonce: [u8; 16]       the nonce of the Probe
  attached: bool
  mac: [u8; 32]
}

IntroduceAck {
  v: 1
  handle: Option<String>   the effective handle this node uses, if it holds a card
  public: Vec<u8>
  attached: bool
  addr: Addr
  mac: [u8; 32]
}
```

The sender of a request is the card of the connection's remote id; no request names its sender otherwise.

Address caps: an Addr carries at most 4 relay URLs of at most 128 bytes each, and at most 8 direct addresses. A node with more carries the first ones its endpoint reports, up to the caps, and never a relay URL longer than 128 bytes. A receiver drops a structure whose Addr exceeds a cap. A node leaves link-local IPv6 addresses (fe80::/10) out of Addr: the encoding carries no scope id, so such an address would not be dialable. With these caps the largest Presence encodes to 3951 bytes, within the 4058 bytes the 4096-byte gossip frame leaves after the gossip layer's 38 (test 17).

Validation of a received Message, in this order. Stop at the first failure.

1. `v` is 1, else close the stream.
2. The mac verifies, else close the stream without a response.
3. `to` equals the card of this process, and the process is not Stopping, else respond `accepted: false, reason: "refused"`.
4. `body` is valid UTF-8 within the limit, else respond `too_large`.
5. Apply the Message rows of 3.8 to the sender. Enqueue the message in the inbox (3.11). Respond `accepted: true`.

Validation of Probe and Introduce: `v` is 1 and the mac verifies, else close the stream.

A probe fails when no valid ProbeAck arrives within the probe timeout, for any reason. A ProbeAck is valid when its mac verifies, its nonce equals the Probe's, and it comes from the probed card.

### 3.7 Gossip protocol

Subscribe to the topic id with the node ids of the known hosts (3.8) as bootstrap. Every payload is one structure. Departure and Unreachable travel on the topic, as Presence does.

```
Payload = Presence | Departure | Unreachable | Infrastructure

Presence {
  v: 1
  card: [u8; 32]
  handle: String        the effective handle the member uses
  public: Vec<u8>
  attached: bool
  addr: Addr            the member's full current endpoint address (3.6)
  seq: u64
  mac: [u8; 32]
  sig: [u8; 64]
}

Departure {
  v: 1
  card: [u8; 32]
  seq: u64
  mac: [u8; 32]
  sig: [u8; 64]
}

Unreachable {
  v: 1
  card: [u8; 32]        the card reported unreachable
  reporter: [u8; 32]    the card of the reporting member
  ts: u64               when the reporter observed it
  mac: [u8; 32]
  sig: [u8; 64]
}

Infrastructure {
  v: 1
  host: [u8; 32]        node id of the host's bootstrap node
  relay: String         relay URL of the host
  addr: Addr            address of the bootstrap node
  ts: u64               the host's record counter, in seconds (3.14); newer than the last accepted for this host
  cert: [u8; 64]        the host certificate (3.4)
  mac: [u8; 32]
  sig: [u8; 64]
}
```

Drop any payload whose mac does not verify. Drop any payload whose signature (3.4) does not verify. Drop an Infrastructure payload whose certificate does not verify, whose `relay` does not parse as a relay URL, or whose `addr.relays` is not exactly [`relay`], compared as parsed relay URLs. Gossip delivers a payload through forwarders, so the node that handed a payload over is not its origin; over gossip the payload signature is the only binding of a payload to its card, and on a direct stream the remote id is the card (3.6). The serialized `public` field has a maximum of 3072 bytes.

A node checks every payload's encoded size against the gossip frame limit before broadcasting and never sends a larger one; the broadcast fails with an error. An oversize broadcast returns success, is lost, and leaves the link dead (measured).

### 3.8 Peer table and presence algorithm

Peer table entry:

```
Peer {
  card: [u8; 32]
  effective_handle: String
  public: Vec<u8>
  attached: bool
  addr: Addr
  seq: u64              the highest seq accepted for this card
  status: Online | Unreachable | Gone
  last_seen: u64
}
```

Keyed by `card`. One entry per card.

Address lookup. The endpoint dials a card by its id; the address comes from a lookup that this node feeds. On every accepted Presence, and on every Introduce or IntroduceAck, the node replaces that card's entry in the lookup with the carried `addr`: replace, never merge. On every accepted Presence, and on every Introduce or IntroduceAck, the node also drops its held connection to that card, unless it is the connection the Introduce or IntroduceAck came on, so the next request dials afresh: a restarted session must not leave sends on a dead connection. The gossip layer's own lookup is not relied on.

Known hosts. The hosts of the network configuration, plus every host learned from a valid Infrastructure payload, for the life of the process. The endpoint's relay map holds the relays of the known hosts, less any removed by the relay-loss row below. The first join goes through the known hosts; a rebind's join goes through every known host and every Online peer whose address the node holds. At start and at every rebind, each known host whose lookup entry holds no address gets `Addr { relays: [relay], direct: [] }` from its HostAddr, until a record or an IntroduceAck replaces it.

Announcement ordering. Every Presence and Departure a node sends carries `seq = max(now_ms, last_seq + 1)`, where `last_seq` is the last seq this node sent. A node accepts a Presence or Departure only if its signature verifies, its seq is above the seq held for that card, above any tombstone for that card, and not older than the announcement max age by the receiver's clock. A Departure leaves a tombstone of its seq for that card, kept for the announcement max age.

Events and actions:

| Event | Action |
|---|---|
| process starts | Start the inbox listener. Build no endpoint before init. |
| init takes the sidecar lock | Build the endpoint with the card key (3.6), start the direct protocol listener, and subscribe to the topic. When the topic reports joined, wait the observer wait, assign your own effective handle, and send Presence. A join that gains no neighbor within the join timeout ends in a rebind; init then assigns the handle against the table as it stands and returns, and the node sends Presence when a later join succeeds. |
| Presence received and accepted, card unknown | Insert the peer as Online. Send Introduce directly to `card`. Apply handle assignment. |
| Presence received and accepted, card known | Update `public`, `attached`, `addr`, `seq`, and `last_seen`. Set Online. |
| Presence or Departure received for your own card | Ignore it. |
| Introduce or IntroduceAck received, with a handle | Same as Presence received and accepted, for the card of the connection's remote id, without the seq check. |
| Introduce or IntroduceAck received with `handle: None`, or any contact from a node that holds no card (a bootstrap node, an observer) | Update the lookup only. The node never enters the peer table. |
| Message received and validated, card unknown | Insert the sender as Online with the Message's `handle` and `attached`. Send Introduce to the sender. Apply handle assignment. |
| Message received and validated, card known | Update `attached` and `last_seen`. Set Online. |
| gossip reports a neighbor up | Send Introduce to that node, unless it is the card of an Online entry. |
| Departure received and accepted | Set the peer Gone. Keep the entry for 3600 seconds, then remove it. |
| Unreachable received, about a peer | Drop it if its `ts` is older than the announcement max age by the receiver's clock, or below the seq held for that card. Otherwise set the peer Unreachable, unless a direct contact with that peer is more recent than `ts`. |
| Unreachable received, about yourself | Drop it if its `ts` is older than the announcement max age by the receiver's clock, or below the seq of your last Presence. Otherwise send Presence. |
| gossip reports a neighbor down | If the neighbor is not in the peer table, do nothing. Otherwise send Probe to that node. On ProbeAck, keep Online. On failure, set Unreachable and send Unreachable. |
| send to a peer fails at connect or at ack | Set the peer Unreachable. Send Unreachable. |
| any direct contact with a peer succeeds | Set the peer Online. |
| the home relay's status changes, or the node's own address set changes (iroh `home_relay_status`, `watch_addr`) | Send Presence only if the address set to announce differs from the one in the last Presence, and at most once per re-announce interval; a change inside the interval is announced when it ends, if it still differs. |
| the home relay is still disconnected the relay-loss wait after its status reported the loss | Remove that relay from the relay map (iroh `remove_relay`). |
| Infrastructure received and valid, with `ts` at or below the newest `ts` held for that host, or ahead of this node's clock by more than the infrastructure record future bound | Drop it. Hold and save nothing from it. |
| Infrastructure received and valid, with `ts` above the newest held for that host | Hold its `ts` as the newest for that host, and the time as the host's last heard. Replace the host's entry in the lookup with its address. If its relay is absent from the relay map, add it (iroh `insert_relay`; members only, 3.14); this re-adds a relay the relay-loss row removed. If the host is not known, add it to the known hosts and save the known hosts to the network configuration (3.3). |
| this node is a member, knows at least one other node (another host among the known hosts, or an Online peer), has received no gossip payload of any kind from anyone for the deaf timeout, and has not rebound within the rebind interval | Rebind. The Infrastructure broadcast of every live host (3.14) keeps a hearing node from reaching this timeout. A member that knows no other node never rebinds on silence; a bootstrap node never does (D46). |
| a rebind's topic reports joined, and no other member is a gossip neighbor after the member join wait | Ask the gossip layer to join the cards of the Online and Unreachable entries, and again every member join interval, until one of them is a neighbor, at most the member join tries in all, and never past the next rebind. |
| a client connects to the inbox socket, or the last client disconnects | Set your attached flag. Send Presence. |
| process stops | Hand the Departure broadcast to a task. Release the sidecar lock. Rewrite the network configuration if this node holds a record `ts` newer than the file's (3.3). Drop every held direct connection. From here on the direct listener answers every Message `refused` and closes every Probe and Introduce stream without an answer; the gossip broadcast stays alive. Wait the departure wait. Exit. |

Rebind. Close the endpoint. Probe each relay of the known hosts by a TCP connect to its host and port, with the relay probe timeout; strip every relay that does not answer from the lookup entries of the other members. Build a new endpoint with the same card key and the relay map, build a new gossip instance, and subscribe to the topic through every known host and every Online peer whose address it holds. When the topic reports joined, send Presence. A join that gains no neighbor within the join timeout ends in another rebind, no sooner than the rebind interval after this one. The peer table, the lookup, the known hosts, the seq, the inbox, the inbox socket, and the sidecar lock outlive a rebind; connections do not.

Handle assignment. A member announces the effective handle it uses. At this node, the effective handle of a new entry is the handle it announces, `h`, if no entry seen earlier at this node, yourself included, holds effective handle `h`; otherwise it is `h-n` for the smallest integer `n` of at least 2 such that `h-n` is free at this node. An entry keeps its effective handle for as long as it exists, Gone entries included until they are removed; a later entry never displaces it. Nodes assign from what each saw first, so two nodes can name one member differently when two members with one requested handle arrive within one propagation window; the `you` field of every tool response names the handle this node uses for itself, and `from` names the handle the receiving node uses for the sender.

Your own effective handle at init follows the same rule against the table after the observer wait that follows the join, and never changes afterward. The init tool returns it; Presence, Introduce, and Message carry it.

### 3.9 Session state machine

```
Unclaimed --init ok--> Claimed
any state --end of stdin or signal--> Stopping --Departure handed over, lock released, configuration saved, departure wait--> exit
```

In Unclaimed, every tool except list_cards and init returns `not_claimed`. Before init only the inbox listener runs. The inbox socket stays open in every state.

End of standard input, SIGTERM, SIGINT, and SIGHUP start Stopping; a second signal exits at once. The binary reads standard input through its own reader, which notes the end of input and starts Stopping at once, before the MCP library's drain. On SIGTERM the binary hands over the Departure first and, after the departure wait, shuts its runtime down in the background, so a pending read of standard input does not hold the exit. In Stopping every Message is answered `refused`: the inbox dies with the process, and a restarted session on the same card may already hold the lock.

Init in state Claimed returns `bad_request`. A process claims one card for its life.

### 3.10 MCP server

Transport: standard input and output, read through the binary's own reader (3.9). The server name is `estafette`. The tool list is fixed at start and never changes. The MCP library is rmcp with default features off and the features `server` and `transport-io`, no macros, a hand-written handler, and hand-written server info.

Tool result form: `content[0].text` is the envelope as JSON text; `structuredContent` is the envelope; `isError` is the negation of `ok`.

Response envelope, for every tool, success or failure:

```json
{
  "ok": true,
  "you": { "card": "<card name>", "handle": "<effective handle>", "status": "claimed" },
  "result": { }
}
```

```json
{
  "ok": false,
  "you": { "card": null, "handle": null, "status": "unclaimed" },
  "error": { "code": "not_claimed", "message": "Claim a card with init first." }
}
```

`status` is one of `unclaimed`, `claimed`.

Before init, the input is checked first: invalid input returns `bad_request`, valid input `not_claimed`. An unknown tool name returns the `bad_request` envelope with `you`. A request whose `arguments` is not an object gets the MCP library's own JSON-RPC error (-32601), with no envelope.

Tools:

| Tool | Input | Result |
|---|---|---|
| `list_cards` | none | `cards: [{ name, handle, held: bool }]`. `held` is true when the sidecar lock of that card is taken; the binary learns it by trying the lock and releasing it at once, except for the card this process holds, which is held without a try. |
| `init` | `card: string`, `description?: string` | `handle: string`, `inbox_path: string`. Validate the card (3.5), insert the description, and check the cap below, all before the lock; a failure returns its error and leaves the state Unclaimed. Then take the sidecar lock, retrying for up to the lock retry window so that a list_cards probe of the lock in another process does not make init fail; build the endpoint, and subscribe (3.8); return when the effective handle is assigned, at most the join timeout plus the observer wait after the endpoint is built (3.8). The description, if given, is put under `public.description` in the presence of this session only; the 3072-byte cap of 3.5 applies to `public` after the insertion, and over it init returns `bad_request` before the lock is taken. |
| `peers` | none | `peers: [{ handle, status, attached, public: object, last_seen }]`. Excludes yourself. |
| `send` | `to: string`, `body: string` | `id: string`, `delivered: true`, `attached: bool`. |
| `broadcast` | `to: string[]`, `body: string` | `results: [{ to, id?, delivered: bool, attached?: bool, error?: { code, message } }]`. Sends run concurrently. |
| `inbox` | `since?: integer` | `messages: [{ seq, id, from, body, sent_at, received_at }]`, `last_seq: integer`. Returns frames with `seq` greater than `since`. Does not remove frames. The advertised schema gives `since` a minimum of 0; a negative value is `bad_request`. |

Send algorithm:

1. If the state is not Claimed, return `not_claimed`.
2. Find peers whose effective handle equals `to`. None: `unknown_handle`. More than one: `ambiguous_handle`, with the candidates in the message.
3. A body over 1,048,576 bytes: `too_large`, before any connection.
4. Use the held connection to the peer, or dial the peer's card id; the address comes from the lookup fed by Presence and Introduce (3.8). Connect failure or a 5000 millisecond timeout: `unreachable`.
5. Write a Message. Wait for MessageAck with a 10000 millisecond timeout. Timeout: `timeout`.
6. `accepted` false: return the error with the code in `reason`.
7. Return `delivered: true` with `attached` from the ack.

On any failure in steps 4 to 6, drop the held connection to the peer and return the error. No step is retried.

Error codes. This list is closed.

| Code | Message | When |
|---|---|---|
| `not_claimed` | Claim a card with init first. | Any tool before init. |
| `card_not_found` | No card with that name in the registry. | init |
| `card_held` | That card has a live holder. | init, when the sidecar lock of the card is still taken after the lock retry window. |
| `card_invalid` | The card file did not pass validation. | init |
| `sidecar_invalid` | The sidecar is absent, unreadable, has wrong permissions, or does not match the card. | init |
| `unknown_handle` | No present member has that handle. | send, broadcast |
| `ambiguous_handle` | More than one member matches that handle. Retry shortly. | send, broadcast |
| `unreachable` | Could not connect to that member. | send, broadcast |
| `timeout` | The member did not acknowledge in time. | send, broadcast |
| `refused` | The member refused the message. | send, broadcast; also while the recipient is stopping, for example during a restart on the same card |
| `too_large` | The body exceeds the size limit. | send, broadcast |
| `bad_request` | The input did not match the tool schema. | any tool |

Tool descriptions, as given to the harness. Keep them exactly this short.

- `list_cards`: List the identity cards on this machine and whether each is in use.
- `init`: Claim one card for this session. Returns the handle you must use. Call once.
- `peers`: List the members that are present, with their handles and descriptions.
- `send`: Send a text message to one member by handle. Returns whether it was delivered.
- `broadcast`: Send one text message to several members by handle. Returns one result per member.
- `inbox`: Read messages that arrived for you. Use `since` to read only newer ones.

### 3.11 Inbox socket

Path: `<runtime>/<pid>.sock`. Permissions 0600. The listener starts with the process, before init, so a bridge can connect early.

Protocol: newline-delimited JSON, UTF-8.

Client first line: `{"since": N}`. `N` is the last `seq` the client saw, or 0.

Server frames, one per message, in `seq` order:

```json
{"seq": 12, "id": "<message id>", "from": "<effective handle>", "body": "<text>", "sent_at": 1700000000000, "received_at": 1700000000000}
```

The server sends every buffered frame with `seq` greater than `N`, then live frames as they arrive. The buffer holds the last 1000 frames. If `N` is older than the buffer, the server sends from the oldest buffered frame; the jump in `seq` is the only sign. `seq` starts at 1 and increases by 1 per message for the life of the process. The server writes nothing else on this socket. Each client gets its own replay from its own `since`, then live frames.

Nothing is removed by reading. The node does not track what was read. The sender's ack means the message entered the inbox. Gaps, limits, and what fits a harness's context are the bridge's business.

Attached is true while at least one client is connected.

### 3.12 Bridge contract

A bridge is a program or extension started by the harness, not by the agent. It does the steps below.

1. Find the harness process id.
2. Walk the descendants of that process. Select the processes whose executable is the binary and whose arguments select MCP mode.
3. For each selected process, connect to `<runtime>/<pid>.sock` and send `{"since": N}` with the last `seq` seen, or 0.
4. Inject each frame as one notice into the session.
5. Reconnect with the last `seq` when the socket closes. Poll for new descendants every 2000 milliseconds.
6. Exit when the harness process exits.

Notice content: the native form of the harness where one exists. In every form, the source must be visible as a peer, never as the owner of the session. Template for text form:

```
Message from peer <handle> (verified member of your network), sent <sent_at as ISO 8601, UTC>.
<body>
Reply with the send tool.
```

Rate limit, per sender handle: at most 10 injections in any 60 seconds, and no injection of a body identical to the previous body from that sender within 30 seconds. Count dropped frames and report them in the doctor output of the bridge. A bridge applies no policy that depends on the content of a body beyond its identity with the previous one.

Claude Code bridge:

- Hook configuration: a SessionStart hook starts the bridge executable detached from the hook. The hook passes the values of `CLAUDE_CODE_MESSAGING_SOCKET` and `CLAUDE_CODE_MESSAGING_TOKEN` and the process id of the parent of the hook.
- Injection: open a connection to the socket for each message. Write one line `{"type":"auth","token":"<token>"}`. Write one line with the message in the format that socket accepts. Close. Open the connection only when a message is ready.
- Settings: no `crossSessionInbound` value is set for the agent session. `refuse` would drop the bridge's frames; `accept` would admit every other local session's native messages.
- Doctor: the hook is installed, the bridge process runs for this session, the socket path exists, no `refuse` applies to the session, and at least one binary process was found.

pi bridge:

- The extension starts with the session, finds the binary with the steps above, and injects each frame as a notice that is not user input.
- Doctor: the extension is loaded, at least one binary process was found, the socket connection is open.

### 3.13 Operator commands

Commands are named by function. The command-line names are a choice of the owner.

| Function | Input | Effect | Output |
|---|---|---|---|
| create network | network name | Generate a secret and an owner key. Write the configuration file and the owner key file (3.3). | An invitation string. |
| join network | invitation string, or a host address line for a network already configured | Write the configuration file from the invitation, or add the host to `hosts`. | Nothing. |
| show invitation | network name | Read the configuration file. | An invitation string. |
| certify host | network name, host address line | Sign the host's node id with the owner key (3.4). Only where the owner key file exists. | The certificate, as 128 lowercase hexadecimal characters. |
| create card | network name, card name, handle | See 3.5. | The card public key. |
| list cards | network name | Read the registry. | One line per card: name, handle, public key. |
| show peers | network name | Start an observer node without a card, with a fresh key. It passes the gate, sends Introduce with `handle: None` to each gossip neighbor, answers Introduce with an IntroduceAck with `handle: None`, and sends no payload. Subscribe. Wait 3000 milliseconds for Presence and Introduce. Exit. | One line per peer: effective handle, status, attached, public fields. |
| tail | network name | Same as show peers, but print each gossip payload as it arrives until interrupted. | Payload lines. |
| test send | network name, handle, body | Claim the reserved card `operator` from the registry, create it if absent, run the send algorithm, release. | The send result. |
| doctor | network name | Check: configuration file exists with permissions 0600, each card validates, each sidecar matches, each known relay answers, an observer node sees at least one peer within 3000 milliseconds. | One line per check, pass or fail. |
| run infrastructure | network name, bind IP address, domain name | See 3.14. | The host address line. |

Host address, in the invitation and the host address line; the `hosts` of the configuration hold the same `id` and `relay` in TOML text form (3.3):

```
HostAddr {
  v: 1
  id: [u8; 32]          node id of the host's bootstrap node
  relay: String         relay URL of the host, at most 128 bytes
}

Invitation {
  v: 1
  network: String       network name
  secret: [u8; 32]      network secret
  owner: [u8; 32]       owner public key
  hosts: Vec<HostAddr>  one or more
}
```

Invitation string: `estafette-invite-1:` followed by the lowercase hexadecimal of the postcard encoding of an Invitation. An invitation needs one reachable host.

Host address line, printed by run infrastructure and accepted by join network: `estafette-host-1:` followed by the lowercase hexadecimal of the postcard encoding of the host's HostAddr.

Order for a new network:

1. create network, on the owner's machine. The configuration has no host yet. If the first host is another machine, copy the configuration file to it, never the owner key file.
2. run infrastructure on the first host. Its first run creates the host key, prints the host address line, and exits for want of a certificate.
3. certify host with that line, on the owner's machine. Write the certificate to `<data>/infra/<network>.cert` on the host.
4. run infrastructure on the first host again. With no other host in its configuration it joins its own topic alone and is its own bootstrap.
5. join network with the host address line, on the owner's machine, which adds the host to `hosts`.
6. show invitation. Every invitation from here on names at least one host.

A later host follows steps 2 to 4 with a configuration that names a live host, and joins through it.

Exit codes: 0 success, 1 failure, 2 usage error, 3 doctor found at least one failed check.

### 3.14 Infrastructure process and limits

The infrastructure command runs, in one process, an embedded relay bound to the given IP address and one iroh endpoint with the key from `<data>/infra/<network>.key`; the build carries the feature `relay` (D47). The relay serves HTTPS only, with a certificate obtained automatically by ACME (Let's Encrypt, iroh-relay's `LetsEncrypt` certificate mode) for the given domain name (D51), cached in `<data>/infra/<network>.certs/`. It enables QUIC address discovery (iroh-relay's `enable_quic_addr_discovery`). On the bind address the relay takes TCP 443 for HTTPS, TCP 80 for the ACME challenge, and UDP 7842 for QUIC address discovery; the endpoint takes any free port. The host's relay URL is `https://<domain name>/`. The endpoint's relay map holds its own relay only. The endpoint passes the gate of 3.6, joins the topic through the other hosts of the network configuration, if any; with none, it subscribes alone and is its own bootstrap. It broadcasts its Infrastructure payload when the topic reports joined and every infrastructure record interval after, with `relay` and `addr.relays = [relay]` set from its own relay URL, whatever its endpoint reports. The record `ts` is a strictly increasing counter: each record carries `ts = max(last + 1, now in seconds since the Unix epoch)`, and the host writes `last` to `<data>/infra/<network>.ts` before it broadcasts; a host that has lost the file starts from `last = 0`. It never rebinds on silence and applies no join timeout (D46). When it starts or restarts, it joins through every other known host and every peer whose address it holds. It applies the Infrastructure rows of 3.8 to learn other hosts, and saves them as a member does (3.3), except that it never adds a relay to its relay map: a relay URL in an address is enough to dial through that relay (E21, source reading). It answers Probe, and answers Introduce with `handle: None`. It accepts no Message, and refuses one with `refused`. It prints its host address line at start. Create the key file on first run. The certificate comes from `<data>/infra/<network>.cert`, written there from the output of certify host; without a valid certificate the command prints its host address line and exits with failure.

Limits:

| Limit | Value |
|---|---|
| message body | 1,048,576 bytes |
| public mapping, serialized | 3072 bytes |
| gossip frame | 4096 bytes, the gossip layer's default |
| inbox buffer | 1000 frames |
| connect timeout | 5000 milliseconds |
| ack timeout | 10000 milliseconds |
| server read timeout | 10000 milliseconds |
| QUIC idle timeout | 10000 milliseconds, set on the endpoint's transport config |
| probe timeout | 3000 milliseconds |
| gate timeout | 3000 milliseconds |
| observer wait | 3000 milliseconds |
| announcement max age | 300 seconds |
| departure wait at exit | 5000 milliseconds |
| gone entry retention | 3600 seconds |
| relay-loss wait | 500 milliseconds |
| re-announce interval | 2000 milliseconds |
| member join tries | 10 |
| infrastructure record future bound | 300 seconds ahead of the receiver's clock |
| lock retry window | 200 milliseconds |
| infrastructure record expiry | 30 days |
| deaf timeout | 15 seconds, members only; measured across machines with hosts that do not rebind; must stay above the infrastructure record interval |
| rebind interval | 20 seconds, at least, between any two rebinds |
| join timeout | 5000 milliseconds |
| relay probe timeout | 1000 milliseconds |
| member join wait | 1500 milliseconds |
| member join interval | 3000 milliseconds |
| infrastructure record interval | 5 seconds |
| wire limit (any wire structure) | 1,114,112 bytes |

### 3.15 Acceptance tests

Each test is one scenario with one expected result.

1. Two processes on one machine, two cards. A sends to B. B's inbox socket emits one frame. A's send result is delivered with attached true.
2. Same, with no client on B's socket. A's send result is delivered with attached false. B's `inbox` tool returns the message.
3. Two machines, no direct path, relay only. Same result as test 1.
4. B stops cleanly, also with a tool call in flight when its standard input closes. A's `peers` shows B gone within 1000 milliseconds. A's send to B returns `unreachable`.
5. B is killed with SIGKILL. Within one gossip neighbor-down event and one probe timeout, A's `peers` shows B unreachable. A's send to B returns `unreachable`.
6. Two processes on one machine init one card within 100 milliseconds. Exactly one ends in Claimed. The other gets `card_held`.
7. Two cards with the same requested handle, the second joining after the first's presence has spread. The second member has effective handle `<h>-2` at every node, its own included, and keeps it after the first member restarts.
8. A message arrives from a card whose presence A has not seen. A accepts it, and A's `peers` lists the sender as online.
9. A message with a wrong mac is dropped with no response.
10. A gossip payload whose signature does not verify against its card is dropped.
11. A client reconnects to the inbox socket with `since` equal to 3 after 5 frames. It receives frames 4 and 5.
12. The MCP tool list is identical before and after init.
13. Every tool response contains `you` with the current status, before and after init.
14. `broadcast` to three handles, one of them unknown, returns three results: two delivered, one `unknown_handle`.
15. The observer command on a machine with no active session shows the peers of the other machines.
16. A card whose sidecar has permissions 0644 fails init with `sidecar_invalid`.
17. The largest Presence encodes within the gossip frame limit. Worst case, postcard: variant 1, `v` 1, `card` 32, `handle` 1 + 53, `public` 2 + 3072, `attached` 1, `addr` 521 + 161 (4 relay URLs of 128 bytes: 1 + 4 × 130, each a 2-byte length and 128 bytes; 8 IPv6 socket addresses: 1 + 8 × 20, each a variant tag, 16 address bytes, and a port of at most 3 varint bytes), `seq` 10, `mac` 32, `sig` 64: 3951 bytes, at most 4058 (4096 less the gossip layer's 38).
18. B is killed and restarted with the same card. Within one second of B's new Presence reaching A, A's send to B is delivered.
19. Three nodes, with C a gossip neighbor of B only. A's `peers` shows C within the observer wait after C's Presence.
20. A replayed Presence of B's earlier session, carrying a lower seq, changes nothing in A's table. After B departs, a replay of any earlier Presence of B leaves B gone.
21. A node that holds the topic id but not the secret gets no gossip neighbor and no payload, whether it dials or is dialed.
22. A host is added while members run. Every member has the new host's relay in its relay map and the host in its network configuration within 1000 milliseconds of the host's first record, with no restart.
23. The host every member is homed on is killed, relay and bootstrap node together, while another host lives. Within 60 seconds every member's send to every other member is delivered, and no member rebinds twice.
24. Three members run for 180 seconds with no host killed. No member rebinds.
25. With one host dead, a new member whose invitation names only a live host joins, and its `peers` shows every present member within the observer wait after its own first Presence.
26. A member restarted after one host died, with the learned host in its network configuration, joins through that host.
27. An Infrastructure payload signed by a key other than its `host`, and one from a member whose id the owner never certified, are both dropped; a tampered payload is dropped; a replay of a host's earlier payload, with a `ts` not newer than the last accepted, is dropped; a valid one is accepted.
28. A member's home relay is killed. Within 2000 milliseconds the member's address lists only live relays.

### 3.16 Do not

- Do not add, remove, or hide a tool at runtime.
- Do not write a file that announces the inbox socket.
- Do not keep a message for an absent member, on disk or in memory, past the life of the process.
- Do not persist the inbox.
- Do not read any harness environment variable in the binary.
- Do not send a Message over gossip.
- Do not send a Presence on a timer.
- Do not put a secret, a token, or a sidecar path in a card.
- Do not refuse a send because of the recorded status of a peer.
- Do not push anything but message frames on the inbox socket.
- Do not take the sender of a Message from anything but the connection's remote id.
- Do not add a lock file, a pid file, or a registry index.
- Do not hold a card without its sidecar lock.
- Do not let the bootstrap node hold a card or forward a message.
- Do not put fingerprints or keys in the `to` or `from` fields.
- Do not rate limit in the binary.
- Do not retry a send at the transport level.
- Do not broadcast a payload larger than the gossip frame limit.
- Do not accept an Infrastructure payload whose certificate the owner key does not verify.
