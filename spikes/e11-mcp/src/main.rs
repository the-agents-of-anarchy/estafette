//! E11: estafette's MCP surface on rmcp. Six static tools, stub bodies, the 3.10 envelope.

#![expect(dead_code, reason = "stubs parse arguments they do not read")]

use std::{
    io,
    pin::{Pin, pin},
    sync::{Once, OnceLock},
    task::{Context, Poll},
    time::{Duration, Instant},
};

use rmcp::{
    ErrorData, RoleServer, ServerHandler, ServiceExt,
    model::{
        CallToolRequestParams, CallToolResponse, CallToolResult, Implementation, ListToolsResult,
        PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool, object,
    },
    service::RequestContext,
};
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncRead, ReadBuf, Stdin},
    select,
    signal::unix::{SignalKind, signal},
    time::sleep,
};

const NAME: &str = "estafette";

type Fail = (&'static str, &'static str);
const NOT_CLAIMED: Fail = ("not_claimed", "Claim a card with init first.");
const CARD_HELD: Fail = ("card_held", "That card has a live holder.");
const UNKNOWN_HANDLE: Fail = ("unknown_handle", "No present member has that handle.");
const TIMEOUT: Fail = ("timeout", "The member did not acknowledge in time.");
const BAD_REQUEST: Fail = ("bad_request", "The input did not match the tool schema.");

#[derive(Deserialize)]
struct InitArgs {
    card: String,
    description: Option<String>,
}

#[derive(Deserialize)]
struct SendArgs {
    to: String,
    body: String,
}

#[derive(Deserialize)]
struct BroadcastArgs {
    to: Vec<String>,
    body: String,
}

#[derive(Deserialize)]
struct InboxArgs {
    since: Option<u64>,
}

fn tools() -> Vec<Tool> {
    let none = || json!({"type": "object", "properties": {}});
    [
        (
            "list_cards",
            "List the identity cards on this machine and whether each is in use.",
            none(),
        ),
        (
            "init",
            "Claim one card for this session. Returns the handle you must use. Call once.",
            json!({
                "type": "object",
                "properties": {"card": {"type": "string"}, "description": {"type": "string"}},
                "required": ["card"],
            }),
        ),
        (
            "peers",
            "List the members that are present, with their handles and descriptions.",
            none(),
        ),
        (
            "send",
            "Send a text message to one member by handle. Returns whether it was delivered.",
            json!({
                "type": "object",
                "properties": {"to": {"type": "string"}, "body": {"type": "string"}},
                "required": ["to", "body"],
            }),
        ),
        (
            "broadcast",
            "Send one text message to several members by handle. Returns one result per member.",
            json!({
                "type": "object",
                "properties": {
                    "to": {"type": "array", "items": {"type": "string"}},
                    "body": {"type": "string"},
                },
                "required": ["to", "body"],
            }),
        ),
        (
            "inbox",
            "Read messages that arrived for you. Use `since` to read only newer ones.",
            json!({"type": "object", "properties": {"since": {"type": "integer"}}}),
        ),
    ]
    .map(|(name, description, schema)| Tool::new(name, description, object(schema)))
    .into()
}

fn parse<T: DeserializeOwned>(args: Value) -> Result<T, Fail> {
    serde_json::from_value(args).map_err(|_| BAD_REQUEST)
}

struct Estafette {
    tools: Vec<Tool>,
    /// (card, handle), set once by `init`.
    claim: OnceLock<(String, String)>,
}

impl Estafette {
    fn new() -> Self {
        Self {
            tools: tools(),
            claim: OnceLock::new(),
        }
    }

    fn claimed(&self) -> Result<(), Fail> {
        self.claim.get().map(|_| ()).ok_or(NOT_CLAIMED)
    }

