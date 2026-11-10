use std::process::Command;
use std::time::{Duration, Instant};

use estafette::config::{Host, NetworkConfig};
use estafette::dirs::Directories;
use serde_json::json;

use crate::live_network::LiveNetwork;
use crate::mcp_server::McpServer;
use crate::scratch_machine::{Card, ScratchMachine};

/// How often the network configuration is read while waiting for a host.
const CONFIG_POLL: Duration = Duration::from_millis(100);

/// One machine's worth of estafette state under a scratch root of its own, joined to the live lab network.
pub struct LabMachine {
    scratch: ScratchMachine,
}

/// A running `estafette mcp` process that has claimed its card, kept alive until dropped.
pub struct ClaimedMember {
    _server: McpServer,
}

impl LabMachine {
    /// An empty scratch root named for the test, for the network of that name.
    pub fn fresh(test_tag: &str, network: &str) -> Self {
        Self {
            scratch: ScratchMachine::fresh(test_tag, network),
        }
    }

    /// A fresh machine named for the test, joined to the live network as its invitation describes.
    pub fn joined(test_tag: &str, network: &LiveNetwork) -> Self {
        let machine = Self::fresh(test_tag, network.name());
        network.join(&machine.directories());
        machine
    }

    /// The directories the binary derives from this machine's environment.
    pub fn directories(&self) -> Directories {
        self.scratch.directories()
    }

    /// The binary with this machine's environment alone.
    pub fn command(&self) -> Command {
        self.scratch.command()
    }

    /// Creates a card requesting the handle by the binary's own command.
    pub fn create_card(&self, name: &str, handle: &str) -> Card {
        self.scratch.create_card(name, handle)
    }

    /// Starts the binary's MCP server for the network, handshaken and with no card claimed.
    #[track_caller]
    pub fn start_server(&self) -> McpServer {
        McpServer::handshaken(self.scratch.spawn_mcp_server())
    }

    /// Starts the binary's MCP server for the network and claims the card through its init tool, requiring the claim to succeed.
    #[track_caller]
    pub fn start_member(&self, card: &str) -> ClaimedMember {
        let mut server = self.start_server();
        let claim = server.call("init", &json!({ "card": card }));
        assert_eq!(claim["ok"], json!(true), "{claim}");
        ClaimedMember { _server: server }
    }

    /// The hosts the network configuration names once one of them has the id, requiring that within the wait.
    #[track_caller]
    pub fn hosts_once_named(&self, id: &str, within: Duration) -> Vec<Host> {
        let config_file = self
            .directories()
            .network_config_file(self.scratch.network())
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
