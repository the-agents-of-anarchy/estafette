use std::fmt;

use data_encoding::{DecodeError, HEXLOWER};

use crate::errors::ExitStatus;
use crate::wire::{self, CodecError, HostAddr, Invitation, Wire};

const INVITATION_PREFIX: &str = "estafette-invite-1:";
const HOST_LINE_PREFIX: &str = "estafette-host-1:";

/// Why an invitation or a host address line was refused, naming the only form accepted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InviteError {
    WrongPrefix { expected: &'static str },
    NotHexadecimal(DecodeError),
    NoHost,
    Codec(CodecError),
}

impl fmt::Display for InviteError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WrongPrefix { expected } => {
                write!(formatter, "the text must start with `{expected}`")
            }
            Self::NotHexadecimal(_) => write!(
                formatter,
                "after the prefix come lowercase hexadecimal digits 0-9 and a-f only, in pairs"
            ),
            Self::NoHost => write!(formatter, "an invitation names at least one host"),
            Self::Codec(source) => write!(formatter, "{source}"),
        }
    }
}

impl std::error::Error for InviteError {}

impl From<&InviteError> for ExitStatus {
    fn from(error: &InviteError) -> Self {
        match error {
            InviteError::WrongPrefix { .. }
            | InviteError::NotHexadecimal(_)
            | InviteError::NoHost
            | InviteError::Codec(_) => Self::UsageError,
        }
    }
}

/// Renders an invitation as `estafette-invite-1:` and the lowercase hexadecimal of its postcard encoding.
///
/// # Errors
///
/// Returns [`InviteError`].
pub fn render_invitation(invitation: &Invitation) -> Result<String, InviteError> {
    ensure_one_host(invitation)?;
    render_with_prefix(INVITATION_PREFIX, invitation)
}

/// Parses an invitation string, refusing any other prefix, non-hexadecimal text, or an invitation with no host.
///
/// # Errors
///
/// Returns [`InviteError`].
pub fn parse_invitation(text: &str) -> Result<Invitation, InviteError> {
    let invitation: Invitation = parse_with_prefix(INVITATION_PREFIX, text)?;
    ensure_one_host(&invitation)?;
    Ok(invitation)
}

/// Renders a host address line as `estafette-host-1:` and the lowercase hexadecimal of its postcard encoding.
///
/// # Errors
///
/// Returns [`InviteError`].
pub fn render_host_line(host: &HostAddr) -> Result<String, InviteError> {
    render_with_prefix(HOST_LINE_PREFIX, host)
}

/// Parses a host address line, refusing any other prefix or non-hexadecimal text.
///
/// # Errors
///
/// Returns [`InviteError`].
pub fn parse_host_line(text: &str) -> Result<HostAddr, InviteError> {
    parse_with_prefix(HOST_LINE_PREFIX, text)
}

const fn ensure_one_host(invitation: &Invitation) -> Result<(), InviteError> {
    if invitation.hosts.is_empty() {
        return Err(InviteError::NoHost);
    }
    Ok(())
}

fn render_with_prefix<T: Wire>(prefix: &str, structure: &T) -> Result<String, InviteError> {
    let bytes = wire::encode(structure).map_err(InviteError::Codec)?;
    Ok(format!("{prefix}{}", HEXLOWER.encode(&bytes)))
}

