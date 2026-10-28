//! The MCP server of spec 3.10: six static tools over standard input and output, each answered with the envelope that names the caller.

use std::fmt;
use std::future::Future;
use std::io;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use iroh::endpoint::BindError;
use iroh::{PublicKey, RelayUrl, SecretKey};
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, Implementation, JsonObject,
    ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool,
};
use rmcp::service::{MaybeSendFuture, RequestContext};
use rmcp::{ErrorData, RoleServer, ServerHandler, serve_server};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{mpsc, oneshot};

use crate::card::Card;
use crate::cli::cards::CardRegistryReader;
use crate::config::NetworkConfig;
use crate::crypto::GateKey;
use crate::dirs::{CARD_EXTENSION, Directories};
use crate::errors::{Envelope, ErrorCode, You};
use crate::handle::{AnnouncedHandle, Handle, LocalHandle};
use crate::inbox::buffer::InboxFrame;
use crate::keys::parse_public_key;
use crate::net::endpoint::NodeEndpoint;
use crate::node::{
    BroadcastResult, DEPARTURE_WAIT, Delivery, Exit, Identity, Network, Node, NodeEffects, Session,
    ShutdownSignal, forward_standard_input,
};
use crate::peers::{Peer, Status};
use crate::sidecar::{self, HeldSidecar, Pause, SidecarError};

const SERVER_NAME: &str = "estafette";
const SERVER_INPUT_BUFFER_BYTES: usize = 64 * 1024;
const TOOL_COUNT: usize = 6;
const LOCAL_HANDLE_MAX_CHARACTERS: usize = 74;
const SCHEMA_TYPE_KEY: &str = "type";
const SCHEMA_OBJECT_TYPE: &str = "object";
const SCHEMA_PROPERTIES_KEY: &str = "properties";
const SCHEMA_REQUIRED_KEY: &str = "required";
const SCHEMA_ADDITIONAL_PROPERTIES_KEY: &str = "additionalProperties";

/// Binds the node's endpoint under the card key, the gate key and the relays of the known hosts, as the composition root binds it.
pub type BindEndpoint = Arc<
    dyn Fn(
            SecretKey,
            GateKey,
            Vec<RelayUrl>,
        ) -> Pin<Box<dyn Future<Output = Result<NodeEndpoint, BindError>> + Send>>
        + Send
        + Sync,
>;

/// The network this process serves: where its cards and its inbox socket live, and its configuration with the file that keeps it.
pub struct ServedNetwork {
    pub directories: Directories,
    pub network: Handle,
    pub config: NetworkConfig,
    pub config_path: PathBuf,
    pub inbox_path: PathBuf,
}

/// The side effects the server runs on, initialized by the composition root.
pub struct ServerEffects {
    /// The node's own side effects, handed to the node at init.
    pub node: NodeEffects,
    /// Binds the node's endpoint at init.
    pub bind_endpoint: BindEndpoint,
    /// Reads the card registry.
    pub registry: Arc<dyn CardRegistryReader + Send + Sync>,
    /// Blocks the calling thread between two tries of a card's sidecar lock.
    pub pause_between_lock_tries: Arc<dyn Fn(Duration) + Send + Sync>,
}

/// Serves the six tools over the standard streams until the end of standard input or a signal, then says how the process is to end.
pub async fn serve(
    standard_input: impl AsyncRead + Unpin + Send + 'static,
    standard_output: impl AsyncWrite + Unpin + Send + 'static,
    signals: mpsc::UnboundedReceiver<ShutdownSignal>,
    served: ServedNetwork,
    effects: ServerEffects,
) -> Exit {
    let session = Arc::new(Session::new());
    let (server_input, forwarded_input) = tokio::io::simplex(SERVER_INPUT_BUFFER_BYTES);
    let (input_ended, end_of_input) = oneshot::channel();
    let forwarding = tokio::spawn(forward_standard_input(
        standard_input,
        forwarded_input,
        input_ended,
    ));
    let answering = tokio::spawn(answer_until_closed(
        Server::new(Arc::clone(&session), served, effects),
        server_input,
        standard_output,
    ));
    let exit = session
        .run_until_exit(end_of_input, signals, DEPARTURE_WAIT)
        .await;
    forwarding.abort();
    answering.abort();
    exit
}

/// Answers MCP requests from the input on the output until the client closes the input.
async fn answer_until_closed(
    server: Server,
    input: impl AsyncRead + Unpin + Send + 'static,
    output: impl AsyncWrite + Unpin + Send + 'static,
) {
    if let Ok(running) = serve_server(server, (input, output)).await {
        drop(running.waiting().await);
    }
}

/// The handler of the six tools for the one session of this process.
struct Server {
    session: Arc<Session>,
    claimed: Mutex<Option<ClaimedCard>>,
    served: ServedNetwork,
    effects: ServerEffects,
}

/// The card this session claimed and the effective handle its node assigned itself.
struct ClaimedCard {
    card: Handle,
    handle: LocalHandle,
}

impl Server {
    /// A server whose session has claimed no card yet.
    const fn new(session: Arc<Session>, served: ServedNetwork, effects: ServerEffects) -> Self {
        Self {
            session,
            claimed: Mutex::new(None),
            served,
            effects,
        }
    }

    /// The result of the named tool called with the arguments, or the error code it fails with.
    async fn answer(&self, tool: &str, arguments: JsonObject) -> Result<ToolResult, ErrorCode> {
        match ToolName::named(tool).ok_or(ErrorCode::BadRequest)? {
            ToolName::ListCards => {
                input_of::<NoInput>(arguments)?;
                Ok(self.list_cards())
            }
            ToolName::Init => self.init(input_of(arguments)?).await,
            ToolName::Peers => {
                input_of::<NoInput>(arguments)?;
                Ok(present_peers(&self.session.node()?.peers()))
            }
            ToolName::Send => self.send(input_of(arguments)?).await,
            ToolName::Broadcast => self.broadcast(input_of(arguments)?).await,
            ToolName::Inbox => self.inbox(input_of::<InboxInput>(arguments)?.since),
        }
    }

    /// Sends the body to the one member the input names, by the send algorithm of spec 3.10.
    async fn send(&self, input: SendInput) -> Result<ToolResult, ErrorCode> {
        let node = self.session.node()?;
        let delivery = node.send(&input.to.0, &input.body).await?;
        Ok(ToolResult::Sent(SentMessage::of(delivery)))
    }

    /// Sends the body to each member the input names, one result per member.
    async fn broadcast(&self, input: BroadcastInput) -> Result<ToolResult, ErrorCode> {
        let node = self.session.node()?;
        let handles: Vec<String> = input.to.into_iter().map(|recipient| recipient.0).collect();
        let results = node.broadcast(&handles, &input.body).await;
        Ok(ToolResult::Broadcast {
            results: results.into_iter().map(BroadcastOutcome::of).collect(),
        })
    }

    /// The inbox frames with seq greater than `since`, with the newest seq.
    fn inbox(&self, since: u64) -> Result<ToolResult, ErrorCode> {
        self.session.node()?;
        let (messages, last_seq) = self.effects.node.inbox.frames_since(since);
        Ok(ToolResult::Inbox { messages, last_seq })
    }

    /// Claims the input's card for this session by the init sequence of spec 3.10 and returns its effective handle and inbox path.
    async fn init(&self, input: InitInput) -> Result<ToolResult, ErrorCode> {
        self.refuse_a_second_claim()?;
        let name = card_name(&input.card)?;
        let card = self.registry_card(&name)?;
        let requested_handle = requested_handle_of(&card)?;
        let public_key = public_key_of(&card)?;
        self.refuse_another_network(&card)?;
        let public = public_with_description(card, input.description.as_deref())?;
        let lock = self.lock_sidecar(&name, public_key).await?;
        let node = self.start_node(&lock, requested_handle, public).await?;
        let node = self.session.claim(node, lock)?;
        Ok(self.record_claim(name, node.own_handle().clone()))
    }

