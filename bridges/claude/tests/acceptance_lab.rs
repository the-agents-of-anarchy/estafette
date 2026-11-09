use std::ffi::OsStr;
use std::fs;
use std::io::{self, BufRead, BufReader};
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::process::Output;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::Duration;

use common::{Fixture, MESSAGING_TOKEN};
use serde_json::Value;

mod common;

const INVITATION_VARIABLE: &str = "ESTAFETTE_ACCEPTANCE_INVITATION";
const JOINED_NETWORKS_DIRECTORY: &str = "config/estafette/networks";
const DIGIT_PLACEHOLDER: char = 'D';
const FIELD_SEPARATOR: char = '\t';
const SEND_RESULT_FIELD_COUNT: usize = 3;
const PEER_ATTACHED_FIELD: usize = 2;
const NOTICE_LINE_COUNT: usize = 2;
const DELIVERED_STATUS: &str = "delivered";
const ATTACHED_FLAG: &str = "true";
const USER_NOTICE_TYPE: &str = "user";
const NETWORK_CONFIG_EXTENSION: &str = "toml";
const SENDER_HANDLE: &str = "operator";
const BODY: &str = "ACCEPTANCE-HELLO";
const ISO_8601_UTC_SHAPE: &str = "DDDD-DD-DDTDD:DD:DDZ";
const ATTACHED_WAIT_ATTEMPTS: u32 = 60;
const RETRY_PAUSE: Duration = Duration::from_secs(1);
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
struct SendResult<'line> {
    status: &'line str,
    attached: &'line str,
}

impl<'line> SendResult<'line> {
    fn parse(line: &'line str) -> Option<Self> {
        let fields: Vec<&str> = line.trim_end().split(FIELD_SEPARATOR).collect();
        match fields.as_slice() {
            [status, _id, attached] if fields.len() == SEND_RESULT_FIELD_COUNT => {
                Some(Self { status, attached })
            }
            _ => None,
        }
    }
}

fn has_iso_8601_utc_shape(text: &str) -> bool {
    text.len() == ISO_8601_UTC_SHAPE.len()
        && text
            .chars()
            .zip(ISO_8601_UTC_SHAPE.chars())
            .all(|(actual, shape)| {
                if shape == DIGIT_PLACEHOLDER {
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
            let fields: Vec<&str> = line.split(FIELD_SEPARATOR).collect();
            fields.first() == Some(&card) && fields.get(PEER_ATTACHED_FIELD) == Some(&ATTACHED_FLAG)
        }))
}

fn test_send_until_delivered(fixture: &Fixture, network: &str, card: &str) -> io::Result<Output> {
    let mut delivery = fixture.run_estafette(&["test-send", network, card, BODY])?;
    for _ in 1..TEST_SEND_ATTEMPTS {
        if delivery.status.success() {
            break;
        }
        thread::sleep(RETRY_PAUSE);
        delivery = fixture.run_estafette(&["test-send", network, card, BODY])?;
    }
    Ok(delivery)
}

fn wait_until_bridge_attached(fixture: &Fixture, network: &str, card: &str) -> io::Result<()> {
    for _ in 0..ATTACHED_WAIT_ATTEMPTS {
        if attached_peer_listed(fixture, network, card)? {
            return Ok(());
        }
        thread::sleep(RETRY_PAUSE);
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
    fixture
        .run_estafette_successfully(&["network", "join", &invitation])
        .unwrap();
    let network = joined_network_name(&fixture.root).unwrap();
    fixture
        .run_estafette_successfully(&["create-card", &network, &card, &card])
        .unwrap();
    let notices = collect_connections(UnixListener::bind(fixture.messaging_socket()).unwrap());
    let member = fixture.spawn_claimed_member(&network, &card).unwrap();
    assert_eq!(member.claimed["ok"], true, "{}", member.claimed);
    assert_eq!(
        member.claimed["you"]["handle"],
        card.as_str(),
        "{}",
        member.claimed
    );
    let _bridge = fixture.spawn_bridge().unwrap();

    wait_until_bridge_attached(&fixture, &network, &card).unwrap();
    let delivery = test_send_until_delivered(&fixture, &network, &card).unwrap();
    assert_eq!(delivery.status.code(), Some(0), "{delivery:?}");
    let result_line = String::from_utf8(delivery.stdout).unwrap();
    let result = SendResult::parse(&result_line).unwrap();
    assert_eq!(result.status, DELIVERED_STATUS, "{result_line}");
    assert_eq!(result.attached, ATTACHED_FLAG, "{result_line}");

    let first = notices.recv_timeout(NOTICE_TIMEOUT).unwrap();
    assert_eq!(first.len(), NOTICE_LINE_COUNT, "{first:?}");
    assert_eq!(
        &first[0],
        &format!(r#"{{"type":"auth","token":"{MESSAGING_TOKEN}"}}"#)
    );
    let notice: Value = serde_json::from_str(&first[1]).unwrap();
    assert_eq!(notice["type"], USER_NOTICE_TYPE, "{notice}");
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
    assert_eq!(
        notices.recv_timeout(NO_SECOND_NOTICE_WINDOW),
        Err(RecvTimeoutError::Timeout)
    );
}
