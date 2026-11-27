use estafette::crypto::{MAC_LEN, Mac};
use estafette::wire::{
    Addr, Introduce, IntroduceAck, MessageAck, Request, Response, Version, encode,
};

const SPEC_PROTOCOL_VERSION: u8 = 1;
const SPEC_MESSAGE_ID_BYTES: usize = 16;
const INTRODUCE_VARIANT_TAG: u8 = 2;
const MESSAGE_ACK_VARIANT_TAG: u8 = 0;
const IPV4_VARIANT_TAG: u8 = 0;
const FALSE_FLAG: u8 = 0;
const TRUE_FLAG: u8 = 1;
const SOME_TAG: u8 = 1;
const ONE_ENTRY: u8 = 1;
const TWO_ENTRIES: u8 = 2;
const PINNED_MAC_BYTE: u8 = 0xA1;
const PINNED_ID_BYTE: u8 = 0xE6;
const PINNED_PUBLIC_BYTE: u8 = 0x09;
const PINNED_HANDLE: &str = "ab";
const PINNED_REASON: &str = "r";
const PINNED_RELAYS: [&str; 2] = ["r1", "r2"];
const FIRST_DIRECT_OCTETS: [u8; 4] = [10, 1, 2, 3];
const SECOND_DIRECT_OCTETS: [u8; 4] = [10, 4, 5, 6];
const FIRST_DIRECT_PORT: u8 = 7;
const SECOND_DIRECT_PORT: u8 = 8;

fn pinned_addr() -> Addr {
    Addr::from_endpoint_report(
        PINNED_RELAYS.map(str::to_owned),
        [
            std::net::SocketAddr::from((FIRST_DIRECT_OCTETS, u16::from(FIRST_DIRECT_PORT))),
            std::net::SocketAddr::from((SECOND_DIRECT_OCTETS, u16::from(SECOND_DIRECT_PORT))),
        ],
    )
}

fn pinned_addr_bytes() -> Vec<u8> {
    let relay = |numeral: u8| [TWO_ENTRIES, b'r', numeral];
    let direct = |octets: [u8; 4], port: u8| [&[IPV4_VARIANT_TAG][..], &octets, &[port]].concat();
    [
        &[TWO_ENTRIES][..],
        &relay(b'1'),
        &relay(b'2'),
        &[TWO_ENTRIES],
        &direct(FIRST_DIRECT_OCTETS, FIRST_DIRECT_PORT),
        &direct(SECOND_DIRECT_OCTETS, SECOND_DIRECT_PORT),
    ]
    .concat()
}

fn introduction_bytes() -> Vec<u8> {
    [
        &[SOME_TAG, TWO_ENTRIES, b'a', b'b'][..],
        &[ONE_ENTRY, PINNED_PUBLIC_BYTE, TRUE_FLAG],
        &pinned_addr_bytes(),
        &[PINNED_MAC_BYTE; MAC_LEN],
    ]
    .concat()
}

#[test]
fn spec_3_6_introduce_carries_its_handle_public_attached_flag_address_and_mac_in_that_order() {
    let introduce = Request::Introduce(Introduce {
        v: Version::CURRENT,
        handle: Some(PINNED_HANDLE.to_owned()),
        public: vec![PINNED_PUBLIC_BYTE],
        attached: true,
        addr: pinned_addr(),
        mac: Mac::from_bytes([PINNED_MAC_BYTE; MAC_LEN]),
    });

    assert_eq!(
        encode(&introduce).unwrap(),
        [
            &[INTRODUCE_VARIANT_TAG, SPEC_PROTOCOL_VERSION][..],
            &introduction_bytes(),
        ]
        .concat()
    );
}

#[test]
fn spec_3_6_introduce_ack_carries_its_handle_public_attached_flag_address_and_mac_in_that_order() {
    let introduce_ack = Response::IntroduceAck(IntroduceAck {
        v: Version::CURRENT,
        handle: Some(PINNED_HANDLE.to_owned()),
        public: vec![PINNED_PUBLIC_BYTE],
        attached: true,
        addr: pinned_addr(),
        mac: Mac::from_bytes([PINNED_MAC_BYTE; MAC_LEN]),
    });

    assert_eq!(
        encode(&introduce_ack).unwrap(),
        [
            &[INTRODUCE_VARIANT_TAG, SPEC_PROTOCOL_VERSION][..],
            &introduction_bytes(),
        ]
        .concat()
    );
}

#[test]
fn spec_3_6_a_refusing_message_ack_carries_its_id_accepted_flag_attached_flag_and_reason_in_that_order()
 {
    let refused = Response::MessageAck(MessageAck {
        v: Version::CURRENT,
        id: [PINNED_ID_BYTE; SPEC_MESSAGE_ID_BYTES],
        accepted: false,
        attached: true,
        reason: Some(PINNED_REASON.to_owned()),
    });

    assert_eq!(
        encode(&refused).unwrap(),
        [
            &[MESSAGE_ACK_VARIANT_TAG, SPEC_PROTOCOL_VERSION][..],
            &[PINNED_ID_BYTE; SPEC_MESSAGE_ID_BYTES],
            &[FALSE_FLAG, TRUE_FLAG, SOME_TAG, ONE_ENTRY, b'r'],
        ]
        .concat()
    );
}
