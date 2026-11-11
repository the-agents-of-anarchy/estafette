use std::time::{SystemTime, UNIX_EPOCH};

/// How many clock-drawn letters follow the base of a unique handle.
pub const HANDLE_TAG_LETTERS: usize = 6;
/// How many letters a handle letter is drawn from.
const ALPHABET_LENGTH: u64 = 26;
/// The first letter a handle letter is drawn from.
const FIRST_LETTER: u8 = b'a';

/// The base followed by letters drawn from the clock and the process, so the handle, and a card named for it, is free on the live network and on every lab server whatever earlier runs left.
pub fn unique_handle(base: &str) -> String {
    let since_epoch = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
    let mut drawn = u64::from(since_epoch.subsec_nanos())
        ^ since_epoch.as_secs()
        ^ u64::from(std::process::id());
    let mut handle = base.to_owned();
    for _ in 0..HANDLE_TAG_LETTERS {
        let letter = u8::try_from(drawn % ALPHABET_LENGTH).unwrap();
        handle.push(char::from(FIRST_LETTER.checked_add(letter).unwrap()));
        drawn /= ALPHABET_LENGTH;
    }
    handle
}