    /// `bad_request` once this session has claimed a card, and the session's refusal while it is stopping.
    fn refuse_a_second_claim(&self) -> Result<(), ErrorCode> {
        self.session.node().map_or_else(
            |refusal| {
                if refusal == ErrorCode::NotClaimed {
                    Ok(())
                } else {
                    Err(refusal)
                }
            },
            |_claimed| Err(ErrorCode::BadRequest),
        )
    }

    /// The registry's card of that name, `card_not_found` when it has none and `card_invalid` when its file is unreadable or no card.
    fn registry_card(&self, name: &Handle) -> Result<Card, ErrorCode> {
        let card_path = self
            .served
            .directories
            .card_file(self.served.network.as_str(), name.as_str())
            .map_err(|_not_a_card_name| ErrorCode::CardNotFound)?;
        let text = self
            .effects
            .registry
            .read_text(&card_path)
            .map_err(|failure| {
                if failure.kind() == io::ErrorKind::NotFound {
                    ErrorCode::CardNotFound
                } else {
                    ErrorCode::CardInvalid
                }
            })?;
        text.parse().map_err(|_invalid| ErrorCode::CardInvalid)
    }

    /// `card_invalid` for a card that names a network other than the one this process serves.
    fn refuse_another_network(&self, card: &Card) -> Result<(), ErrorCode> {
        match &card.network {
            Some(network) if network != self.served.network.as_str() => Err(ErrorCode::CardInvalid),
            Some(_) | None => Ok(()),
        }
    }

    /// Validates the card's sidecar and takes its lock within the lock retry window, off the runtime's threads.
    async fn lock_sidecar(
        &self,
        name: &Handle,
        public_key: PublicKey,
    ) -> Result<HeldSidecar, ErrorCode> {
        let sidecar_path = self
            .served
            .directories
            .card_sidecar_file(self.served.network.as_str(), name.as_str())
            .map_err(|_not_a_card_name| ErrorCode::CardNotFound)?;
        let pause = PauseBetweenLockTries(Arc::clone(&self.effects.pause_between_lock_tries));
        tokio::task::spawn_blocking(move || {
            sidecar::claim(&sidecar_path, public_key.as_bytes(), &pause)
        })
        .await
        .map_err(|_lock_task_failed| ErrorCode::SidecarInvalid)?
        .map_err(|refusal| claim_refusal(&refusal))
    }

    /// Builds the endpoint under the card key, starts the node on it, and returns once its effective handle is assigned.
    async fn start_node(
        &self,
        lock: &HeldSidecar,
        requested_handle: AnnouncedHandle,
        public: Vec<u8>,
    ) -> Result<Node, ErrorCode> {
        let network = Network::new(self.served.config.clone(), self.served.config_path.clone())
            .map_err(|_refused| NODE_NOT_STARTED)?;
        let endpoint = (self.effects.bind_endpoint)(
            lock.secret_key().clone(),
            *network.keys().gate_key(),
            known_host_relays(&self.served.config),
        )
        .await
        .map_err(|_unbound| NODE_NOT_STARTED)?;
        let identity = Identity {
            card_key: lock.secret_key().clone(),
            requested_handle,
            public,
        };
        Node::start(
            endpoint,
            identity,
            network,
            node_effects_of(&self.effects.node),
        )
        .await
        .map_err(|_not_started| NODE_NOT_STARTED)
    }

    /// Remembers the claimed card and its effective handle for every later `you`, and returns the init result.
    fn record_claim(&self, card: Handle, handle: LocalHandle) -> ToolResult {
        let claimed = ClaimedCard { card, handle };
        let result = ToolResult::Claimed {
            handle: claimed.handle.as_str().to_owned(),
            inbox_path: self.served.inbox_path.clone(),
        };
        *self.claimed_card() = Some(claimed);
        result
    }

    /// The card this session claimed, none before init.
    fn claimed_card(&self) -> MutexGuard<'_, Option<ClaimedCard>> {
        self.claimed.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Every card of the registry in name order, each with whether its sidecar lock is taken.
    fn list_cards(&self) -> ToolResult {
        let mut cards: Vec<ListedCard> = self
            .registry_cards()
            .into_iter()
            .map(|(name, card)| ListedCard {
                held: self.is_held(&name),
                name: name.as_str().to_owned(),
                handle: card.handle,
            })
            .collect();
        cards.sort_by(|first, second| first.name.cmp(&second.name));
        ToolResult::Cards { cards }
    }

    /// The cards of this network's registry whose name and text are valid, an empty list when the registry cannot be read.
    fn registry_cards(&self) -> Vec<(Handle, Card)> {
        let Ok(cards_directory) = self
            .served
            .directories
            .cards_directory(self.served.network.as_str())
        else {
            return Vec::new();
        };
        let Ok(entries) = self.effects.registry.read_directory(&cards_directory) else {
            return Vec::new();
        };
        entries
            .iter()
            .filter_map(|path| self.registry_card_at(path))
            .collect()
    }

    /// The card at the path with its name, when the path is a card file whose stem is a card name and whose text is a card.
    fn registry_card_at(&self, path: &Path) -> Option<(Handle, Card)> {
        if path.extension()? != CARD_EXTENSION {
            return None;
        }
        let name = path.file_stem()?.to_str()?.parse().ok()?;
        let card = self.effects.registry.read_text(path).ok()?.parse().ok()?;
        Some((name, card))
    }

    /// Whether the card's sidecar lock is taken: held without a try for the card this session claimed, otherwise tried and released at once.
    fn is_held(&self, card: &Handle) -> bool {
        self.is_claimed_here(card) || self.is_locked_elsewhere(card)
    }

    /// Whether this session claimed the card.
    fn is_claimed_here(&self, card: &Handle) -> bool {
        self.claimed_card()
            .as_ref()
            .is_some_and(|claimed| &claimed.card == card)
    }

    /// Whether trying the card's sidecar lock finds it taken, releasing it at once.
    fn is_locked_elsewhere(&self, card: &Handle) -> bool {
        self.served
            .directories
            .card_sidecar_file(self.served.network.as_str(), card.as_str())
            .is_ok_and(|sidecar_path| sidecar::is_held(&sidecar_path).is_ok_and(|held| held))
    }

    /// The caller as this session knows it now.
    fn you(&self) -> You {
        self.claimed_card()
            .as_ref()
            .map_or(You::Unclaimed, |claimed| You::Claimed {
                card: claimed.card.as_str().to_owned(),
                handle: claimed.handle.as_str().to_owned(),
            })
    }

    /// The envelope that carries the outcome and names the caller.
    fn envelope_of(
        &self,
        outcome: Result<ToolResult, ErrorCode>,
    ) -> Result<Envelope, serde_json::Error> {
        let you = self.you();
        Ok(match outcome {
            Ok(result) => Envelope::Success {
                you,
                result: serde_json::to_value(result)?,
            },
            Err(error) => Envelope::Failure { you, error },
        })
    }
}

impl ServerHandler for Server {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(SERVER_NAME, env!("CARGO_PKG_VERSION")))
    }

    fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListToolsResult, ErrorData>> + MaybeSendFuture + '_ {
        std::future::ready(Ok(ListToolsResult::with_all_items(
            ToolName::ALL.map(ToolName::tool).to_vec(),
        )))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let outcome = self
            .answer(&request.name, request.arguments.unwrap_or_default())
            .await;
        let envelope = self
            .envelope_of(outcome)
            .map_err(|error| unserializable(&error))?;
        tool_result(&envelope).map(CallToolResponse::from)
    }
}

