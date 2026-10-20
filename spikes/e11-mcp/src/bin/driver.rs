//! Test client for the E11 server: raw newline-delimited JSON-RPC over the child's pipes.
//!
//!   driver wire <protocol version>   scripted session: exact wire lines, checks for tests 12 and 13
//!   driver probe                     bad input against rmcp's own `#[tool]` macros
//!   driver shutdown <runs>           time from stdin close or SIGTERM to process exit

use std::{
    io::{BufRead, BufReader, Read, Write},
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc::{self, Receiver},
    thread,
    time::{Duration, Instant},
};

use serde_json::{Value, json};

unsafe extern "C" {
    fn kill(pid: i32, sig: i32) -> i32;
}

const SIGTERM: i32 = 15;
const LEGACY: &str = "2025-11-25";
const INLINE: &str = "2026-07-28";

fn lines(reader: impl Read + Send + 'static) -> Receiver<String> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(reader).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    rx
}

struct Server {
    child: Child,
    stdin: Option<ChildStdin>,
    out: Receiver<String>,
    err: Receiver<String>,
    proto: String,
    next_id: u64,
    /// Server messages that answered nothing we asked.
    unsolicited: Vec<String>,
    /// `result` of every `tools/call` made through `tool`.
    results: Vec<Value>,
}

impl Server {
    fn spawn(bin: &str, args: &[&str], proto: &str) -> Self {
        let path = std::env::current_exe().unwrap().with_file_name(bin);
        let mut child = Command::new(path)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        Self {
            stdin: child.stdin.take(),
            out: lines(child.stdout.take().unwrap()),
            err: lines(child.stderr.take().unwrap()),
            child,
            proto: proto.into(),
            next_id: 0,
            unsolicited: vec![],
            results: vec![],
        }
    }

    fn send(&mut self, message: &Value) {
        writeln!(self.stdin.as_mut().unwrap(), "{message}").unwrap();
    }

    /// Sends a request and returns the raw response line with its parsed form.
    fn call(&mut self, method: &str, mut params: Value) -> (String, Value) {
        self.next_id += 1;
        let id = self.next_id;
        if self.proto.as_str() >= INLINE {
            params["_meta"] = json!({
                "io.modelcontextprotocol/protocolVersion": self.proto,
                "io.modelcontextprotocol/clientCapabilities": {},
                "io.modelcontextprotocol/clientInfo": {"name": "e11-driver", "version": "0"},
            });
        }
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        loop {
            let line = self.out.recv_timeout(Duration::from_secs(10)).unwrap();
            let value: Value = serde_json::from_str(&line).unwrap();
            if value["id"] == id {
                return (line, value);
            }
            self.unsolicited.push(line);
        }
    }

    /// The `initialize` handshake. 2026-07-28 has none; every request carries its own `_meta`.
    fn hello(&mut self) -> Option<String> {
        if self.proto.as_str() >= INLINE {
            return None;
        }
        let proto = self.proto.clone();
        let (raw, _) = self.call(
            "initialize",
            json!({"protocolVersion": proto, "capabilities": {},
                "clientInfo": {"name": "e11-driver", "version": "0"}}),
        );
        self.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
        Some(raw)
    }

    /// `tools/call`, printed as received.
    fn tool(&mut self, label: &str, name: &str, arguments: Value) {
        let (raw, value) = self.call("tools/call", json!({"name": name, "arguments": arguments}));
        println!("{label}\n  {raw}");
        self.results.push(value["result"].clone());
    }

    /// A `tools/call` request sent as given, printed as received, kept out of `results`.
    fn raw_call(&mut self, label: &str, params: Value) {
        let (raw, _) = self.call("tools/call", params);
        println!("{label}\n  {raw}");
    }

    fn wait_exit(&mut self, since: Instant, cap: Duration) -> Option<Duration> {
        while since.elapsed() < cap {
            if self.child.try_wait().unwrap().is_some() {
                return Some(since.elapsed());
            }
            thread::sleep(Duration::from_millis(1));
        }
        None
    }