fn parse_with_prefix<T: Wire>(prefix: &'static str, text: &str) -> Result<T, InviteError> {
    let hex = text
        .strip_prefix(prefix)
        .ok_or(InviteError::WrongPrefix { expected: prefix })?;
    let bytes = HEXLOWER
        .decode(hex.as_bytes())
        .map_err(InviteError::NotHexadecimal)?;
    wire::decode(&bytes).map_err(InviteError::Codec)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::{RelayUrl, Version};

    const FIRST_HOST_ID_BYTE: u8 = 0xA1;
    const SECOND_HOST_ID_BYTE: u8 = 0xB2;
    const SECRET_BYTE: u8 = 0x5C;
    const OWNER_BYTE: u8 = 0x0F;
    const FIRST_RELAY: &str = "https://relay-one.example/";
    const SECOND_RELAY: &str = "https://relay-two.example.net/";
    const NETWORK_NAME: &str = "home-lab";

    fn host(id_byte: u8, relay: &str) -> HostAddr {
        HostAddr {
            v: Version::CURRENT,
            id: [id_byte; 32],
            relay: RelayUrl::try_from(relay.to_owned()).unwrap(),
        }
    }

    fn two_host_invitation() -> Invitation {
        Invitation {
            v: Version::CURRENT,
            network: NETWORK_NAME.to_owned(),
            secret: [SECRET_BYTE; 32],
            owner: [OWNER_BYTE; 32],
            hosts: vec![
                host(FIRST_HOST_ID_BYTE, FIRST_RELAY),
                host(SECOND_HOST_ID_BYTE, SECOND_RELAY),
            ],
        }
    }

    #[test]
    fn spec_3_13_an_invitation_parses_back_to_the_invitation_rendered() {
        let invitation = two_host_invitation();

        let text = render_invitation(&invitation).unwrap();

        assert_eq!(parse_invitation(&text), Ok(invitation));
    }

    const PINNED_HOST_LINE: &str = concat!(
        "estafette-host-1:",
        "01",
        "a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1",
        "1a",
        "68747470733a2f2f72656c61792d6f6e652e6578616d706c652f",
    );

    #[test]
    fn spec_3_13_a_host_line_is_the_prefix_and_lowercase_hex_of_the_postcard_host_addr() {
        let first_host = host(FIRST_HOST_ID_BYTE, FIRST_RELAY);

        let line = render_host_line(&first_host).unwrap();

        assert_eq!(line, PINNED_HOST_LINE);
        assert_eq!(parse_host_line(&line), Ok(first_host));
    }

    #[test]
    fn spec_3_13_a_string_with_the_other_kinds_prefix_is_refused_naming_the_prefix_accepted() {
        let invitation_text = render_invitation(&two_host_invitation()).unwrap();
        let host_line = render_host_line(&host(FIRST_HOST_ID_BYTE, FIRST_RELAY)).unwrap();

        let host_line_error = parse_host_line(&invitation_text).unwrap_err();
        let invitation_error = parse_invitation(&host_line).unwrap_err();

        assert_eq!(
            host_line_error,
            InviteError::WrongPrefix {
                expected: HOST_LINE_PREFIX
            }
        );
        assert_eq!(
            invitation_error.to_string(),
            "the text must start with `estafette-invite-1:`"
        );
        assert_eq!(
            host_line_error.to_string(),
            "the text must start with `estafette-host-1:`"
        );
    }

    #[test]
    fn spec_3_13_an_invitation_with_no_host_is_refused_in_both_directions() {
        let hostless = Invitation {
            hosts: Vec::new(),
            ..two_host_invitation()
        };
        let hostless_text = format!(
            "{INVITATION_PREFIX}{}",
            HEXLOWER.encode(&wire::encode(&hostless).unwrap())
        );

        assert_eq!(render_invitation(&hostless), Err(InviteError::NoHost));
        assert_eq!(parse_invitation(&hostless_text), Err(InviteError::NoHost));
    }

    #[test]
    fn spec_3_13_every_refusal_of_a_parse_ends_the_program_as_a_usage_error() {
        let hostless = Invitation {
            hosts: Vec::new(),
            ..two_host_invitation()
        };
        let hostless_text = format!(
            "{INVITATION_PREFIX}{}",
            HEXLOWER.encode(&wire::encode(&hostless).unwrap())
        );
        let refusals = [
            parse_host_line(&render_invitation(&two_host_invitation()).unwrap()).unwrap_err(),
            parse_host_line(&host_line_of_hex("0")).unwrap_err(),
            parse_invitation(&hostless_text).unwrap_err(),
            parse_host_line(&host_line_of_hex(&format!("{}00", pinned_host_hex()))).unwrap_err(),
        ];

        let statuses = refusals.each_ref().map(ExitStatus::from);

        assert_eq!(statuses, [ExitStatus::UsageError; 4]);
        assert_eq!(u8::from(statuses[0]), 2);
    }

    const RELAY_CAP_BYTES: usize = 128;
    const RELAY_LENGTH_VARINT_OF_129: &str = "8101";

    fn host_line_of_hex(hex: &str) -> String {
        format!("{HOST_LINE_PREFIX}{hex}")
    }

    fn pinned_host_hex() -> &'static str {
        PINNED_HOST_LINE.strip_prefix(HOST_LINE_PREFIX).unwrap()
    }

    #[test]
    fn spec_3_13_a_host_line_with_truncated_hex_is_refused() {
        let hex = pinned_host_hex();
        let cut_in_a_byte = host_line_of_hex(hex.strip_suffix(|_: char| true).unwrap());
        let cut_between_bytes = host_line_of_hex(hex.strip_suffix("2f").unwrap());

        assert!(matches!(
            parse_host_line(&cut_in_a_byte),
            Err(InviteError::NotHexadecimal(_))
        ));
        assert!(matches!(
            parse_host_line(&cut_between_bytes),
            Err(InviteError::Codec(CodecError::Malformed(_)))
        ));
    }

    #[test]
    fn spec_3_13_hex_must_be_lowercase_digits_only() {
        let hex = pinned_host_hex();
        let uppercase = host_line_of_hex(&hex.to_uppercase());
        let with_a_stray_letter = host_line_of_hex(&hex.replacen("01", "0g", 1));

        assert!(matches!(
            parse_host_line(&uppercase),
            Err(InviteError::NotHexadecimal(_))
        ));
        assert!(matches!(
            parse_host_line(&with_a_stray_letter),
            Err(InviteError::NotHexadecimal(_))
        ));
    }

    #[test]
    fn spec_3_13_a_host_line_with_bytes_after_the_structure_is_refused() {
        let with_a_trailing_byte = host_line_of_hex(&format!("{}00", pinned_host_hex()));

        assert_eq!(
            parse_host_line(&with_a_trailing_byte),
            Err(InviteError::Codec(CodecError::TrailingBytes))
        );
    }

    #[test]
    fn spec_3_13_a_relay_url_past_128_bytes_is_refused_when_parsing() {
        let version_and_id = format!("01{}", "a1".repeat(32));
        let long_relay = "61".repeat(RELAY_CAP_BYTES + 1);
        let line = host_line_of_hex(&format!(
            "{version_and_id}{RELAY_LENGTH_VARINT_OF_129}{long_relay}"
        ));

        assert_eq!(
            parse_host_line(&line),
            Err(InviteError::Codec(CodecError::RelayUrlTooLong {
                bytes: RELAY_CAP_BYTES + 1
            }))
        );
    }
}
