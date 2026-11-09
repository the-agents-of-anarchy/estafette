use std::cell::Cell;
use std::ffi::OsString;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::Command;

use estafette::dirs::{Directories, OperatingSystemFilesystem};
use estafette::keys::parse_public_key;
use iroh::EndpointId;

use crate::member::Member;

const BINARY: &str = env!("CARGO_BIN_EXE_estafette");
const SHORT_SCRATCH_PARENT: &str = "/tmp";
const SCRATCH_PREFIX: &str = "ea";
const CONFIG_HOME_VARIABLE: &str = "XDG_CONFIG_HOME";
const DATA_HOME_VARIABLE: &str = "XDG_DATA_HOME";
const RUNTIME_DIRECTORY_VARIABLE: &str = "XDG_RUNTIME_DIR";
const SEARCH_PATH_VARIABLE: &str = "PATH";
const CONFIG_HOME: &str = "c";
const DATA_HOME: &str = "d";
const RUNTIME_DIRECTORY: &str = "r";
const RUN_RECORD_EXTENSION: &str = "stderr";

/// One machine's worth of estafette state under a scratch root of its own, short enough for an inbox socket path, configured for one network.
pub struct Machine {
    root: PathBuf,
    network: String,
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
        let root = PathBuf::from(SHORT_SCRATCH_PARENT).join(format!(
            "{SCRATCH_PREFIX}-{}-{test_tag}",
            std::process::id()
        ));
        drop(fs::remove_dir_all(&root));
        fs::create_dir_all(&root).unwrap();
        Self {
            root,
            network: network.to_owned(),
            members_started: Cell::new(0),
        }
    }

    /// Runs the binary to completion on this machine, requiring success, and returns what it printed.
    pub fn run(&self, arguments: &[&str]) -> String {
        let output = self.command().args(arguments).output().unwrap();
        assert!(output.status.success(), "{arguments:?}: {output:?}");
        String::from_utf8(output.stdout).unwrap()
    }

    /// Creates a card requesting the handle by the binary's own command.
    pub fn create_card(&self, name: &str, handle: &str) -> Card {
        let printed_key = self.run(&["create-card", &self.network, name, handle]);
        Card {
            name: name.to_owned(),
            id: parse_public_key(printed_key.trim_end()).unwrap(),
        }
    }

    /// Starts the binary's MCP server for the network, handshaken and with no card claimed, its run record kept in a file of the scratch root.
    pub fn start_member(&self) -> Member {
        let started = self.members_started.get();
        self.members_started.set(started.checked_add(1).unwrap());
        let run_record = File::create(
            self.root
                .join(format!("member-{started}.{RUN_RECORD_EXTENSION}")),
        )
        .unwrap();
        let mut command = self.command();
        command.args(["mcp", &self.network]);
        Member::start(command, run_record)
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

    /// The binary with this machine's environment alone.
    fn command(&self) -> Command {
        let mut command = Command::new(BINARY);
        command.env_clear().envs(environment(&self.root));
        command
    }
}

impl Drop for Machine {
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
