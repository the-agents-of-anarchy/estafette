use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use estafette::config::{Host, NetworkConfig};
use estafette::dirs::{Directories, OperatingSystemFilesystem};
use serde_json::{Value, json};

/// The binary under test.
const BINARY: &str = env!("CARGO_BIN_EXE_estafette");
/// Where scratch roots go, short enough for an inbox socket path beneath them.
const SHORT_SCRATCH_PARENT: &str = "/tmp";
/// The start of every scratch root's name.
const SCRATCH_PREFIX: &str = "el";
/// The base directory variable for configuration (spec 3.3).
const CONFIG_HOME_VARIABLE: &str = "XDG_CONFIG_HOME";
/// The base directory variable for data (spec 3.3).
const DATA_HOME_VARIABLE: &str = "XDG_DATA_HOME";
/// The base directory variable for runtime files (spec 3.3).
const RUNTIME_DIRECTORY_VARIABLE: &str = "XDG_RUNTIME_DIR";
/// The search path the binary finds `ps` on.
const SEARCH_PATH_VARIABLE: &str = "PATH";
/// The configuration directory under a scratch root.
const CONFIG_HOME: &str = "c";
/// The data directory under a scratch root.
const DATA_HOME: &str = "d";
/// The runtime directory under a scratch root.
const RUNTIME_DIRECTORY: &str = "r";
/// The file a member's run record goes to under its scratch root.
const MEMBER_RUN_RECORD: &str = "member.stderr";
/// The JSON-RPC version MCP speaks.
const JSON_RPC_VERSION: &str = "2.0";
/// The MCP protocol version the client asks for.
const MCP_PROTOCOL_VERSION: &str = "2025-06-18";
/// The name the client gives in its handshake.
const CLIENT_NAME: &str = "estafette-lab-acceptance";
/// The version the client gives in its handshake.
const CLIENT_VERSION: &str = "0";
/// The id of the handshake request.
const INITIALIZE_REQUEST_ID: u64 = 1;
/// The id of the init tool call.
const INIT_REQUEST_ID: u64 = 2;
/// How long the member may take to answer a request.
const RESPONSE_WAIT: Duration = Duration::from_secs(60);
/// How often the network configuration is read while waiting for a host.
const CONFIG_POLL: Duration = Duration::from_millis(100);

/// One machine's worth of estafette state under a scratch root of its own, short enough for an inbox socket path, configured for one network.
pub struct LabMachine {
    root: PathBuf,
    network: String,
}

impl LabMachine {
    /// An empty scratch root named for the test, for the network of that name.
    pub fn fresh(test_tag: &str, network: &str) -> Self {
        let root = PathBuf::from(SHORT_SCRATCH_PARENT).join(format!(
            "{SCRATCH_PREFIX}-{}-{test_tag}",
            std::process::id()
        ));
        drop(fs::remove_dir_all(&root));
        fs::create_dir_all(&root).unwrap();
        Self {
            root,
            network: network.to_owned(),
        }
    }

    /// The directories the binary derives from this machine's environment.
    pub fn directories(&self) -> Directories {
        let environment = environment(&self.root);
        Directories::establish(
            |name: &str| {
                environment
                    .iter()
                    .find(|(variable, _)| *variable == name)
                    .map(|(_, value)| value.clone())
            },
            &OperatingSystemFilesystem,
            None,
        )
        .unwrap()
    }

    /// Runs the binary to completion on this machine, requiring success, and returns what it printed.
    pub fn run(&self, arguments: &[&str]) -> String {
        let output = self.command().args(arguments).output().unwrap();
        assert!(output.status.success(), "{arguments:?}: {output:?}");
        String::from_utf8(output.stdout).unwrap()
    }

    /// Starts the binary's MCP server for the network and claims the card through its init tool, requiring the claim to succeed.
    pub fn start_member(&self, card: &str) -> ClaimedMember {
        let run_record = File::create(self.root.join(MEMBER_RUN_RECORD)).unwrap();
        let mut command = self.command();
        command.args(["mcp", &self.network]);
        ClaimedMember::claim(command, run_record, card)
    }