/// The tool result of spec 3.10: the envelope as text and as structured content, an error exactly when the envelope is not ok.
fn tool_result(envelope: &Envelope) -> Result<CallToolResult, ErrorData> {
    let structured = serde_json::to_value(envelope).map_err(|error| unserializable(&error))?;
    Ok(match envelope {
        Envelope::Success { .. } => CallToolResult::structured(structured),
        Envelope::Failure { .. } => CallToolResult::structured_error(structured),
    })
}

/// The library's internal error for a result that cannot be rendered as JSON.
fn unserializable(error: &serde_json::Error) -> ErrorData {
    ErrorData::internal_error(error.to_string(), None)
}

/// The relays of the configuration's hosts, which the endpoint's relay map starts from (spec 3.8).
fn known_host_relays(config: &NetworkConfig) -> Vec<RelayUrl> {
    config
        .hosts
        .iter()
        .filter_map(|host| host.relay.parse().ok())
        .collect()
}

/// The node's side effects for this init, sharing every one the composition root handed the server.
fn node_effects_of(effects: &NodeEffects) -> NodeEffects {
    NodeEffects {
        clock: Arc::clone(&effects.clock),
        monotonic: Arc::clone(&effects.monotonic),
        fill_probe_nonce: Arc::clone(&effects.fill_probe_nonce),
        report: Arc::clone(&effects.report),
        inbox: Arc::clone(&effects.inbox),
        probe_relay: Arc::clone(&effects.probe_relay),
    }
}

/// The error code of a claim whose network, endpoint or node did not start after the lock was taken.
const NODE_NOT_STARTED: ErrorCode = ErrorCode::Unreachable;

/// The tool's arguments read as its input, `bad_request` when they do not match its schema.
fn input_of<Input: DeserializeOwned>(arguments: JsonObject) -> Result<Input, ErrorCode> {
    serde_json::from_value(Value::Object(arguments)).map_err(|_mismatch| ErrorCode::BadRequest)
}

/// The card name the input gives, `card_not_found` for a name no registry can hold.
fn card_name(card: &str) -> Result<Handle, ErrorCode> {
    card.parse()
        .map_err(|_not_a_card_name| ErrorCode::CardNotFound)
}

/// The handle the card requests, `card_invalid` when it is not a handle.
fn requested_handle_of(card: &Card) -> Result<AnnouncedHandle, ErrorCode> {
    let handle: Handle = card
        .handle
        .parse()
        .map_err(|_not_a_handle| ErrorCode::CardInvalid)?;
    handle
        .as_str()
        .parse()
        .map_err(|_not_announceable| ErrorCode::CardInvalid)
}

/// The card's public key, `card_invalid` when it does not decode to an Ed25519 key.
fn public_key_of(card: &Card) -> Result<PublicKey, ErrorCode> {
    parse_public_key(&card.public_key).map_err(|_not_a_key| ErrorCode::CardInvalid)
}

/// The card's public mapping as the bytes Presence carries, with the description under `public.description`, `bad_request` past the cap.
fn public_with_description(card: Card, description: Option<&str>) -> Result<Vec<u8>, ErrorCode> {
    match description {
        None => Ok(card.public_json()),
        Some(description) => card
            .with_description(description)
            .map(|described| described.public_json())
            .map_err(|_over_cap| ErrorCode::BadRequest),
    }
}

/// The error code of a sidecar that could not be claimed: `card_held` for a lock still taken, `sidecar_invalid` for anything else.
const fn claim_refusal(refusal: &SidecarError) -> ErrorCode {
    match refusal {
        SidecarError::CardHeld => ErrorCode::CardHeld,
        SidecarError::Invalid(_) | SidecarError::AlreadyExists | SidecarError::Io(_) => {
            ErrorCode::SidecarInvalid
        }
    }
}

/// The pause between lock tries, handed in by the composition root.
struct PauseBetweenLockTries(Arc<dyn Fn(Duration) + Send + Sync>);

impl Pause for PauseBetweenLockTries {
    fn pause(&self, duration: Duration) {
        (self.0)(duration);
    }
}

/// The arguments of a tool that takes none.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NoInput {}

/// The arguments of `init`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InitInput {
    card: String,
    description: Option<String>,
}

/// The arguments of `send`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SendInput {
    to: Recipient,
    body: String,
}

/// The arguments of `broadcast`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BroadcastInput {
    to: Vec<Recipient>,
    body: String,
}

/// A `to` handle no longer than any local name of spec 3.1.
#[derive(Deserialize)]
#[serde(try_from = "String")]
struct Recipient(String);

impl TryFrom<String> for Recipient {
    type Error = RecipientTooLong;

    fn try_from(handle: String) -> Result<Self, RecipientTooLong> {
        if handle.chars().count() > LOCAL_HANDLE_MAX_CHARACTERS {
            Err(RecipientTooLong)
        } else {
            Ok(Self(handle))
        }
    }
}

/// The refusal of a `to` longer than any local name, read as `bad_request`.
#[derive(Debug)]
struct RecipientTooLong;

impl fmt::Display for RecipientTooLong {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "a handle is at most {LOCAL_HANDLE_MAX_CHARACTERS} characters"
        )
    }
}

/// The arguments of `inbox`, reading from the first frame when `since` is absent.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InboxInput {
    #[serde(default)]
    since: u64,
}

/// What a tool returns inside a successful envelope.
#[derive(Serialize)]
#[serde(untagged)]
enum ToolResult {
    Cards {
        cards: Vec<ListedCard>,
    },
    Claimed {
        handle: String,
        inbox_path: PathBuf,
    },
    Peers {
        peers: Vec<PresentPeer>,
    },
    Sent(SentMessage),
    Broadcast {
        results: Vec<BroadcastOutcome>,
    },
    Inbox {
        messages: Vec<InboxFrame>,
        last_seq: u64,
    },
}

/// A card as `list_cards` lists it.
#[derive(Serialize)]
struct ListedCard {
    name: String,
    handle: String,
    held: bool,
}

/// A peer as the peers tool lists it.
#[derive(Serialize)]
struct PresentPeer {
    handle: String,
    status: &'static str,
    attached: bool,
    public: JsonObject,
    last_seen: u64,
}

impl PresentPeer {
    /// The peer as the peers tool lists it, with an empty `public` until a valid mapping arrives.
    fn of(peer: &Peer) -> Self {
        Self {
            handle: peer.effective_handle.as_str().to_owned(),
            status: status_name(peer.status),
            attached: peer.attached,
            public: peer
                .public
                .as_deref()
                .and_then(|bytes| serde_json::from_slice(bytes).ok())
                .unwrap_or_default(),
            last_seen: peer.last_seen,
        }
    }
}

/// The word the peers tool reports a status by.
const fn status_name(status: Status) -> &'static str {
    match status {
        Status::Online => "online",
        Status::Unreachable => "unreachable",
        Status::Gone { .. } => "gone",
    }
}

/// The result of a message the recipient acknowledged.
#[derive(Serialize)]
struct SentMessage {
    id: String,
    delivered: bool,
    attached: bool,
}

impl SentMessage {
    /// The send result of a delivered message.
    fn of(delivery: Delivery) -> Self {
        Self {
            id: delivery.id,
            delivered: true,
            attached: delivery.attached,
        }
    }
}

/// One member's result in a broadcast.
#[derive(Serialize)]
#[serde(untagged)]
enum BroadcastOutcome {
    Delivered {
        to: String,
        #[serde(flatten)]
        sent: SentMessage,
    },
    Failed {
        to: String,
        delivered: bool,
        error: ErrorCode,
    },
}

impl BroadcastOutcome {
    /// The broadcast result of one handle.
    fn of(result: BroadcastResult) -> Self {
        match result.outcome {
            Ok(delivery) => Self::Delivered {
                to: result.to,
                sent: SentMessage::of(delivery),
            },
            Err(error) => Self::Failed {
                to: result.to,
                delivered: false,
                error,
            },
        }
    }
}