    // stubs:begin
    async fn dispatch(
        &self,
        name: &str,
        args: Value,
        ctx: &RequestContext<RoleServer>,
    ) -> Result<Value, Fail> {
        match name {
            "list_cards" => Ok(json!({"cards": [
                {"name": "alice", "handle": "alice", "held": false},
                {"name": "taken", "handle": "taken", "held": true},
            ]})),
            "init" => {
                let InitArgs { card, .. } = parse(args)?;
                if card == "taken" {
                    return Err(CARD_HELD);
                }
                self.claim
                    .set((card.clone(), card.clone()))
                    .map_err(|_| BAD_REQUEST)?;
                Ok(json!({"handle": card, "inbox_path": "/tmp/e11-mcp.sock"}))
            }
            "peers" => {
                self.claimed()?;
                Ok(json!({"peers": []}))
            }
            "send" => {
                let SendArgs { to, body } = parse(args)?;
                self.claimed()?;
                if to == "ghost" {
                    return Err(UNKNOWN_HANDLE);
                }
                // `slow:<ms>` holds the call in flight; `coop:<ms>` does too, but yields to cancellation.
                if let Some(ms) = body.strip_prefix("slow:").and_then(|ms| ms.parse().ok()) {
                    sleep(Duration::from_millis(ms)).await;
                } else if let Some(ms) = body.strip_prefix("coop:").and_then(|ms| ms.parse().ok()) {
                    select! {
                        _ = sleep(Duration::from_millis(ms)) => {}
                        _ = ctx.ct.cancelled() => return Err(TIMEOUT),
                    }
                }
                Ok(json!({"id": "stub", "delivered": true, "attached": false}))
            }
            "broadcast" => {
                let BroadcastArgs { to, .. } = parse(args)?;
                self.claimed()?;
                let results: Vec<Value> = to
                    .iter()
                    .map(|to| match to.as_str() {
                        "ghost" => json!({"to": to, "delivered": false,
                            "error": {"code": UNKNOWN_HANDLE.0, "message": UNKNOWN_HANDLE.1}}),
                        _ => json!({"to": to, "id": "stub", "delivered": true, "attached": false}),
                    })
                    .collect();
                Ok(json!({"results": results}))
            }
            "inbox" => {
                let InboxArgs { since } = parse(args)?;
                self.claimed()?;
                Ok(json!({"messages": [], "last_seq": since.unwrap_or(0)}))
            }
            _ => Err(BAD_REQUEST),
        }
    }
    // stubs:end

    /// The 3.10 envelope. `you` is read after the call, so it reports the state the call left.
    fn envelope(&self, outcome: Result<Value, Fail>) -> CallToolResponse {
        let you = match self.claim.get() {
            Some((card, handle)) => json!({"card": card, "handle": handle, "status": "claimed"}),
            None => json!({"card": null, "handle": null, "status": "unclaimed"}),
        };
        match outcome {
            Ok(result) => {
                CallToolResult::structured(json!({"ok": true, "you": you, "result": result}))
            }
            Err((code, message)) => CallToolResult::structured_error(
                json!({"ok": false, "you": you, "error": {"code": code, "message": message}}),
            ),
        }
        .into()
    }
}

impl ServerHandler for Estafette {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(NAME, env!("CARGO_PKG_VERSION")))
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(self.tools.clone()))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let args = Value::Object(request.arguments.unwrap_or_default());
        Ok(self.envelope(self.dispatch(&request.name, args, &context).await))
    }
}

/// Stub for broadcasting Departure: once, at the first trigger.
static DEPARTED: Once = Once::new();

fn depart(t0: Instant, why: &str) {
    DEPARTED.call_once(|| eprintln!("{:?} departure ({why})", t0.elapsed()));
}

/// Stdin that departs at the first end of input, before rmcp's own drain starts.
struct DepartOnEof {
    inner: Stdin,
    t0: Instant,
}

impl AsyncRead for DepartOnEof {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let before = buf.filled().len();
        let polled = Pin::new(&mut self.inner).poll_read(cx, buf);
        match &polled {
            Poll::Ready(Ok(())) if buf.filled().len() == before => depart(self.t0, "stdin closed"),
            Poll::Ready(Err(_)) => depart(self.t0, "stdin failed"),
            _ => {}
        }
        polled
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let t0 = Instant::now();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let result = rt.block_on(run(t0));
    eprintln!("{:?} runtime returned", t0.elapsed());
    if std::env::args().nth(1).as_deref() == Some("bg") {
        rt.shutdown_background();
    } else {
        // Waits for blocking threads, a pending stdin read included.
        drop(rt);
    }
    eprintln!("{:?} runtime dropped", t0.elapsed());
    result
}

async fn run(t0: Instant) -> Result<(), Box<dyn std::error::Error>> {
    let mut term = signal(SignalKind::terminate())?;
    let running = select! {
        served = Estafette::new().serve((DepartOnEof { inner: tokio::io::stdin(), t0 }, tokio::io::stdout())) => served?,
        _ = term.recv() => {
            depart(t0, "SIGTERM");
            return Ok(());
        }
    };
    eprintln!("{:?} serving", t0.elapsed());
    let stop = running.cancellation_token();
    let mut done = pin!(running.waiting());
    select! {
        _ = &mut done => {}
        _ = term.recv() => {
            depart(t0, "SIGTERM");
            stop.cancel();
            (&mut done).await?;
        }
    }
    eprintln!("{:?} stopped", t0.elapsed());
    Ok(())
}
