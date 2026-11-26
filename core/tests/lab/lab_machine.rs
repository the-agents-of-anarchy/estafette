use std::ffi::OsStr;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use estafette::config::{Host, NetworkConfig};
use estafette::dirs::TraceDirectory;

use crate::file_growth::{GrowthWatch, WaitEnded, first_found_as_files_grow};
use crate::live_network::LiveNetwork;
use crate::mcp_server::{INIT_TOOL, McpServer, call_succeeded, init_arguments};
use crate::member_events::MemberEvents;
use crate::scratch_machine::ScratchMachine;

/// The trace directory of the server the machine started last.
pub fn traces_directory_of_latest_member(machine: &ScratchMachine) -> PathBuf {
    let latest = machine.members_started().checked_sub(1).unwrap();
    machine.traces_directory_of_member(latest)
}

/// One machine's worth of estafette state under a scratch root of its own, joined to the live lab network.
pub struct LabMachine {
    scratch: ScratchMachine,
}

/// A running `estafette mcp` process that has claimed its card, kept alive until dropped, holding the machine it runs on and ending the process in its own drop, which runs before any field drops, so the process ends before the machine keeps its data whatever the order of the fields.
pub struct ClaimedMember {
    server: McpServer,
    machine: LabMachine,
}

impl Drop for ClaimedMember {
    fn drop(&mut self) {
        self.server.end();
    }
}

impl ClaimedMember {
    /// The events the member records, awaited in its trace directory.
    pub const fn events(&self) -> &MemberEvents {
        self.server.events()
    }

    /// The machine the member runs on.
    pub const fn machine(&self) -> &LabMachine {
        &self.machine
    }

    /// The trace directory of the server the member runs, the last its machine started.
    pub fn own_traces(&self) -> PathBuf {
        traces_directory_of_latest_member(self.machine.scratch())
    }
}

impl LabMachine {
    /// The scratch machine the lab machine's files and commands belong to.
    pub const fn scratch(&self) -> &ScratchMachine {
        &self.scratch
    }

    /// An empty scratch root named for the test, for the live network, its members' run records kept as the test asks.
    pub fn fresh(test_tag: &str, network: &LiveNetwork) -> Self {
        Self {
            scratch: ScratchMachine::fresh(test_tag, network.name(), network.kept_files_policy()),
        }
    }

    /// A fresh machine named for the test, joined to the live network as its invitation describes.
    pub fn joined(test_tag: &str, network: &LiveNetwork) -> Self {
        let machine = Self::fresh(test_tag, network);
        network.join(&machine.scratch().directories());
        machine
    }

    /// The events an observer command run on this machine records, in the trace directory the binary derives from the machine's own environment.
    pub fn observer_events(&self, label: &str) -> MemberEvents {
        let command = self.scratch().command();
        let traces = TraceDirectory::resolve(&|name: &str| {
            command
                .get_envs()
                .find(|(variable, _value)| *variable == name)
                .and_then(|(_variable, value)| value.map(OsStr::to_os_string))
        })
        .unwrap()
        .path();
        MemberEvents::new(label, &traces)
    }

    /// Starts the binary's MCP server for the network, handshaken and with no card claimed.
    #[track_caller]
    pub fn start_server(&self) -> McpServer {
        let (process, events) = self.scratch.spawn_recorded_mcp_server();
        McpServer::handshaken(process, events)
    }

    /// Starts the binary's MCP server for the network on this machine, which the member then holds, and claims the card through its init tool, requiring the claim to succeed.
    #[track_caller]
    pub fn start_member(self, card: &str) -> ClaimedMember {
        let mut server = self.start_server();
        let claim = server.call(INIT_TOOL, &init_arguments(card));
        assert!(call_succeeded(&claim), "{claim}");
        ClaimedMember {
            server,
            machine: self,
        }
    }

    /// The hosts the network configuration names once one of them has the id, looked at once and again on each change in the configuration's directory, requiring that within the wait.
    #[track_caller]
    pub fn hosts_once_named(&self, id: &str, within: Duration) -> Vec<Host> {
        let config_file = self
            .scratch()
            .directories()
            .network_config_file(&self.scratch.network().parse().unwrap());
        let mut watch = GrowthWatch::of_path(config_file.parent().unwrap())
            .unwrap_or_else(|failure| panic!("{failure}"));
        let named = first_found_as_files_grow(
            &mut watch,
            within,
            Instant::now,
            || {},
            |_watch| {
                let hosts = NetworkConfig::load(&config_file).unwrap().hosts;
                hosts.iter().any(|host| host.id == id).then_some(hosts)
            },
        );
        match named {
            Ok(hosts) => hosts,
            Err(WaitEnded::RanOut) => {
                panic!("the network configuration did not name host {id} within {within:?}")
            }
            Err(WaitEnded::WatcherFailed(failure)) => {
                panic!("the file watcher failed: {failure}")
            }
            Err(WaitEnded::WatcherEnded) => panic!("the file watcher ended"),
        }
    }
}
