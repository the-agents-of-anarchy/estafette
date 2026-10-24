use data_encoding::{HEXLOWER, HEXLOWER_PERMISSIVE};
use iroh::{PublicKey, SecretKey};
use std::fmt;

const KEY_BYTES: usize = 32;
const HEX_CHARS_PER_KEY: usize = KEY_BYTES * 2;
const SECRET_KEY_LINE_END: char = '\n';

/// Why a key text was refused, naming the only form accepted.
#[derive(Debug, PartialEq, Eq)]
pub enum KeyError {
    WrongLength { found: usize },
    NotHexadecimal,
    NotAnEd25519PublicKey,
}

impl fmt::Display for KeyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WrongLength { found } => write!(
                formatter,
                "a key is exactly {HEX_CHARS_PER_KEY} hexadecimal characters, found {found}"
            ),
            Self::NotHexadecimal => write!(
                formatter,
                "a key is made of the hexadecimal digits 0-9 and a-f only"
            ),
            Self::NotAnEd25519PublicKey => {
                write!(formatter, "the 32 bytes are not an Ed25519 public key")
            }
        }
    }
}

impl std::error::Error for KeyError {}

/// The 32 random bytes every member of a network shares.
#[derive(Clone, PartialEq, Eq)]
pub struct NetworkSecret([u8; KEY_BYTES]);

impl fmt::Debug for NetworkSecret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "NetworkSecret(..)")
    }
}

impl NetworkSecret {
    /// The secret's 32 bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; KEY_BYTES] {
        &self.0
    }
}

/// Reads a card public key or owner public key from 64 hexadecimal characters of either case.
///
/// # Errors
///
/// Refuses text that is not 64 hexadecimal characters or whose bytes are no Ed25519 public key.
pub fn parse_public_key(text: &str) -> Result<PublicKey, KeyError> {
    let bytes = decode_key_bytes(text)?;
    PublicKey::from_bytes(&bytes)
        .ok()
        .ok_or(KeyError::NotAnEd25519PublicKey)
}

/// Writes a public key as 64 lowercase hexadecimal characters.
#[must_use]
pub fn render_public_key(key: &PublicKey) -> String {
    HEXLOWER.encode(key.as_bytes())
}

/// Reads a seed from 64 hexadecimal characters of either case and an optional final newline.
///
/// # Errors
///
/// Refuses text that is not 64 hexadecimal characters, apart from one final newline.
pub fn parse_secret_key(text: &str) -> Result<SecretKey, KeyError> {
    let without_line_end = text.strip_suffix(SECRET_KEY_LINE_END).unwrap_or(text);
    decode_key_bytes(without_line_end).map(|bytes| SecretKey::from_bytes(&bytes))
}

/// Writes a seed as 64 lowercase hexadecimal characters and one newline.
#[must_use]
pub fn render_secret_key(key: &SecretKey) -> String {
    let mut line = HEXLOWER.encode(&key.to_bytes());
    line.push(SECRET_KEY_LINE_END);
    line
}

/// Reads a network secret from 64 hexadecimal characters of either case.
///
/// # Errors
///
/// Refuses text that is not 64 hexadecimal characters.
pub fn parse_network_secret(text: &str) -> Result<NetworkSecret, KeyError> {
    decode_key_bytes(text).map(NetworkSecret)
}

/// Writes a network secret as 64 lowercase hexadecimal characters.
#[must_use]
pub fn render_network_secret(secret: &NetworkSecret) -> String {
    HEXLOWER.encode(secret.as_bytes())
}

/// Makes an Ed25519 key from 32 bytes the secure source writes.
pub fn generate_secret_key(
    fill_from_secure_source: impl FnOnce(&mut [u8; KEY_BYTES]),
) -> SecretKey {
    SecretKey::from_bytes(&draw_key_bytes(fill_from_secure_source))
}

/// Makes a network secret from 32 bytes the secure source writes.
pub fn generate_network_secret(
    fill_from_secure_source: impl FnOnce(&mut [u8; KEY_BYTES]),
) -> NetworkSecret {
    NetworkSecret(draw_key_bytes(fill_from_secure_source))
}

fn draw_key_bytes(fill_from_secure_source: impl FnOnce(&mut [u8; KEY_BYTES])) -> [u8; KEY_BYTES] {
    let mut bytes = [0; KEY_BYTES];
    fill_from_secure_source(&mut bytes);
    bytes
}

