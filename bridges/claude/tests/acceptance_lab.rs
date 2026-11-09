use std::ffi::OsStr;
use std::fs;
use std::io::{self, BufRead, BufReader};
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::process::{Output, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::Duration;

use common::{Fixture, MESSAGING_TOKEN, Spawned, claim_card_over_mcp, forward_json_lines};
use serde_json::Value;

mod common;

const INVITATION_VARIABLE: &str = "ESTAFETTE_ACCEPTANCE_INVITATION";
const JOINED_NETWORKS_DIRECTORY: &str = "config/estafette/networks";
const NETWORK_CONFIG_EXTENSION: &str = "toml";
const SENDER_HANDLE: &str = "operator";
const BODY: &str = "ACCEPTANCE-HELLO";
const ISO_8601_UTC_SHAPE: &str = "DDDD-DD-DDTDD:DD:DDZ";
const ATTACHED_WAIT_ATTEMPTS: u32 = 60;
const SHOW_PEERS_PAUSE: Duration = Duration::from_secs(1);
const TEST_SEND_ATTEMPTS: u32 = 10;
const NOTICE_TIMEOUT: Duration = Duration::from_secs(30);
const NO_SECOND_NOTICE_WINDOW: Duration = Duration::from_secs(5);

fn collect_connections(listener: UnixListener) -> Receiver<Vec<String>> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        for connection in listener.incoming().map_while(Result::ok) {
            let lines = BufReader::new(connection)
                .lines()
                .map_while(Result::ok)
                .collect();
            if sender.send(lines).is_err() {
                break;
            }
        }
    });
    receiver
}

fn invitation_from_variable(name: &str) -> Result<String, String> {
    std::env::var(name).map_err(|source| {
        format!(
            "{name} is not usable ({source}): run `just gate::acceptance`, which exports it from the live lab host"
        )
    })
}

fn joined_network_name(root: &Path) -> io::Result<String> {
    for entry in fs::read_dir(root.join(JOINED_NETWORKS_DIRECTORY))? {
        let path = entry?.path();
        if path.extension().and_then(OsStr::to_str) == Some(NETWORK_CONFIG_EXTENSION)
            && let Some(name) = path.file_stem().and_then(OsStr::to_str)
        {
            return Ok(name.to_owned());
        }
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "joining wrote no network configuration",
    ))
}
fn has_iso_8601_utc_shape(text: &str) -> bool {
    text.len() == ISO_8601_UTC_SHAPE.len()
        && text
            .chars()
            .zip(ISO_8601_UTC_SHAPE.chars())
            .all(|(actual, shape)| {
                if shape == 'D' {
                    actual.is_ascii_digit()
                } else {
                    actual == shape
                }
            })
}

fn attached_peer_listed(fixture: &Fixture, network: &str, card: &str) -> io::Result<bool> {
    let listing = fixture.run_estafette(&["show-peers", network])?;
    Ok(String::from_utf8_lossy(&listing.stdout)
        .lines()
        .any(|line| {
            let fields: Vec<&str> = line.split('\t').collect();
            fields.first() == Some(&card) && fields.get(2) == Some(&"true")
        }))
}

fn test_send_until_delivered(fixture: &Fixture, network: &str, card: &str) -> io::Result<Output> {
    let mut delivery = fixture.run_estafette(&["test-send", network, card, BODY])?;
    for _ in 1..TEST_SEND_ATTEMPTS {
        if delivery.status.success() {
            break;
        }
        delivery = fixture.run_estafette(&["test-send", network, card, BODY])?;
    }
    Ok(delivery)
}

fn wait_until_bridge_attached(fixture: &Fixture, network: &str, card: &str) -> io::Result<()> {
    for _ in 0..ATTACHED_WAIT_ATTEMPTS {
        if attached_peer_listed(fixture, network, card)? {
            return Ok(());
        }
        thread::sleep(SHOW_PEERS_PAUSE);
    }
    Err(io::Error::new(
        io::ErrorKind::TimedOut,
        format!("the bridge never attached to the inbox socket of {card}"),
    ))
}

#[test]
fn spec_3_12_the_bridge_injects_one_notice_per_inbox_frame_of_a_real_binary_joined_through_the_lab_host()
 {
    let invitation = invitation_from_variable(INVITATION_VARIABLE).unwrap();
    let card = format!("acc-{}", std::process::id());
    let fixture = Fixture::new().unwrap();
    assert_eq!(
        fixture
            .run_estafette(&["network", "join", &invitation])
            .unwrap()
            .status
            .code(),
        Some(0)
    );
    let network = joined_network_name(&fixture.root).unwrap();
    assert_eq!(
        fixture
            .run_estafette(&["create-card", &network, &card, &card])
            .unwrap()
            .status
            .code(),
        Some(0)
    );
    let notices = collect_connections(UnixListener::bind(fixture.messaging_socket()).unwrap());
    let mut member = Spawned(
        fixture
            .estafette()
            .args(["mcp", &network])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let mut member_input = member.0.stdin.take().unwrap();
    let answers = forward_json_lines(member.0.stdout.take().unwrap());
    let claimed = claim_card_over_mcp(&mut member_input, &answers, &card).unwrap();
    assert_eq!(claimed["ok"], true, "{claimed}");
    assert_eq!(claimed["you"]["handle"], card.as_str(), "{claimed}");
    let _bridge = Spawned(
        fixture
            .bridge()
            .args(["run", &std::process::id().to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );

    wait_until_bridge_attached(&fixture, &network, &card).unwrap();
    let delivery = test_send_until_delivered(&fixture, &network, &card).unwrap();
    assert_eq!(delivery.status.code(), Some(0), "{delivery:?}");
    let result_line = String::from_utf8(delivery.stdout).unwrap();
    let result_fields: Vec<&str> = result_line.trim_end().split('\t').collect();
    assert_eq!(result_fields.len(), 3, "{result_line}");
    assert_eq!(result_fields[0], "delivered", "{result_line}");
    assert_eq!(result_fields[2], "true", "{result_line}");

    let first = notices.recv_timeout(NOTICE_TIMEOUT).unwrap();
    assert_eq!(first.len(), 2, "{first:?}");
    assert_eq!(
        &first[0],
        &format!(r#"{{"type":"auth","token":"{MESSAGING_TOKEN}"}}"#)
    );
    let notice: Value = serde_json::from_str(&first[1]).unwrap();
    assert_eq!(notice["type"], "user", "{notice}");
    let text = notice["message"]["content"].as_str().unwrap();
    let after_prefix = text
        .strip_prefix(&format!(
            "Message from peer {SENDER_HANDLE} (verified member of your network), sent "
        ))
        .unwrap();
    let sent_at = after_prefix
        .strip_suffix(&format!(".\n{BODY}\nReply with the send tool."))
        .unwrap();
    assert!(has_iso_8601_utc_shape(sent_at), "{text}");
    notices.recv_timeout(NO_SECOND_NOTICE_WINDOW).unwrap_err();
}