    /// The binary with this machine's environment alone.
    pub fn command(&self) -> Command {
        let mut command = Command::new(BINARY);
        command.env_clear().envs(environment(&self.root));
        command
    }

    /// The hosts the network configuration names once one of them has the id, requiring that within the wait.
    pub fn hosts_once_named(&self, id: &str, within: Duration) -> Vec<Host> {
        let config_file = self
            .directories()
            .network_config_file(&self.network)
            .unwrap();
        let started = Instant::now();
        loop {
            let hosts = NetworkConfig::load(&config_file).unwrap().hosts;
            if hosts.iter().any(|host| host.id == id) {
                return hosts;
            }
            assert!(
                started.elapsed() < within,
                "the network configuration did not name host {id} within {within:?}"
            );
            std::thread::sleep(CONFIG_POLL);
        }
    }
}

impl Drop for LabMachine {
    fn drop(&mut self) {
        drop(fs::remove_dir_all(&self.root));
    }
}

/// The base directories under the scratch root, and the search path the binary finds `ps` on.
fn environment(root: &Path) -> Vec<(&'static str, OsString)> {
    vec![
        (
            CONFIG_HOME_VARIABLE,
            root.join(CONFIG_HOME).into_os_string(),
        ),
        (DATA_HOME_VARIABLE, root.join(DATA_HOME).into_os_string()),
        (
            RUNTIME_DIRECTORY_VARIABLE,
            root.join(RUNTIME_DIRECTORY).into_os_string(),
        ),
        (
            SEARCH_PATH_VARIABLE,
            std::env::var_os(SEARCH_PATH_VARIABLE).unwrap_or_default(),
        ),
    ]
}

/// A running `estafette mcp` process that has claimed its card, kept alive until dropped.
pub struct ClaimedMember {
    process: Child,
}

impl ClaimedMember {
    /// Starts the command as an MCP server writing its run record to the file, completes the MCP handshake and claims the card.
    fn claim(mut command: Command, run_record: File, card: &str) -> Self {
        let mut process = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(run_record)
            .spawn()
            .unwrap();
        let messages = json_lines_of(process.stdout.take().unwrap());
        let mut member = Self { process };
        member.write(&json!({
            "jsonrpc": JSON_RPC_VERSION,
            "id": INITIALIZE_REQUEST_ID,
            "method": "initialize",
            "params": {
                "protocolVersion": MCP_PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": { "name": CLIENT_NAME, "version": CLIENT_VERSION }
            }
        }));
        response_to(&messages, INITIALIZE_REQUEST_ID);
        member
            .write(&json!({ "jsonrpc": JSON_RPC_VERSION, "method": "notifications/initialized" }));
        member.write(&json!({
            "jsonrpc": JSON_RPC_VERSION,
            "id": INIT_REQUEST_ID,
            "method": "tools/call",
            "params": { "name": "init", "arguments": { "card": card } }
        }));
        let claim = response_to(&messages, INIT_REQUEST_ID)["result"]["structuredContent"].clone();
        assert_eq!(claim["ok"], json!(true), "{claim}");
        member
    }

    /// Writes one JSON-RPC message as a line on the member's standard input.
    fn write(&mut self, message: &Value) {
        let input = self.process.stdin.as_mut().unwrap();
        writeln!(input, "{message}").unwrap();
        input.flush().unwrap();
    }
}

impl Drop for ClaimedMember {
    fn drop(&mut self) {
        drop(self.process.stdin.take());
        drop(self.process.kill());
        drop(self.process.wait());
    }
}

/// Each line of the stream that parses as JSON, forwarded by a thread of its own.
fn json_lines_of(stream: impl std::io::Read + Send + 'static) -> Receiver<Value> {
    let (forward, messages) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stream).lines().map_while(Result::ok) {
            if let Ok(message) = serde_json::from_str(&line)
                && forward.send(message).is_err()
            {
                return;
            }
        }
    });
    messages
}

/// The response to the request of that id, skipping every other message.
fn response_to(messages: &Receiver<Value>, request_id: u64) -> Value {
    loop {
        let message = messages.recv_timeout(RESPONSE_WAIT).unwrap();
        if message["id"] == json!(request_id) {
            return message;
        }
    }
}