fn decode_key_bytes(text: &str) -> Result<[u8; KEY_BYTES], KeyError> {
    if text.len() != HEX_CHARS_PER_KEY {
        return Err(KeyError::WrongLength { found: text.len() });
    }
    let decoded = HEXLOWER_PERMISSIVE
        .decode(text.as_bytes())
        .ok()
        .ok_or(KeyError::NotHexadecimal)?;
    decoded
        .try_into()
        .ok()
        .ok_or(KeyError::WrongLength { found: text.len() })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SEED_HEX: &str = "9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60";
    const PUBLIC_HEX: &str = "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a";

    fn bytes_seeded_by(index: usize) -> [u8; KEY_BYTES] {
        *blake3::hash(&index.to_le_bytes()).as_bytes()
    }

    fn fill_seeded_by(index: usize) -> impl FnOnce(&mut [u8; KEY_BYTES]) {
        move |buffer| *buffer = bytes_seeded_by(index)
    }

    #[test]
    fn public_key_derives_from_a_seed_of_either_case_with_or_without_newline() {
        for text in [
            SEED_HEX.to_owned(),
            format!("{SEED_HEX}\n"),
            SEED_HEX.to_uppercase(),
            format!("{}\n", SEED_HEX.to_uppercase()),
        ] {
            let seed = parse_secret_key(&text).unwrap();
            assert_eq!(render_public_key(&seed.public()), PUBLIC_HEX, "{text:?}");
        }
    }

    #[test]
    fn public_key_round_trips_through_text_for_many_keys() {
        for index in 0..64 {
            let key = generate_secret_key(fill_seeded_by(index)).public();
            let text = render_public_key(&key);
            assert_eq!(text.len(), HEX_CHARS_PER_KEY);
            assert_eq!(text, text.to_lowercase());
            assert_eq!(parse_public_key(&text).unwrap(), key);
            assert_eq!(parse_public_key(&text.to_uppercase()).unwrap(), key);
        }
    }

    #[test]
    fn public_key_text_is_the_nodes_own_display_form() {
        let key = generate_secret_key(fill_seeded_by(7)).public();
        assert_eq!(render_public_key(&key), key.to_string());
    }

    #[test]
    fn secret_key_renders_lowercase_with_one_newline_and_round_trips() {
        for index in 0..64 {
            let key = generate_secret_key(fill_seeded_by(index));
            let line = render_secret_key(&key);
            assert_eq!(line.len(), HEX_CHARS_PER_KEY + 1);
            assert!(line.ends_with('\n'));
            assert_eq!(line, line.to_lowercase());
            assert_eq!(parse_secret_key(&line).unwrap().to_bytes(), key.to_bytes());
        }
    }

    #[test]
    fn secret_key_refuses_a_second_newline() {
        assert_eq!(
            parse_secret_key(&format!("{SEED_HEX}\n\n")).unwrap_err(),
            KeyError::WrongLength {
                found: HEX_CHARS_PER_KEY + 1
            }
        );
    }

    #[test]
    fn every_key_form_refuses_63_and_65_characters() {
        for found in [HEX_CHARS_PER_KEY - 1, HEX_CHARS_PER_KEY + 1] {
            let text: String = PUBLIC_HEX.repeat(2).chars().take(found).collect();
            let text = text.as_str();
            let wrong_length = KeyError::WrongLength { found };
            assert_eq!(parse_public_key(text).unwrap_err(), wrong_length);
            assert_eq!(parse_secret_key(text).unwrap_err(), wrong_length);
            assert_eq!(parse_network_secret(text).unwrap_err(), wrong_length);
        }
    }

    #[test]
    fn every_key_form_refuses_a_character_that_is_not_hexadecimal() {
        let text = SEED_HEX.replacen('9', "g", 1);
        assert_eq!(
            parse_public_key(&text).unwrap_err(),
            KeyError::NotHexadecimal
        );
        assert_eq!(
            parse_secret_key(&text).unwrap_err(),
            KeyError::NotHexadecimal
        );
        assert_eq!(
            parse_network_secret(&text).unwrap_err(),
            KeyError::NotHexadecimal
        );
    }

    #[test]
    fn public_key_refuses_32_bytes_that_are_not_a_curve_point() {
        let not_a_point = format!("02{}", "00".repeat(KEY_BYTES - 1));
        assert_eq!(
            parse_public_key(&not_a_point).unwrap_err(),
            KeyError::NotAnEd25519PublicKey
        );
    }

    #[test]
    fn generated_keys_carry_exactly_the_bytes_the_source_wrote() {
        for index in 0..64 {
            let expected = bytes_seeded_by(index);
            assert_eq!(
                generate_secret_key(fill_seeded_by(index)).to_bytes(),
                expected
            );
            assert_eq!(
                generate_network_secret(fill_seeded_by(index)).as_bytes(),
                &expected
            );
        }
    }

    #[test]
    fn network_secret_renders_lowercase_without_newline_and_round_trips() {
        for index in 0..64 {
            let secret = generate_network_secret(fill_seeded_by(index));
            let text = render_network_secret(&secret);
            assert_eq!(text.len(), HEX_CHARS_PER_KEY);
            assert_eq!(text, text.to_lowercase());
            assert_eq!(parse_network_secret(&text).unwrap(), secret);
            assert_eq!(parse_network_secret(&text.to_uppercase()).unwrap(), secret);
        }
    }

    #[test]
    fn refusals_name_the_accepted_form() {
        assert_eq!(
            KeyError::WrongLength { found: 63 }.to_string(),
            "a key is exactly 64 hexadecimal characters, found 63"
        );
    }
}