/// The peers result listing every other member this node knows.
fn present_peers(peers: &[Peer]) -> ToolResult {
    ToolResult::Peers {
        peers: peers.iter().map(PresentPeer::of).collect(),
    }
}

/// The six tools of spec 3.10, the same before and after init.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToolName {
    ListCards,
    Init,
    Peers,
    Send,
    Broadcast,
    Inbox,
}

impl ToolName {
    const ALL: [Self; TOOL_COUNT] = [
        Self::ListCards,
        Self::Init,
        Self::Peers,
        Self::Send,
        Self::Broadcast,
        Self::Inbox,
    ];

    /// The tool a client calls by this name, if any.
    fn named(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|tool| tool.name() == name)
    }

    /// The name a client calls the tool by.
    const fn name(self) -> &'static str {
        match self {
            Self::ListCards => "list_cards",
            Self::Init => "init",
            Self::Peers => "peers",
            Self::Send => "send",
            Self::Broadcast => "broadcast",
            Self::Inbox => "inbox",
        }
    }

    /// The description given to the harness, exactly as spec 3.10 words it.
    const fn description(self) -> &'static str {
        match self {
            Self::ListCards => {
                "List the identity cards on this machine and whether each is in use."
            }
            Self::Init => {
                "Claim one card for this session. Returns the handle you must use. Call once."
            }
            Self::Peers => {
                "List the members that are present, with their handles and descriptions."
            }
            Self::Send => {
                "Send a text message to one member by handle. Returns whether it was delivered."
            }
            Self::Broadcast => {
                "Send one text message to several members by handle. Returns one result per member."
            }
            Self::Inbox => {
                "Read messages that arrived for you. Use `since` to read only newer ones."
            }
        }
    }

    /// The JSON schema of the tool's arguments, refusing every key it does not name.
    fn input_schema(self) -> JsonObject {
        let recipient = json!({ "type": "string", "maxLength": LOCAL_HANDLE_MAX_CHARACTERS });
        let text = json!({ "type": "string" });
        match self {
            Self::ListCards | Self::Peers => object_schema(json!({}), &[]),
            Self::Init => object_schema(json!({ "card": text, "description": text }), &["card"]),
            Self::Send => object_schema(json!({ "to": recipient, "body": text }), &["to", "body"]),
            Self::Broadcast => object_schema(
                json!({ "to": { "type": "array", "items": recipient }, "body": text }),
                &["to", "body"],
            ),
            Self::Inbox => {
                object_schema(json!({ "since": { "type": "integer", "minimum": 0 } }), &[])
            }
        }
    }

    /// The tool as the tool list advertises it.
    fn tool(self) -> Tool {
        Tool::new(self.name(), self.description(), self.input_schema())
    }
}

