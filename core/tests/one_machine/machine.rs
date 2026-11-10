use std::cell::Cell;

use estafette::dirs::Directories;
use estafette::keys::parse_public_key;
use iroh::EndpointId;

use crate::member::Member;
use crate::scratch_machine::ScratchMachine;

/// One machine's worth of estafette state under a scratch root of its own, counting the members it started so each keeps its own run record.
pub struct Machine {
    scratch: ScratchMachine,
    members_started: Cell<u32>,
}

/// A card the binary created: the name init claims it by and the node id its key gives.
pub struct Card {
    pub name: String,
    pub id: EndpointId,
}

impl Machine {
    /// An empty scratch root named for the test and the machine, for the network of that name.
    pub fn fresh(test_tag: &str, network: &str) -> Self {
        Self {
            scratch: ScratchMachine::fresh(test_tag, network),
            members_started: Cell::new(0),
        }
    }

    /// Runs the binary to completion on this machine, requiring success, and returns what it printed.
    pub fn run(&self, arguments: &[&str]) -> String {
        self.scratch.run(arguments)
    }

    /// Creates a card requesting the handle by the binary's own command.
    pub fn create_card(&self, name: &str, handle: &str) -> Card {
        let printed_key = self.run(&["create-card", self.scratch.network(), name, handle]);
        Card {
            name: name.to_owned(),
            id: parse_public_key(printed_key.trim_end()).unwrap(),
        }
    }

    /// Starts the binary's MCP server for the network, handshaken and with no card claimed, its run record kept in a file of the scratch root.
    pub fn start_member(&self) -> Member {
        let started = self.members_started.get();
        self.members_started.set(started.checked_add(1).unwrap());
        Member::start(self.scratch.spawn_mcp_server(&format!("member-{started}")))
    }

    /// The directories the binary derives from this machine's environment.
    pub fn directories(&self) -> Directories {
        self.scratch.directories()
    }
}
