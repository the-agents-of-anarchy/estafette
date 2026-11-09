use std::fs;
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Output};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::Duration;

use serde_json::{Value, json};

pub const BRIDGE: &str = env!("CARGO_BIN_EXE_estafette-bridge-claude");
const SHORT_SCRATCH_PARENT: &str = "/tmp";
const SCRATCH_PREFIX: &str = "eb35";
pub const MESSAGING_TOKEN: &str = "acceptance-token";
const MCP_PROTOCOL_VERSION: &str = "2025-06-18";
const INITIALIZE_REQUEST_ID: u64 = 1;
const INIT_REQUEST_ID: u64 = 2;
const MCP_ANSWER_TIMEOUT: Duration = Duration::from_secs(30);
const SETTINGS_HOOK: &str = include_str!("../../hooks/session-start.json");
type Outcome<T> = Result<T, Box<dyn std::error::Error>>;

pub struct Fixture {
    pub root: PathBuf,
}

impl Fixture {
    pub fn new() -> io::Result<Self> {
        let root = PathBuf::from(SHORT_SCRATCH_PARENT)
            .join(format!("{SCRATCH_PREFIX}-{}", std::process::id()));
        drop(fs::remove_dir_all(&root));
        for directory in ["home/.claude", "runtime", "tmp"] {
            fs::create_dir_all(root.join(directory))?;
        }
        fs::write(root.join("home/.claude/settings.json"), SETTINGS_HOOK)?;
        Ok(Self { root })
    }

    pub fn messaging_socket(&self) -> PathBuf {
        self.root.join("messaging.sock")
    }

    fn binary_path() -> PathBuf {
        Path::new(BRIDGE).with_file_name("estafette")
    }

    pub fn estafette(&self) -> Command {
        let mut command = Command::new(Self::binary_path());
        command
            .env_clear()
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env("XDG_DATA_HOME", self.root.join("data"))
            .env("XDG_RUNTIME_DIR", self.root.join("runtime"));
        command
    }

    pub fn bridge(&self) -> Command {
        let mut command = Command::new(BRIDGE);
        command
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", self.root.join("home"))
            .env("TMPDIR", self.root.join("tmp"))
            .env("XDG_RUNTIME_DIR", self.root.join("runtime"))
            .env("CLAUDE_CODE_MESSAGING_SOCKET", self.messaging_socket())
            .env("CLAUDE_CODE_MESSAGING_TOKEN", MESSAGING_TOKEN)
            .current_dir(&self.root);
        command
    }

    pub fn run_estafette(&self, arguments: &[&str]) -> io::Result<Output> {
        self.estafette().args(arguments).output()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        drop(fs::remove_dir_all(&self.root));
    }
}

pub struct Spawned(pub Child);

impl Drop for Spawned {
    fn drop(&mut self) {
        drop(self.0.kill());
        drop(self.0.wait());
    }
}

pub fn forward_json_lines(source: impl std::io::Read + Send + 'static) -> Receiver<Value> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(source).lines().map_while(Result::ok) {
            if let Ok(message) = serde_json::from_str(&line)
                && sender.send(message).is_err()
            {
                break;
            }
        }
    });
    receiver
}

fn send_message(input: &mut ChildStdin, message: &Value) -> io::Result<()> {
    writeln!(input, "{message}")?;
    input.flush()
}

fn answer_to(answers: &Receiver<Value>, request_id: u64) -> Outcome<Value> {
    loop {
        let answer = answers.recv_timeout(MCP_ANSWER_TIMEOUT)?;
        if answer.get("id") == Some(&json!(request_id)) {
            return Ok(answer);
        }
    }
}

pub fn claim_card_over_mcp(
    input: &mut ChildStdin,
    answers: &Receiver<Value>,
    card: &str,
) -> Outcome<Value> {
    send_message(
        input,
        &json!({"jsonrpc": "2.0", "id": INITIALIZE_REQUEST_ID, "method": "initialize", "params": {
            "protocolVersion": MCP_PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": {"name": "acceptance", "version": "0"},
        }}),
    )?;
    answer_to(answers, INITIALIZE_REQUEST_ID)?;
    send_message(
        input,
        &json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
    )?;
    send_message(
        input,
        &json!({"jsonrpc": "2.0", "id": INIT_REQUEST_ID, "method": "tools/call", "params": {
            "name": "init",
            "arguments": {"card": card},
        }}),
    )?;
    let answer = answer_to(answers, INIT_REQUEST_ID)?;
    Ok(answer
        .pointer("/result/structuredContent")
        .cloned()
        .unwrap_or(answer))
}