/// The schema of an object with these properties, these required, and no other key.
fn object_schema(properties: Value, required: &[&str]) -> JsonObject {
    let mut schema = JsonObject::from_iter([
        (SCHEMA_TYPE_KEY.to_owned(), Value::from(SCHEMA_OBJECT_TYPE)),
        (SCHEMA_PROPERTIES_KEY.to_owned(), properties),
    ]);
    if !required.is_empty() {
        schema.insert(
            SCHEMA_REQUIRED_KEY.to_owned(),
            Value::from(required.to_vec()),
        );
    }
    schema.insert(
        SCHEMA_ADDITIONAL_PROPERTIES_KEY.to_owned(),
        Value::Bool(false),
    );
    schema
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::fs;
    use std::io;
    use std::os::unix::fs::PermissionsExt;
    use std::time::{Instant, SystemTime, UNIX_EPOCH};

    use serde_json::{Value, json};
    use tokio::io::{
        AsyncBufReadExt, AsyncWriteExt, BufReader, ReadHalf, SimplexStream, WriteHalf,
    };
    use tokio::task::JoinHandle;

    use super::*;
    use crate::config::{Host, HostOrigin};
    use crate::dirs::OperatingSystemFilesystem;
    use crate::inbox::socket::{InboxSocket, ProcessTable};
    use crate::keys::render_public_key;
    use crate::node::{NodeRecord, RuntimeShutdown};
    use crate::sidecar;

    const NETWORK: &str = "acme";
    const NETWORK_SECRET_BYTE: u8 = 44;
    const OWNER_SEED: u8 = 45;
    const KEY_BYTES: usize = 32;
    const GATE_NONCE_BYTE: u8 = 46;
    const PIPE_BYTES: usize = 1 << 20;
    const RESPONSE_WAIT: Duration = Duration::from_secs(20);
    const CLIENT_PROTOCOL_VERSION: &str = "2025-06-18";
    const FIRST_PROCESS_ID: u32 = 41;
    const SECOND_PROCESS_ID: u32 = 42;
    const UNREACHABLE_RELAY: &str = "https://relay.estafette.invalid/";
    const BOB_DESCRIPTION: &str = "reviews the parser";
    const MESSAGE_BODY: &str = "the parser review is ready";
    const PEER_LISTED_WITHIN: Duration = Duration::from_millis(3000);
    const PEER_POLL: Duration = Duration::from_millis(50);
    const AMY_SEED: u8 = 51;
    const BOB_SEED: u8 = 52;
    const ZED_SEED: u8 = 53;
    const LOOSE_SIDECAR_MODE: u32 = 0o644;
    const PUBLIC_MAX_BYTES: usize = 3072;
    const SPEC_JOIN_TIMEOUT_PLUS_OBSERVER_WAIT: Duration = Duration::from_millis(8000);
    const NODE_TESTS_AT_ONCE: usize = 1;

    static NODE_TEST_SLOTS: tokio::sync::Semaphore =
        tokio::sync::Semaphore::const_new(NODE_TESTS_AT_ONCE);

    async fn node_slot() -> tokio::sync::SemaphorePermit<'static> {
        NODE_TEST_SLOTS.acquire().await.unwrap()
    }

    struct EveryProcessRuns;

    impl ProcessTable for EveryProcessRuns {
        fn is_running(&self, _process_id: u32) -> io::Result<bool> {
            Ok(true)
        }
    }

    struct Machine {
        root: PathBuf,
    }

    impl Machine {
        fn new(tag: &str) -> Self {
            let root = std::env::temp_dir().join(format!("u24-{tag}-{}", std::process::id()));
            drop(fs::remove_dir_all(&root));
            fs::create_dir_all(&root).unwrap();
            Self { root }
        }

        fn directories(&self) -> Directories {
            let base = self.root.clone();
            let environment = move |name: &str| match name {
                "XDG_CONFIG_HOME" => Some(base.join("c").into_os_string()),
                "XDG_DATA_HOME" => Some(base.join("d").into_os_string()),
                "XDG_RUNTIME_DIR" => Some(base.join("r").into_os_string()),
                _ => None::<OsString>,
            };
            Directories::establish(environment, &OperatingSystemFilesystem, None).unwrap()
        }

        fn create_card(&self, name: &str, handle: &str, seed: u8) {
            let directories = self.directories();
            fs::create_dir_all(directories.cards_directory(NETWORK).unwrap()).unwrap();
            sidecar::create_card(
                &directories.card_file(NETWORK, name).unwrap(),
                &directories.card_sidecar_file(NETWORK, name).unwrap(),
                handle,
                [seed; KEY_BYTES],
            )
            .unwrap();
        }

        fn write_card_text(&self, name: &str, text: &str) {
            fs::write(self.directories().card_file(NETWORK, name).unwrap(), text).unwrap();
        }

        fn sidecar_path(&self, name: &str) -> PathBuf {
            self.directories().card_sidecar_file(NETWORK, name).unwrap()
        }

        fn hold_card(&self, name: &str, seed: u8) -> sidecar::HeldSidecar {
            sidecar::claim(
                &self.sidecar_path(name),
                SecretKey::from_bytes(&[seed; KEY_BYTES])
                    .public()
                    .as_bytes(),
                &sidecar::ThreadSleep,
            )
            .unwrap()
        }
    }

    impl Drop for Machine {
        fn drop(&mut self) {
            drop(fs::remove_dir_all(&self.root));
        }
    }

    fn wall_clock_ms() -> u64 {
        u64::try_from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_millis(),
        )
        .unwrap()
    }

    fn network_config(hosts: Vec<Host>) -> NetworkConfig {
        NetworkConfig {
            secret: data_encoding::HEXLOWER.encode(&[NETWORK_SECRET_BYTE; KEY_BYTES]),
            owner: render_public_key(&SecretKey::from_bytes(&[OWNER_SEED; KEY_BYTES]).public()),
            hosts,
        }
    }

    fn bind_alone() -> BindEndpoint {
        Arc::new(|card_key, gate_key, _relays| {
            Box::pin(NodeEndpoint::bind(
                card_key,
                gate_key,
                Vec::new(),
                |nonce: &mut [u8; KEY_BYTES]| *nonce = [GATE_NONCE_BYTE; KEY_BYTES],
                |_closing| {},
            ))
        })
    }

    struct ServerPlan {
        process_id: u32,
        hosts: Vec<Host>,
        bind_endpoint: BindEndpoint,
    }

    impl ServerPlan {
        fn alone(process_id: u32) -> Self {
            Self {
                process_id,
                hosts: Vec::new(),
                bind_endpoint: bind_alone(),
            }
        }

        fn meeting(process_id: u32, other_seed: u8, meeting: &Arc<Meeting>) -> Self {
            Self {
                process_id,
                hosts: vec![Host {
                    id: render_public_key(
                        &SecretKey::from_bytes(&[other_seed; KEY_BYTES]).public(),
                    ),
                    relay: UNREACHABLE_RELAY.to_owned(),
                    ts: None,
                    origin: HostOrigin::Configured,
                }],
                bind_endpoint: bind_meeting(meeting),
            }
        }
    }

    struct Meeting {
        addresses: std::sync::Mutex<Vec<iroh::EndpointAddr>>,
        everyone_bound: tokio::sync::Barrier,
    }

    impl Meeting {
        fn of(members: usize) -> Arc<Self> {
            Arc::new(Self {
                addresses: std::sync::Mutex::new(Vec::new()),
                everyone_bound: tokio::sync::Barrier::new(members),
            })
        }
    }

    fn bind_meeting(meeting: &Arc<Meeting>) -> BindEndpoint {
        let meeting = Arc::clone(meeting);
        Arc::new(move |card_key, gate_key, _relays| {
            let meeting = Arc::clone(&meeting);
            Box::pin(async move {
                let endpoint = NodeEndpoint::bind(
                    card_key,
                    gate_key,
                    Vec::new(),
                    |nonce: &mut [u8; KEY_BYTES]| *nonce = [GATE_NONCE_BYTE; KEY_BYTES],
                    |_closing| {},
                )
                .await?;
                meeting
                    .addresses
                    .lock()
                    .unwrap()
                    .push(endpoint.endpoint().addr());
                meeting.everyone_bound.wait().await;
                let others = meeting.addresses.lock().unwrap().clone();
                for address in others {
                    if address.id != endpoint.endpoint().id() {
                        endpoint.replace_address(address);
                    }
                }
                Ok(endpoint)
            })
        })
    }

    async fn claimed_pair(machine: &Machine) -> (Client, Client) {
        machine.create_card("amy-card", "amy", AMY_SEED);
        machine.create_card("bob-card", "bob", BOB_SEED);
        let meeting = Meeting::of(2);
        let mut amy = start_server(
            machine,
            ServerPlan::meeting(FIRST_PROCESS_ID, BOB_SEED, &meeting),
        );
        let mut bob = start_server(
            machine,
            ServerPlan::meeting(SECOND_PROCESS_ID, AMY_SEED, &meeting),
        );
        amy.initialize().await;
        bob.initialize().await;
        let (amy_claim, bob_claim) = tokio::join!(
            amy.envelope("init", json!({ "card": "amy-card" })),
            bob.envelope(
                "init",
                json!({ "card": "bob-card", "description": BOB_DESCRIPTION })
            ),
        );
        assert_eq!(amy_claim["ok"], json!(true), "{amy_claim}");
        assert_eq!(bob_claim["ok"], json!(true), "{bob_claim}");
        amy.peer_listed_within("bob", PEER_LISTED_WITHIN).await;
        bob.peer_listed_within("amy", PEER_LISTED_WITHIN).await;
        (amy, bob)
    }

    struct Client {
        to_server: WriteHalf<SimplexStream>,
        from_server: BufReader<ReadHalf<SimplexStream>>,
        next_id: u64,
        signals: mpsc::UnboundedSender<ShutdownSignal>,
        exit: JoinHandle<Exit>,
    }

    fn start_server(machine: &Machine, plan: ServerPlan) -> Client {
        let directories = machine.directories();
        let inbox =
            Arc::new(InboxSocket::start(&directories, plan.process_id, &EveryProcessRuns).unwrap());
        let inbox_path = directories.inbox_socket_file(plan.process_id);
        let message_id_byte = u8::try_from(plan.process_id % 256).unwrap();
        let served = ServedNetwork {
            config_path: machine
                .root
                .join(format!("{}-network.toml", plan.process_id)),
            directories,
            network: NETWORK.parse().unwrap(),
            config: network_config(plan.hosts),
            inbox_path,
        };
        let effects = ServerEffects {
            node: NodeEffects {
                clock: Arc::new(wall_clock_ms),
                monotonic: Arc::new(tokio::time::Instant::now),
                fill_probe_nonce: Arc::new(move |nonce: &mut crate::wire::Nonce| {
                    *nonce = [message_id_byte; 16];
                }),
                report: Arc::new(|_record: NodeRecord| {}),
                inbox,
                probe_relay: Arc::new(|_relay| Box::pin(async { true })),
            },
            bind_endpoint: plan.bind_endpoint,
            registry: Arc::new(OperatingSystemFilesystem),
            pause_between_lock_tries: Arc::new(std::thread::sleep),
        };
        let (server_input, to_server) = tokio::io::simplex(PIPE_BYTES);
        let (from_server, server_output) = tokio::io::simplex(PIPE_BYTES);
        let (signals, signals_received) = mpsc::unbounded_channel();
        let exit = tokio::spawn(serve(
            server_input,
            server_output,
            signals_received,
            served,
            effects,
        ));
        Client {
            to_server,
            from_server: BufReader::new(from_server),
            next_id: 1,
            signals,
            exit,
        }
    }

    impl Client {
        async fn write_message(&mut self, message: &Value) {
            let mut line = serde_json::to_vec(message).unwrap();
            line.push(b'\n');
            self.to_server.write_all(&line).await.unwrap();
        }

        async fn request(&mut self, method: &str, params: Value) -> Value {
            let id = self.next_id;
            self.next_id = self.next_id.checked_add(1).unwrap();
            self.write_message(
                &json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }),
            )
            .await;
            loop {
                let mut line = String::new();
                tokio::time::timeout(RESPONSE_WAIT, self.from_server.read_line(&mut line))
                    .await
                    .unwrap()
                    .unwrap();
                let message: Value = serde_json::from_str(&line).unwrap();
                if message["id"] == json!(id) {
                    return message;
                }
            }
        }

        async fn initialize(&mut self) -> Value {
            let response = self
                .request(
                    "initialize",
                    json!({
                        "protocolVersion": CLIENT_PROTOCOL_VERSION,
                        "capabilities": {},
                        "clientInfo": { "name": "u24-test", "version": "0" }
                    }),
                )
                .await;
            self.write_message(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }))
                .await;
            response
        }

        async fn call(&mut self, tool: &str, arguments: Value) -> Value {
            self.request(
                "tools/call",
                json!({ "name": tool, "arguments": arguments }),
            )
            .await["result"]
                .clone()
        }

        async fn envelope(&mut self, tool: &str, arguments: Value) -> Value {
            self.call(tool, arguments).await["structuredContent"].clone()
        }

        async fn peer_listed_within(&mut self, handle: &str, within: Duration) -> Value {
            let deadline = Instant::now().checked_add(within).unwrap();
            loop {
                let listed = self.envelope("peers", json!({})).await;
                let found = listed["result"]["peers"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|peer| peer["handle"] == handle)
                    .cloned();
                if let Some(peer) = found {
                    return peer;
                }
                assert!(Instant::now() < deadline, "{handle} not listed: {listed}");
                tokio::time::sleep(PEER_POLL).await;
            }
        }
    }

    fn unclaimed() -> Value {
        json!({ "card": null, "handle": null, "status": "unclaimed" })
    }

    #[tokio::test]
    async fn spec_3_9_the_end_of_standard_input_ends_the_session_with_its_runtime_awaited() {
        let machine = Machine::new("input-ends");
        let mut client = start_server(&machine, ServerPlan::alone(FIRST_PROCESS_ID));
        client.initialize().await;

        client.to_server.shutdown().await.unwrap();
        let exit = tokio::time::timeout(RESPONSE_WAIT, client.exit).await;

        assert_eq!(
            exit.unwrap().unwrap(),
            Exit::Stopped(RuntimeShutdown::Awaited)
        );
    }

    #[tokio::test]
    async fn spec_3_9_sigterm_ends_the_session_with_its_runtime_shut_down_in_the_background() {
        let machine = Machine::new("sigterm");
        let mut client = start_server(&machine, ServerPlan::alone(FIRST_PROCESS_ID));
        client.initialize().await;

        client.signals.send(ShutdownSignal::Terminate).unwrap();
        let exit = tokio::time::timeout(RESPONSE_WAIT, client.exit).await;

        assert_eq!(
            exit.unwrap().unwrap(),
            Exit::Stopped(RuntimeShutdown::InBackground)
        );
    }

    #[tokio::test]
    async fn spec_3_10_a_tool_result_carries_the_envelope_as_text_and_as_structured_content() {
        let machine = Machine::new("result-form");
        let mut client = start_server(&machine, ServerPlan::alone(FIRST_PROCESS_ID));
        client.initialize().await;

        let result = client.call("peers", json!({})).await;

        let expected = json!({
            "ok": false,
            "you": unclaimed(),
            "error": { "code": "not_claimed", "message": "Claim a card with init first." }
        });
        assert_eq!(result["structuredContent"], expected);
        let text = result["content"][0]["text"].as_str().unwrap();
        assert_eq!(serde_json::from_str::<Value>(text).unwrap(), expected);
        assert_eq!(result["isError"], json!(true));
    }

    #[tokio::test]
    async fn spec_3_10_before_init_invalid_input_is_bad_request_and_valid_input_is_not_claimed() {
        let machine = Machine::new("before-init");
        let mut client = start_server(&machine, ServerPlan::alone(FIRST_PROCESS_ID));
        client.initialize().await;
        let cases = [
            ("peers", json!({}), json!({ "verbose": true })),
            (
                "send",
                json!({ "to": "bob", "body": "hi" }),
                json!({ "to": "bob" }),
            ),
            (
                "broadcast",
                json!({ "to": ["bob", "cal"], "body": "hi" }),
                json!({ "to": "bob", "body": "hi" }),
            ),
            ("inbox", json!({ "since": 3 }), json!({ "since": "3" })),
            ("inbox", json!({}), json!({ "since": 1.5 })),
        ];

        for (tool, valid, invalid) in cases {
            let refused = client.envelope(tool, invalid).await;
            let unclaimed_answer = client.envelope(tool, valid).await;

            assert_eq!(refused["error"]["code"], "bad_request", "{tool}");
            assert_eq!(refused["you"], unclaimed(), "{tool}");
            assert_eq!(unclaimed_answer["error"]["code"], "not_claimed", "{tool}");
            assert_eq!(unclaimed_answer["you"], unclaimed(), "{tool}");
        }
    }

    #[tokio::test]
    async fn spec_3_10_an_inbox_since_below_zero_is_bad_request() {
        let machine = Machine::new("since");
        let mut client = start_server(&machine, ServerPlan::alone(FIRST_PROCESS_ID));
        client.initialize().await;

        let refused = client.envelope("inbox", json!({ "since": -1 })).await;

        assert_eq!(refused["ok"], json!(false));
        assert_eq!(refused["error"]["code"], "bad_request");
    }

    #[tokio::test]
    async fn spec_3_10_an_unknown_tool_gets_the_bad_request_envelope_with_you() {
        let machine = Machine::new("unknown-tool");
        let mut client = start_server(&machine, ServerPlan::alone(FIRST_PROCESS_ID));
        client.initialize().await;

        let result = client.call("delete_everything", json!({})).await;

        assert_eq!(
            result["structuredContent"],
            json!({
                "ok": false,
                "you": unclaimed(),
                "error": { "code": "bad_request", "message": "The input did not match the tool schema." }
            })
        );
        assert_eq!(result["isError"], json!(true));
    }

    #[tokio::test]
    async fn spec_3_1_a_to_longer_than_74_characters_is_bad_request_and_74_is_not() {
        let machine = Machine::new("to-length");
        let mut client = start_server(&machine, ServerPlan::alone(FIRST_PROCESS_ID));
        client.initialize().await;
        let longest = format!("a{}", "b".repeat(73));
        let too_long = format!("{longest}2");

        let sent_too_long = client
            .envelope("send", json!({ "to": too_long, "body": "hi" }))
            .await;
        let broadcast_too_long = client
            .envelope(
                "broadcast",
                json!({ "to": ["bob", too_long], "body": "hi" }),
            )
            .await;
        let sent_longest = client
            .envelope("send", json!({ "to": longest, "body": "hi" }))
            .await;

        assert_eq!(longest.len(), 74);
        assert_eq!(sent_too_long["error"]["code"], "bad_request");
        assert_eq!(broadcast_too_long["error"]["code"], "bad_request");
        assert_eq!(sent_longest["error"]["code"], "not_claimed");
    }

    #[tokio::test]
    async fn spec_3_10_arguments_that_are_not_an_object_get_the_library_error_and_no_envelope() {
        let machine = Machine::new("non-object");
        let mut client = start_server(&machine, ServerPlan::alone(FIRST_PROCESS_ID));
        client.initialize().await;

        let response = client
            .request(
                "tools/call",
                json!({ "name": "peers", "arguments": ["not", "an", "object"] }),
            )
            .await;

        assert_eq!(response.get("result"), None);
        assert_eq!(response["error"]["code"], json!(-32601));
    }

    #[tokio::test]
    async fn spec_3_10_list_cards_names_each_card_with_its_handle_and_whether_its_lock_is_taken() {
        let machine = Machine::new("list-cards");
        machine.create_card("zed-card", "zed", ZED_SEED);
        machine.create_card("amy-card", "amy", AMY_SEED);
        machine.create_card("bob-card", "bob", BOB_SEED);
        let _held_by_another_process = machine.hold_card("bob-card", BOB_SEED);
        let mut client = start_server(&machine, ServerPlan::alone(FIRST_PROCESS_ID));
        client.initialize().await;

        let listed = client.envelope("list_cards", json!({})).await;

        assert_eq!(
            listed,
            json!({
                "ok": true,
                "you": unclaimed(),
                "result": { "cards": [
                    { "name": "amy-card", "handle": "amy", "held": false },
                    { "name": "bob-card", "handle": "bob", "held": true },
                    { "name": "zed-card", "handle": "zed", "held": false }
                ] }
            })
        );
    }

    #[tokio::test]
    async fn spec_3_15_test_16_a_sidecar_with_permissions_0644_fails_init_with_sidecar_invalid() {
        let machine = Machine::new("test-16");
        machine.create_card("amy-card", "amy", AMY_SEED);
        fs::set_permissions(
            machine.sidecar_path("amy-card"),
            fs::Permissions::from_mode(LOOSE_SIDECAR_MODE),
        )
        .unwrap();
        let mut client = start_server(&machine, ServerPlan::alone(FIRST_PROCESS_ID));
        client.initialize().await;

        let refused = client.envelope("init", json!({ "card": "amy-card" })).await;

        assert_eq!(
            refused,
            json!({
                "ok": false,
                "you": unclaimed(),
                "error": {
                    "code": "sidecar_invalid",
                    "message": "The sidecar is absent, unreadable, has wrong permissions, or does not match the card."
                }
            })
        );
    }

    #[tokio::test]
    async fn spec_3_10_init_of_a_name_the_registry_does_not_hold_is_card_not_found() {
        let machine = Machine::new("not-found");
        machine.create_card("amy-card", "amy", AMY_SEED);
        let mut client = start_server(&machine, ServerPlan::alone(FIRST_PROCESS_ID));
        client.initialize().await;

        for name in ["nobody-card", "../acme/amy-card", "Amy-card"] {
            let refused = client.envelope("init", json!({ "card": name })).await;

            assert_eq!(refused["error"]["code"], "card_not_found", "{name}");
            assert_eq!(refused["you"], unclaimed(), "{name}");
        }
    }

    #[tokio::test]
    async fn spec_3_5_init_of_a_card_that_fails_validation_is_card_invalid() {
        let machine = Machine::new("invalid");
        machine.create_card("amy-card", "amy", AMY_SEED);
        let key = render_public_key(&SecretKey::from_bytes(&[AMY_SEED; KEY_BYTES]).public());
        let cases = [
            ("unparsable", "handle: [amy\n".to_owned()),
            ("not-a-handle", format!("handle: Amy\npublic_key: {key}\n")),
            (
                "announced-only",
                format!("handle: {}-2\npublic_key: {key}\n", "a".repeat(32)),
            ),
            ("not-a-key", "handle: amy\npublic_key: 00ff\n".to_owned()),
            (
                "other-network",
                format!("handle: amy\npublic_key: {key}\nnetwork: other\n"),
            ),
        ];
        for (name, text) in &cases {
            machine.write_card_text(name, text);
            fs::copy(machine.sidecar_path("amy-card"), machine.sidecar_path(name)).unwrap();
        }
        let mut client = start_server(&machine, ServerPlan::alone(FIRST_PROCESS_ID));
        client.initialize().await;

        for (name, _) in cases {
            let refused = client.envelope("init", json!({ "card": name })).await;

            assert_eq!(refused["error"]["code"], "card_invalid", "{name}");
            assert_eq!(refused["you"], unclaimed(), "{name}");
        }
    }

    #[tokio::test]
    async fn spec_3_10_a_description_that_takes_public_past_3072_bytes_is_bad_request_before_the_lock()
     {
        let machine = Machine::new("over-cap");
        machine.create_card("amy-card", "amy", AMY_SEED);
        let _held_by_another_process = machine.hold_card("amy-card", AMY_SEED);
        let mut client = start_server(&machine, ServerPlan::alone(FIRST_PROCESS_ID));
        client.initialize().await;
        let description = "d".repeat(PUBLIC_MAX_BYTES);

        let refused = client
            .envelope(
                "init",
                json!({ "card": "amy-card", "description": description }),
            )
            .await;

        assert_eq!(refused["error"]["code"], "bad_request");
        assert_eq!(refused["you"], unclaimed());
    }

    #[tokio::test]
    async fn spec_3_10_init_claims_the_card_and_returns_its_handle_and_inbox_path_in_time() {
        let _slot = node_slot().await;
        let machine = Machine::new("init");
        machine.create_card("amy-card", "amy", AMY_SEED);
        let mut client = start_server(&machine, ServerPlan::alone(FIRST_PROCESS_ID));
        client.initialize().await;
        let started = Instant::now();

        let claimed = client
            .envelope(
                "init",
                json!({ "card": "amy-card", "description": "tester" }),
            )
            .await;

        assert!(started.elapsed() < SPEC_JOIN_TIMEOUT_PLUS_OBSERVER_WAIT);
        let inbox_path = machine.directories().inbox_socket_file(FIRST_PROCESS_ID);
        assert_eq!(
            claimed,
            json!({
                "ok": true,
                "you": { "card": "amy-card", "handle": "amy", "status": "claimed" },
                "result": { "handle": "amy", "inbox_path": inbox_path }
            })
        );
    }

    #[tokio::test]
    async fn spec_3_10_list_cards_reports_the_card_this_process_holds_as_held_without_trying_it() {
        let _slot = node_slot().await;
        let machine = Machine::new("own-held");
        machine.create_card("amy-card", "amy", AMY_SEED);
        let mut client = start_server(&machine, ServerPlan::alone(FIRST_PROCESS_ID));
        client.initialize().await;
        client.envelope("init", json!({ "card": "amy-card" })).await;
        fs::remove_file(machine.sidecar_path("amy-card")).unwrap();

        let listed = client.envelope("list_cards", json!({})).await;

        assert_eq!(
            listed["result"],
            json!({ "cards": [{ "name": "amy-card", "handle": "amy", "held": true }] })
        );
    }

    #[tokio::test]
    async fn spec_3_15_test_6_two_servers_init_one_card_at_once_and_exactly_one_claims_it() {
        let _slot = node_slot().await;
        let machine = Machine::new("test-6");
        machine.create_card("amy-card", "amy", AMY_SEED);
        let mut first = start_server(&machine, ServerPlan::alone(FIRST_PROCESS_ID));
        let mut second = start_server(&machine, ServerPlan::alone(SECOND_PROCESS_ID));
        first.initialize().await;
        second.initialize().await;

        let (first_answer, second_answer) = tokio::join!(
            first.envelope("init", json!({ "card": "amy-card" })),
            second.envelope("init", json!({ "card": "amy-card" })),
        );

        let mut outcomes = [&first_answer, &second_answer]
            .map(|answer| {
                answer["result"]["handle"]
                    .as_str()
                    .or_else(|| answer["error"]["code"].as_str())
                    .unwrap()
                    .to_owned()
            })
            .to_vec();
        outcomes.sort();
        assert_eq!(outcomes, ["amy", "card_held"]);
        let loser = if first_answer["ok"] == json!(true) {
            &second_answer
        } else {
            &first_answer
        };
        assert_eq!(loser["you"], unclaimed());
    }

    #[tokio::test]
    async fn spec_3_15_test_2_a_message_to_a_member_with_no_inbox_client_is_delivered_unattached_and_its_inbox_tool_returns_it()
     {
        let _slot = node_slot().await;
        let machine = Machine::new("test-2");
        let (mut amy, mut bob) = claimed_pair(&machine).await;

        let sent = amy
            .envelope("send", json!({ "to": "bob", "body": MESSAGE_BODY }))
            .await;
        let read = bob.envelope("inbox", json!({})).await;

        let id = sent["result"]["id"].as_str().unwrap();
        assert_eq!(
            sent["result"],
            json!({ "id": id, "delivered": true, "attached": false })
        );
        let message = &read["result"]["messages"][0];
        assert_eq!(read["result"]["last_seq"], json!(1));
        assert_eq!(read["result"]["messages"].as_array().unwrap().len(), 1);
        assert_eq!(message["seq"], json!(1));
        assert_eq!(message["id"], json!(id));
        assert_eq!(message["from"], json!("amy"));
        assert_eq!(message["body"], json!(MESSAGE_BODY));
        assert!(message["sent_at"].as_u64().unwrap() <= message["received_at"].as_u64().unwrap());
        assert_eq!(
            bob.envelope("inbox", json!({ "since": 1 })).await["result"],
            json!({ "messages": [], "last_seq": 1 })
        );
    }

    #[tokio::test]
    async fn spec_3_10_peers_lists_the_other_member_with_its_status_public_mapping_and_last_seen() {
        let _slot = node_slot().await;
        let machine = Machine::new("peers");
        let started_ms = wall_clock_ms();
        let (mut amy, _bob) = claimed_pair(&machine).await;

        let listed = amy.envelope("peers", json!({})).await;

        let peers = listed["result"]["peers"].as_array().unwrap();
        assert_eq!(peers.len(), 1, "{listed}");
        let last_seen = peers[0]["last_seen"].as_u64().unwrap();
        assert!((started_ms..=wall_clock_ms()).contains(&last_seen));
        assert_eq!(
            peers[0],
            json!({
                "handle": "bob",
                "status": "online",
                "attached": false,
                "public": { "description": BOB_DESCRIPTION },
                "last_seen": last_seen
            })
        );
    }

    #[tokio::test]
    async fn spec_3_10_broadcast_returns_one_result_per_handle_in_the_order_given() {
        let _slot = node_slot().await;
        let machine = Machine::new("broadcast");
        let (mut amy, mut bob) = claimed_pair(&machine).await;

        let broadcast = amy
            .envelope(
                "broadcast",
                json!({ "to": ["nobody", "bob"], "body": MESSAGE_BODY }),
            )
            .await;
        let read = bob.envelope("inbox", json!({})).await;

        let id = broadcast["result"]["results"][1]["id"].as_str().unwrap();
        assert_eq!(
            broadcast["result"],
            json!({ "results": [
                {
                    "to": "nobody",
                    "delivered": false,
                    "error": { "code": "unknown_handle", "message": "No present member has that handle." }
                },
                { "to": "bob", "id": id, "delivered": true, "attached": false }
            ] })
        );
        assert_eq!(read["result"]["messages"][0]["id"], json!(id));
    }

    #[tokio::test]
    async fn spec_3_15_test_12_the_tool_list_is_identical_before_and_after_init() {
        let _slot = node_slot().await;
        let machine = Machine::new("test-12");
        machine.create_card("amy-card", "amy", AMY_SEED);
        let mut client = start_server(&machine, ServerPlan::alone(FIRST_PROCESS_ID));
        client.initialize().await;
        let before = client.request("tools/list", json!({})).await["result"].clone();

        let claimed = client.envelope("init", json!({ "card": "amy-card" })).await;
        let after = client.request("tools/list", json!({})).await["result"].clone();

        assert_eq!(claimed["ok"], json!(true));
        assert_eq!(before["tools"].as_array().unwrap().len(), 6);
        assert_eq!(after, before);
    }

    fn every_tool_call() -> [(&'static str, Value); 7] {
        [
            ("list_cards", json!({})),
            ("peers", json!({})),
            ("send", json!({ "to": "bob", "body": "hi" })),
            ("broadcast", json!({ "to": ["bob"], "body": "hi" })),
            ("inbox", json!({ "since": 0 })),
            ("unknown_tool", json!({})),
            ("init", json!({ "card": "amy-card" })),
        ]
    }

    #[tokio::test]
    async fn spec_3_15_test_13_every_tool_response_carries_you_with_the_current_status() {
        let _slot = node_slot().await;
        let machine = Machine::new("test-13");
        machine.create_card("amy-card", "amy", AMY_SEED);
        let mut client = start_server(&machine, ServerPlan::alone(FIRST_PROCESS_ID));
        client.initialize().await;
        let mut before = Vec::new();
        for (tool, arguments) in every_tool_call() {
            before.push((tool, client.envelope(tool, arguments).await));
        }

        let mut after = Vec::new();
        for (tool, arguments) in every_tool_call() {
            after.push((tool, client.envelope(tool, arguments).await));
        }

        let claimed = json!({ "card": "amy-card", "handle": "amy", "status": "claimed" });
        for (tool, envelope) in &before {
            let expected = if *tool == "init" {
                &claimed
            } else {
                &unclaimed()
            };
            assert_eq!(&envelope["you"], expected, "before init, {tool}");
        }
        for (tool, envelope) in &after {
            assert_eq!(envelope["you"], claimed, "after init, {tool}");
        }
        let answers: Vec<(&str, Value)> = after
            .into_iter()
            .map(|(tool, envelope)| {
                (
                    tool,
                    envelope
                        .get("result")
                        .or_else(|| envelope["error"].get("code"))
                        .cloned()
                        .unwrap(),
                )
            })
            .collect();
        assert_eq!(
            answers,
            vec![
                (
                    "list_cards",
                    json!({ "cards": [{ "name": "amy-card", "handle": "amy", "held": true }] })
                ),
                ("peers", json!({ "peers": [] })),
                ("send", json!("unknown_handle")),
                (
                    "broadcast",
                    json!({ "results": [{
                        "to": "bob",
                        "delivered": false,
                        "error": { "code": "unknown_handle", "message": "No present member has that handle." }
                    }] })
                ),
                ("inbox", json!({ "messages": [], "last_seq": 0 })),
                ("unknown_tool", json!("bad_request")),
                ("init", json!("bad_request")),
            ]
        );
    }

    #[tokio::test]
    async fn spec_3_10_initialize_names_the_server_estafette_and_offers_tools() {
        let machine = Machine::new("server-name");
        let mut client = start_server(&machine, ServerPlan::alone(FIRST_PROCESS_ID));

        let response = client.initialize().await;

        assert_eq!(response["result"]["serverInfo"]["name"], "estafette");
        assert!(response["result"]["capabilities"]["tools"].is_object());
    }

    #[tokio::test]
    async fn spec_3_10_the_tool_list_is_the_six_tools_with_their_exact_descriptions_and_schemas() {
        let machine = Machine::new("tool-list");
        let mut client = start_server(&machine, ServerPlan::alone(FIRST_PROCESS_ID));
        client.initialize().await;

        let listed = client.request("tools/list", json!({})).await;

        let no_input = json!({ "type": "object", "properties": {}, "additionalProperties": false });
        let recipient = json!({ "type": "string", "maxLength": 74 });
        assert_eq!(
            listed["result"]["tools"],
            json!([
                {
                    "name": "list_cards",
                    "description": "List the identity cards on this machine and whether each is in use.",
                    "inputSchema": no_input
                },
                {
                    "name": "init",
                    "description": "Claim one card for this session. Returns the handle you must use. Call once.",
                    "inputSchema": {
                        "type": "object",
                        "properties": { "card": { "type": "string" }, "description": { "type": "string" } },
                        "required": ["card"],
                        "additionalProperties": false
                    }
                },
                {
                    "name": "peers",
                    "description": "List the members that are present, with their handles and descriptions.",
                    "inputSchema": no_input
                },
                {
                    "name": "send",
                    "description": "Send a text message to one member by handle. Returns whether it was delivered.",
                    "inputSchema": {
                        "type": "object",
                        "properties": { "to": recipient, "body": { "type": "string" } },
                        "required": ["to", "body"],
                        "additionalProperties": false
                    }
                },
                {
                    "name": "broadcast",
                    "description": "Send one text message to several members by handle. Returns one result per member.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "to": { "type": "array", "items": recipient },
                            "body": { "type": "string" }
                        },
                        "required": ["to", "body"],
                        "additionalProperties": false
                    }
                },
                {
                    "name": "inbox",
                    "description": "Read messages that arrived for you. Use `since` to read only newer ones.",
                    "inputSchema": {
                        "type": "object",
                        "properties": { "since": { "type": "integer", "minimum": 0 } },
                        "additionalProperties": false
                    }
                }
            ])
        );
    }
}
