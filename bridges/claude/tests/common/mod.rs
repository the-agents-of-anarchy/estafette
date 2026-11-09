use std::fs;
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Output, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::Duration;

use serde_json::{Value, json};

pub const BRIDGE: &str = env!("CARGO_BIN_EXE_estafette-bridge-claude");
const SHORT_SCRATCH_PARENT: &str = "/tmp";
const SCRATCH_PREFIX: &str = "eb35";
const DEFAULT_MESSAGING_TOKEN: &str = "acceptance-token";
const MCP_PROTOCOL_VERSION: &str = "2025-06-18";
const INITIALIZE_REQUEST_ID: u64 = 1;
const INIT_REQUEST_ID: u64 = 2;
const MCP_ANSWER_TIMEOUT: Duration = Duration::from_secs(30);
const SETTINGS_HOOK: &str = include_str!("../../hooks/session-start.json");
const BINARY_NAME: &str = "estafette";
const HOME_DIRECTORY: &str = "home";
const CLAUDE_DIRECTORY: &str = ".claude";
const RUNTIME_DIRECTORY: &str = "runtime";
const TMP_DIRECTORY: &str = "tmp";
const CONFIG_HOME: &str = "config";
const DATA_HOME: &str = "data";
const SETTINGS_FILE: &str = "settings.json";
const MESSAGING_SOCKET_FILE: &str = "messaging.sock";
const BINARY_NOT_BUILT: &str = "build it first: cargo build -p estafette (cargo test at the workspace root builds it through the integration tests of core)";
const SUCCESS_EXIT_CODE: i32 = 0;
const MISSING_PIPE: &str = "the spawned member has no piped standard stream";
type Outcome<T> = Result<T, Box<dyn std::error::Error>>;

pub struct Fixture {
    pub root: PathBuf,
    pub messaging_socket: PathBuf,
    pub messaging_token: String,
    pub claude_config_dir: Option<PathBuf>,
}

impl Fixture {
    pub fn new() -> io::Result<Self> {
        let root = PathBuf::from(SHORT_SCRATCH_PARENT)
            .join(format!("{SCRATCH_PREFIX}-{}", std::process::id()));
        drop(fs::remove_dir_all(&root));
        if !Self::binary_path().exists() {
            return Err(io::Error::new(io::ErrorKind::NotFound, BINARY_NOT_BUILT));
        }
        let claude_directory = root.join(HOME_DIRECTORY).join(CLAUDE_DIRECTORY);
        for directory in [
            claude_directory.clone(),
            root.join(RUNTIME_DIRECTORY),
            root.join(TMP_DIRECTORY),
        ] {
            fs::create_dir_all(directory)?;
        }
        fs::write(claude_directory.join(SETTINGS_FILE), SETTINGS_HOOK)?;
        Ok(Self {
            messaging_socket: root.join(MESSAGING_SOCKET_FILE),
            messaging_token: DEFAULT_MESSAGING_TOKEN.to_owned(),
            claude_config_dir: None,
            root,
        })
    }

    fn binary_path() -> PathBuf {
        Path::new(BRIDGE).with_file_name(BINARY_NAME)
    }

    fn estafette(&self) -> Command {
        let mut command = Command::new(Self::binary_path());
        command
            .env_clear()
            .env("XDG_CONFIG_HOME", self.root.join(CONFIG_HOME))
            .env("XDG_DATA_HOME", self.root.join(DATA_HOME))
            .env("XDG_RUNTIME_DIR", self.root.join(RUNTIME_DIRECTORY));
        command
    }

    pub fn bridge(&self) -> Command {
        let mut command = Command::new(BRIDGE);
        command
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", self.root.join(HOME_DIRECTORY))
            .env("TMPDIR", self.root.join(TMP_DIRECTORY))
            .env("XDG_RUNTIME_DIR", self.root.join(RUNTIME_DIRECTORY))
            .env("CLAUDE_CODE_MESSAGING_SOCKET", &self.messaging_socket)
            .env("CLAUDE_CODE_MESSAGING_TOKEN", &self.messaging_token)
            .current_dir(&self.root);
        if let Some(claude_config_dir) = &self.claude_config_dir {
            command.env("CLAUDE_CONFIG_DIR", claude_config_dir);
        }
        command
    }

    pub fn run_estafette(&self, arguments: &[&str]) -> io::Result<Output> {
        self.estafette().args(arguments).output()
    }

    pub fn run_estafette_successfully(&self, arguments: &[&str]) -> io::Result<()> {
        let output = self.run_estafette(arguments)?;
        if output.status.code() == Some(SUCCESS_EXIT_CODE) {
            Ok(())
        } else {
            Err(io::Error::other(format!(
                "estafette {arguments:?} failed: {}",
                String::from_utf8_lossy(&output.stderr)
            )))
        }
    }

    pub fn spawn_claimed_member(&self, network: &str, card: &str) -> Outcome<Member> {
        let mut process = Spawned(
            self.estafette()
                .args(["mcp", network])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()?,
        );
        let mut input = process.0.stdin.take().ok_or(MISSING_PIPE)?;
        let answers = forward_json_lines(process.0.stdout.take().ok_or(MISSING_PIPE)?);
        let claimed = claim_card_over_mcp(&mut input, &answers, card)?;
        Ok(Member {
            claimed,
            _input: input,
            _answers: answers,
            _process: process,
        })
    }

    pub fn spawn_bridge(&self) -> io::Result<Spawned> {
        self.bridge()
            .args(["run", &std::process::id().to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map(Spawned)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        drop(fs::remove_dir_all(&self.root));
    }
}

pub struct Member {
    pub claimed: Value,
    _input: ChildStdin,
    _answers: Receiver<Value>,
    _process: Spawned,
}

pub struct Spawned(Child);

impl Drop for Spawned {
    fn drop(&mut self) {
        drop(self.0.kill());
        drop(self.0.wait());
    }
}

fn forward_json_lines(source: impl std::io::Read + Send + 'static) -> Receiver<Value> {
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

fn claim_card_over_mcp(
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
