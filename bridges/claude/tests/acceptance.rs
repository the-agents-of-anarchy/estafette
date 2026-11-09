use std::fs;
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Output, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::Duration;

use serde_json::{Value, json};

const BRIDGE: &str = env!("CARGO_BIN_EXE_estafette-bridge-claude");
const SHORT_SCRATCH_PARENT: &str = "/tmp";
const SCRATCH_PREFIX: &str = "eb35";
const NETWORK: &str = "lab";
const CARD: &str = "ana-card";
const HANDLE: &str = "ana";
const MESSAGING_TOKEN: &str = "acceptance-token";
const MCP_PROTOCOL_VERSION: &str = "2025-06-18";
const INITIALIZE_REQUEST_ID: u64 = 1;
const INIT_REQUEST_ID: u64 = 2;
const MCP_ANSWER_TIMEOUT: Duration = Duration::from_secs(30);
type Outcome<T> = Result<T, Box<dyn std::error::Error>>;
const SETTINGS_HOOK: &str = include_str!("../hooks/session-start.json");
const DOCTOR_CHECK_COUNT: usize = 5;

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new() -> io::Result<Self> {
        let root = PathBuf::from(SHORT_SCRATCH_PARENT)
            .join(format!("{SCRATCH_PREFIX}-{}", std::process::id()));
        drop(fs::remove_dir_all(&root));
        for directory in ["home/.claude", "runtime", "tmp"] {
            fs::create_dir_all(root.join(directory))?;
        }
        fs::write(root.join("home/.claude/settings.json"), SETTINGS_HOOK)?;
        Ok(Self { root })
    }

    fn messaging_socket(&self) -> PathBuf {
        self.root.join("messaging.sock")
    }

    fn binary_path() -> PathBuf {
        Path::new(BRIDGE).with_file_name("estafette")
    }

    fn estafette(&self) -> Command {
        let mut command = Command::new(Self::binary_path());
        command
            .env_clear()
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env("XDG_DATA_HOME", self.root.join("data"))
            .env("XDG_RUNTIME_DIR", self.root.join("runtime"));
        command
    }

    fn bridge(&self) -> Command {
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

    fn run_estafette(&self, arguments: &[&str]) -> io::Result<Output> {
        self.estafette().args(arguments).output()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        drop(fs::remove_dir_all(&self.root));
    }
}

struct Spawned(Child);

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

fn claim_card_over_mcp(input: &mut ChildStdin, answers: &Receiver<Value>) -> Outcome<Value> {
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
            "arguments": {"card": CARD},
        }}),
    )?;
    let answer = answer_to(answers, INIT_REQUEST_ID)?;
    Ok(answer
        .pointer("/result/structuredContent")
        .cloned()
        .unwrap_or(answer))
}

#[test]
fn spec_3_12_doctor_passes_every_check_for_a_bridge_beside_a_real_binary_serving_mcp() {
    let fixture = Fixture::new().unwrap();
    assert_eq!(
        fixture
            .run_estafette(&["network", "create", NETWORK])
            .unwrap()
            .status
            .code(),
        Some(0)
    );
    assert_eq!(
        fixture
            .run_estafette(&["create-card", NETWORK, CARD, HANDLE])
            .unwrap()
            .status
            .code(),
        Some(0)
    );
    let _messaging_socket = UnixListener::bind(fixture.messaging_socket()).unwrap();
    let mut member = Spawned(
        fixture
            .estafette()
            .args(["mcp", NETWORK])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let mut member_input = member.0.stdin.take().unwrap();
    let answers = forward_json_lines(member.0.stdout.take().unwrap());
    let claimed = claim_card_over_mcp(&mut member_input, &answers).unwrap();
    assert_eq!(claimed["ok"], true, "{claimed}");
    assert_eq!(claimed["you"]["handle"], HANDLE, "{claimed}");
    let _bridge = Spawned(
        fixture
            .bridge()
            .args(["run", &std::process::id().to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );

    let doctor = fixture.bridge().arg("doctor").output().unwrap();
    let report = String::from_utf8(doctor.stdout).unwrap();
    assert_eq!(doctor.status.code(), Some(0), "{report}");
    assert_eq!(
        report.lines().filter(|line| line.starts_with("ok")).count(),
        DOCTOR_CHECK_COUNT,
        "{report}"
    );
}