    fn stderr(&self) -> Vec<String> {
        self.err.try_iter().collect()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn wire(proto: &str) {
    let mut s = Server::spawn("e11-mcp", &[], proto);
    let (label, announce) = match s.hello() {
        Some(raw) => ("initialize", raw),
        None => ("server/discover", s.call("server/discover", json!({})).0),
    };
    println!("{label}\n  {announce}");
    let (raw, before) = s.call("tools/list", json!({}));
    println!("tools/list before init\n  {raw}");

    s.tool("list_cards before init", "list_cards", json!({}));
    let gated = s.results.len();
    s.tool("peers before init", "peers", json!({}));
    s.tool(
        "send before init",
        "send",
        json!({"to": "bob", "body": "x"}),
    );
    s.tool(
        "broadcast before init",
        "broadcast",
        json!({"to": ["bob"], "body": "x"}),
    );
    s.tool("inbox before init", "inbox", json!({}));
    let gated = s.results[gated..].to_vec();
    s.tool(
        "send before init, body missing",
        "send",
        json!({"to": "bob"}),
    );
    s.tool(
        "send, to has the wrong type",
        "send",
        json!({"to": 5, "body": "x"}),
    );
    s.tool("inbox, since negative", "inbox", json!({"since": -1}));
    s.tool("unknown tool", "nope", json!({}));
    s.raw_call("send without arguments", json!({"name": "send"}));
    s.raw_call(
        "send, arguments a string",
        json!({"name": "send", "arguments": "oops"}),
    );
    s.tool("init, card held", "init", json!({"card": "taken"}));
    s.tool(
        "init",
        "init",
        json!({"card": "alice", "description": "stub"}),
    );
    s.tool("init again", "init", json!({"card": "alice"}));
    s.tool("peers after init", "peers", json!({}));
    s.tool(
        "send to an unknown handle",
        "send",
        json!({"to": "ghost", "body": "x"}),
    );
    s.tool(
        "broadcast, one unknown handle",
        "broadcast",
        json!({"to": ["a", "ghost", "b"], "body": "hi"}),
    );
    s.tool("inbox since 3", "inbox", json!({"since": 3}));
    let (raw, after) = s.call("tools/list", json!({}));
    println!("tools/list after init\n  {raw}");
    if let Ok(line) = s.out.recv_timeout(Duration::from_millis(300)) {
        s.unsolicited.push(line);
    }

    let names: Vec<&str> = after["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect();
    let envelopes: Vec<(&Value, &Value)> = s
        .results
        .iter()
        .map(|r| (&r["structuredContent"], r))
        .collect();
    let checks = [
        (
            "test 12: tools/list identical before and after init",
            before["result"] == after["result"],
        ),
        (
            "tools/list names are the six, in 3.10 order",
            names == ["list_cards", "init", "peers", "send", "broadcast", "inbox"],
        ),
        (
            "D28: peers, send, broadcast, inbox return not_claimed before init",
            gated.len() == 4
                && gated
                    .iter()
                    .all(|r| r["structuredContent"]["error"]["code"] == "not_claimed"),
        ),
        (
            "test 13: every tools/call result carries you.status",
            envelopes.iter().all(|(e, _)| {
                ["unclaimed", "claimed", "displaced"]
                    .contains(&e["you"]["status"].as_str().unwrap_or(""))
            }),
        ),
        (
            "content[0].text parses to the same JSON as structuredContent",
            envelopes.iter().all(|(e, r)| {
                let text = r["content"][0]["text"].as_str().unwrap_or("");
                serde_json::from_str::<Value>(text).ok().as_ref() == Some(*e)
            }),
        ),
        (
            "isError equals not ok on every result",
            envelopes
                .iter()
                .all(|(e, r)| r["isError"] == !e["ok"].as_bool().unwrap_or(true)),
        ),
        (
            "no server message that answers nothing (no list_changed)",
            s.unsolicited.is_empty(),
        ),
    ];
    println!("checks");
    for (name, ok) in &checks {
        println!("  {} {name}", if *ok { "PASS" } else { "FAIL" });
    }
    println!("unsolicited: {:?}", s.unsolicited);
    println!("stderr: {:?}", s.stderr());
    if checks.iter().any(|(_, ok)| !ok) {
        std::process::exit(1);
    }
}

fn probe() {
    let mut s = Server::spawn("probe", &[], LEGACY);
    let announce = s.hello().unwrap();
    println!("initialize\n  {announce}");
    println!("tools/list\n  {}", s.call("tools/list", json!({})).0);
    s.tool("valid", "send", json!({"to": "a", "body": "b"}));
    s.tool("body missing", "send", json!({"to": "a"}));
    s.tool(
        "to has the wrong type",
        "send",
        json!({"to": 5, "body": "b"}),
    );
    s.tool(
        "since negative",
        "send",
        json!({"to": "a", "body": "b", "since": -1}),
    );
    s.raw_call("no arguments", json!({"name": "send"}));
    s.raw_call("unknown tool", json!({"name": "nope", "arguments": {}}));
    s.raw_call(
        "arguments a string",
        json!({"name": "send", "arguments": "oops"}),
    );
}

fn median(sorted: &[Duration]) -> Duration {
    sorted[sorted.len() / 2]
}

/// One run of `case` (`<eof|term>-<pre|idle|slow|coop>`). Returns the time to exit, if the
/// process exited within the cap, then the time to exit after stdin closed, if it did not.
fn shutdown_once(
    case: &str,
    args: &[&str],
    stderr_out: &mut Vec<String>,
) -> (Option<Duration>, Option<Duration>) {
    let (trigger, stage) = case.split_once('-').unwrap();
    let mut s = Server::spawn("e11-mcp", args, LEGACY);
    if stage != "pre" {
        s.hello();
    }
    if stage == "slow" || stage == "coop" {
        s.call(
            "tools/call",
            json!({"name": "init", "arguments": {"card": "alice"}}),
        );
        let body = format!("{stage}:30000");
        s.send(&json!({"jsonrpc": "2.0", "id": 99, "method": "tools/call",
            "params": {"name": "send", "arguments": {"to": "bob", "body": body}}}));
    }
    // Let the server settle: the call in flight, or the stdin read pending.
    thread::sleep(Duration::from_millis(300));
    let t0 = Instant::now();
    let cap = match trigger {
        "eof" => {
            drop(s.stdin.take());
            Duration::from_secs(12)
        }
        _ => {
            // SAFETY: plain syscall on the pid of our own child.
            unsafe { kill(i32::try_from(s.child.id()).unwrap(), SIGTERM) };
            Duration::from_secs(4)
        }
    };
    let exited = s.wait_exit(t0, cap);
    let after_close = if exited.is_none() {
        let t1 = Instant::now();
        drop(s.stdin.take());
        s.wait_exit(t1, Duration::from_secs(4))
    } else {
        None
    };
    *stderr_out = s.stderr();
    stderr_out.push(format!("status {:?}", s.child.try_wait().unwrap()));
    (exited, after_close)
}

fn shutdown(runs: usize) {
    let cases: [(&str, &[&str]); 12] = [
        ("eof-pre", &[]),
        ("eof-idle", &[]),
        ("eof-slow", &[]),
        ("eof-coop", &[]),
        ("term-pre", &[]),
        ("term-idle", &[]),
        ("term-slow", &[]),
        ("term-coop", &[]),
        ("term-pre", &["bg"]),
        ("term-idle", &["bg"]),
        ("term-slow", &["bg"]),
        ("term-coop", &["bg"]),
    ];
    println!(
        "| case | mode | min ms | median ms | max ms | exited | hung at cap | exit ms after stdin closed (median) |"
    );
    for (case, args) in cases {
        let mut exits = vec![];
        let mut after = vec![];
        let mut timeline = vec![];
        for _ in 0..runs {
            let (exited, after_close) = shutdown_once(case, args, &mut timeline);
            exits.extend(exited);
            after.extend(after_close);
        }
        exits.sort();
        after.sort();
        let ms = |d: Duration| format!("{:.0}", d.as_secs_f64() * 1000.0);
        let (lo, mid, hi) = match exits.as_slice() {
            [] => ("-".into(), "-".into(), "-".into()),
            e => (ms(e[0]), ms(median(e)), ms(e[e.len() - 1])),
        };
        let resid = if after.is_empty() {
            "-".into()
        } else {
            ms(median(&after))
        };
        let mode = args.first().copied().unwrap_or("naive");
        println!(
            "| {case} | {mode} | {lo} | {mid} | {hi} | {}/{runs} | {}/{runs} | {resid} |",
            exits.len(),
            runs - exits.len()
        );
        println!("  last run stderr: {timeline:?}");
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("wire") => wire(&args[2]),
        Some("probe") => probe(),
        Some("shutdown") => shutdown(args[2].parse().unwrap()),
        _ => eprintln!("usage: driver wire <version> | probe | shutdown <runs>"),
    }
}
