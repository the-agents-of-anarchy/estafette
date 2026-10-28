use std::fmt;
use std::fs::{File, OpenOptions, TryLockError};
use std::io::{self, Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::time::Duration;

use data_encoding::{HEXLOWER, HEXLOWER_PERMISSIVE};
use iroh::{PublicKey, SecretKey};

const OWNER_READ_WRITE_ONLY: u32 = 0o600;
const CARD_FILE_MODE: u32 = 0o644;
const PERMISSION_BITS_MASK: u32 = 0o777;
pub const SEED_BYTES: usize = 32;
const PUBLIC_KEY_BYTES: usize = 32;
const LOCK_RETRY_WINDOW: Duration = Duration::from_millis(200);
const LOCK_RETRY_INTERVAL: Duration = Duration::from_millis(10);

/// Why a sidecar fails the claim checks of spec 3.5, all reported as `sidecar_invalid`.
#[derive(Debug, PartialEq, Eq)]
pub enum InvalidSidecar {
    Missing,
    PermissionsNotOwnerReadWrite { mode: u32 },
    SeedNotThirtyTwoHexBytes,
    PublicKeyDiffersFromCard,
}

/// A refusal or failure of reading, locking, or creating a card with its sidecar.
#[derive(Debug)]
pub enum SidecarError {
    Invalid(InvalidSidecar),
    CardHeld,
    AlreadyExists,
    Io(io::Error),
}

impl fmt::Display for SidecarError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(InvalidSidecar::Missing) => formatter.write_str(
                "the sidecar of the card does not exist; only `create card` mints one",
            ),
            Self::Invalid(InvalidSidecar::PermissionsNotOwnerReadWrite { mode }) => write!(
                formatter,
                "the sidecar has permissions {mode:04o}; only 0600 is accepted"
            ),
            Self::Invalid(InvalidSidecar::SeedNotThirtyTwoHexBytes) => formatter.write_str(
                "the sidecar holds no 64 hexadecimal characters of either case, with an optional final newline",
            ),
            Self::Invalid(InvalidSidecar::PublicKeyDiffersFromCard) => formatter.write_str(
                "the public key derived from the sidecar differs from the public_key of the card",
            ),
            Self::CardHeld => formatter.write_str("another process holds this card"),
            Self::AlreadyExists => formatter
                .write_str("the card or its sidecar already exists; create never overwrites"),
            Self::Io(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for SidecarError {}

impl From<io::Error> for SidecarError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// Waits between lock attempts so a test can stand in for the clock.
pub trait Pause {
    /// Blocks the caller for the duration.
    fn pause(&self, duration: Duration);
}

/// Pauses by sleeping the current thread.
pub struct ThreadSleep;

impl Pause for ThreadSleep {
    fn pause(&self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

/// A validated sidecar whose exclusive lock lives as long as this value.
pub struct HeldSidecar {
    _locked_file: File,
    secret_key: SecretKey,
}

impl HeldSidecar {
    /// The card's secret key, which is also the node's endpoint key.
    #[must_use]
    pub const fn secret_key(&self) -> &SecretKey {
        &self.secret_key
    }
}

/// Validates the sidecar against the card's public key, then locks it within the retry window.
///
/// # Errors
/// `Invalid` for a missing, loosely permitted, malformed, or mismatched sidecar; `CardHeld` when another holder keeps the lock past the window.
pub fn claim(
    sidecar_path: &Path,
    card_public_key: &[u8; PUBLIC_KEY_BYTES],
    pause: &impl Pause,
) -> Result<HeldSidecar, SidecarError> {
    let mut file = open_sidecar(sidecar_path)?;
    require_owner_read_write_only(&file)?;
    let secret_key = read_secret_key(&mut file)?;
    require_public_key_matches(&secret_key, card_public_key)?;
    acquire_exclusive_lock(&file, pause)?;
    Ok(HeldSidecar {
        _locked_file: file,
        secret_key,
    })
}

/// Reports whether another holder keeps the sidecar lock, by trying it and releasing at once.
///
/// # Errors
/// `Invalid(Missing)` when the sidecar is absent.
pub fn is_held(sidecar_path: &Path) -> Result<bool, SidecarError> {
    match open_sidecar(sidecar_path)?.try_lock() {
        Ok(()) => Ok(false),
        Err(TryLockError::WouldBlock) => Ok(true),
        Err(TryLockError::Error(error)) => Err(SidecarError::Io(error)),
    }
}

/// Derives the key pair from the given seed, writes the 0600 sidecar durably, then writes a card of handle and public key.
///
/// # Errors
/// `AlreadyExists` when either file exists; nothing is written then.
pub fn create_card(
    card_path: &Path,
    sidecar_path: &Path,
    handle: &str,
    seed: [u8; SEED_BYTES],
) -> Result<PublicKey, SidecarError> {
    refuse_existing_card(card_path)?;
    let public_key = SecretKey::from_bytes(&seed).public();
    write_sidecar_durably(sidecar_path, &seed)?;
    write_card(card_path, handle, public_key.as_bytes()).or_else(|error| {
        std::fs::remove_file(sidecar_path)?;
        Err(error)
    })?;
    Ok(public_key)
}

fn refuse_existing_card(card_path: &Path) -> Result<(), SidecarError> {
    if card_path.try_exists()? {
        Err(SidecarError::AlreadyExists)
    } else {
        Ok(())
    }
}

fn write_sidecar_durably(sidecar_path: &Path, seed: &[u8; SEED_BYTES]) -> Result<(), SidecarError> {
    let mut file = create_new_file(sidecar_path, OWNER_READ_WRITE_ONLY)?;
    file.write_all(format!("{}\n", HEXLOWER.encode(seed)).as_bytes())?;
    file.sync_all()?;
    let directory = sidecar_path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "the sidecar path has no directory",
        )
    })?;
    File::open(directory)?.sync_all()?;
    Ok(())
}

fn write_card(
    card_path: &Path,
    handle: &str,
    public_key: &[u8; PUBLIC_KEY_BYTES],
) -> Result<(), SidecarError> {
    let mut file = create_new_file(card_path, CARD_FILE_MODE)?;
    let contents = format!(
        "handle: {handle}\npublic_key: {}\n",
        HEXLOWER.encode(public_key)
    );
    file.write_all(contents.as_bytes())?;
    Ok(())
}

fn create_new_file(path: &Path, mode: u32) -> Result<File, SidecarError> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(path)
        .map_err(|error| match error.kind() {
            io::ErrorKind::AlreadyExists => SidecarError::AlreadyExists,
            _ => SidecarError::Io(error),
        })
}

fn open_sidecar(sidecar_path: &Path) -> Result<File, SidecarError> {
    File::open(sidecar_path).map_err(|error| match error.kind() {
        io::ErrorKind::NotFound => SidecarError::Invalid(InvalidSidecar::Missing),
        _ => SidecarError::Io(error),
    })
}

fn require_owner_read_write_only(file: &File) -> Result<(), SidecarError> {
    let mode = file.metadata()?.permissions().mode() & PERMISSION_BITS_MASK;
    if mode == OWNER_READ_WRITE_ONLY {
        Ok(())
    } else {
        Err(SidecarError::Invalid(
            InvalidSidecar::PermissionsNotOwnerReadWrite { mode },
        ))
    }
}

fn read_secret_key(file: &mut File) -> Result<SecretKey, SidecarError> {
    let mut contents = Vec::new();
    file.read_to_end(&mut contents)?;
    let hex = contents.strip_suffix(b"\n").unwrap_or(&contents);
    let seed: [u8; SEED_BYTES] = HEXLOWER_PERMISSIVE
        .decode(hex)
        .ok()
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or(SidecarError::Invalid(
            InvalidSidecar::SeedNotThirtyTwoHexBytes,
        ))?;
    Ok(SecretKey::from_bytes(&seed))
}

fn require_public_key_matches(
    secret_key: &SecretKey,
    card_public_key: &[u8; PUBLIC_KEY_BYTES],
) -> Result<(), SidecarError> {
    if secret_key.public().as_bytes() == card_public_key {
        Ok(())
    } else {
        Err(SidecarError::Invalid(
            InvalidSidecar::PublicKeyDiffersFromCard,
        ))
    }
}

fn acquire_exclusive_lock(file: &File, pause: &impl Pause) -> Result<(), SidecarError> {
    let mut waited = Duration::ZERO;
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(()),
            Err(TryLockError::WouldBlock) if waited < LOCK_RETRY_WINDOW => {
                pause.pause(LOCK_RETRY_INTERVAL);
                waited = waited.saturating_add(LOCK_RETRY_INTERVAL);
            }
            Err(TryLockError::WouldBlock) => return Err(SidecarError::CardHeld),
            Err(TryLockError::Error(error)) => return Err(SidecarError::Io(error)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct NoPause;

    impl Pause for NoPause {
        fn pause(&self, _duration: Duration) {}
    }

    fn fresh_directory(test_name: &str) -> std::path::PathBuf {
        let directory = std::env::temp_dir().join(format!(
            "estafette-sidecar-{}-{test_name}",
            std::process::id()
        ));
        if directory.exists() {
            std::fs::remove_dir_all(&directory).unwrap();
        }
        std::fs::create_dir_all(&directory).unwrap();
        directory
    }

    fn seed_of(index: u8) -> [u8; 32] {
        let mut seed = [0u8; 32];
        for (position, byte) in seed.iter_mut().enumerate() {
            *byte = index
                .wrapping_mul(31)
                .wrapping_add(u8::try_from(position).unwrap());
        }
        seed
    }

    fn public_key_of(seed: &[u8; 32]) -> [u8; 32] {
        *SecretKey::from_bytes(seed).public().as_bytes()
    }

    fn write_sidecar(path: &Path, contents: &str, mode: u32) {
        std::fs::write(path, contents).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    #[test]
    fn claim_of_a_valid_sidecar_yields_its_secret_key_spec_3_5() {
        let directory = fresh_directory("claim_valid");
        let seed = seed_of(1);
        let sidecar = directory.join("a.key");
        write_sidecar(&sidecar, &format!("{}\n", HEXLOWER.encode(&seed)), 0o600);

        let held = claim(&sidecar, &public_key_of(&seed), &NoPause).unwrap();

        assert_eq!(held.secret_key().to_bytes(), seed);
    }

    struct ReleaseHolderAfter {
        pauses_left: std::cell::Cell<u32>,
        holder: std::cell::RefCell<Option<HeldSidecar>>,
    }

    impl Pause for ReleaseHolderAfter {
        fn pause(&self, _duration: Duration) {
            self.pauses_left
                .set(self.pauses_left.get().saturating_sub(1));
            if self.pauses_left.get() == 0 {
                self.holder.borrow_mut().take();
            }
        }
    }

    #[test]
    fn claim_retries_until_the_holder_lets_go_within_the_window_spec_3_14() {
        let directory = fresh_directory("claim_retries");
        let seed = seed_of(2);
        let public_key = public_key_of(&seed);
        let sidecar = directory.join("b.key");
        write_sidecar(&sidecar, &format!("{}\n", HEXLOWER.encode(&seed)), 0o600);
        let holder = claim(&sidecar, &public_key, &NoPause).unwrap();
        let releasing_pause = ReleaseHolderAfter {
            pauses_left: std::cell::Cell::new(5),
            holder: std::cell::RefCell::new(Some(holder)),
        };

        let second = claim(&sidecar, &public_key, &releasing_pause);

        second.unwrap();
    }

    struct RecordingPause {
        paused: std::cell::Cell<Duration>,
    }

    impl Pause for RecordingPause {
        fn pause(&self, duration: Duration) {
            self.paused.set(self.paused.get().saturating_add(duration));
        }
    }

    #[test]
    fn second_claimer_of_one_card_gets_card_held_after_the_lock_retry_window_test_6() {
        let directory = fresh_directory("two_claimers");
        let seed = seed_of(3);
        let public_key = public_key_of(&seed);
        let sidecar = directory.join("c.key");
        write_sidecar(&sidecar, &format!("{}\n", HEXLOWER.encode(&seed)), 0o600);
        let _first_holder = claim(&sidecar, &public_key, &NoPause).unwrap();
        let recording_pause = RecordingPause {
            paused: std::cell::Cell::new(Duration::ZERO),
        };

        let second = claim(&sidecar, &public_key, &recording_pause);

        assert!(matches!(second, Err(SidecarError::CardHeld)));
        assert_eq!(recording_pause.paused.get(), LOCK_RETRY_WINDOW);
    }

    #[test]
    fn sidecar_with_any_mode_but_0600_is_invalid_test_16() {
        let directory = fresh_directory("loose_mode");
        let seed = seed_of(4);
        for (index, refused_mode) in [0o644, 0o640, 0o660, 0o666, 0o400, 0o700]
            .into_iter()
            .enumerate()
        {
            let sidecar = directory.join(format!("d{index}.key"));
            write_sidecar(
                &sidecar,
                &format!("{}\n", HEXLOWER.encode(&seed)),
                refused_mode,
            );

            let outcome = claim(&sidecar, &public_key_of(&seed), &NoPause);

            assert!(
                matches!(
                    outcome,
                    Err(SidecarError::Invalid(
                        InvalidSidecar::PermissionsNotOwnerReadWrite { mode }
                    )) if mode == refused_mode
                ),
                "mode {refused_mode:o} was not refused"
            );
        }
    }

    #[test]
    fn claim_accepts_uppercase_seed_without_newline_spec_3_1() {
        let directory = fresh_directory("uppercase");
        let seed = seed_of(5);
        let sidecar = directory.join("e.key");
        write_sidecar(&sidecar, &HEXLOWER.encode(&seed).to_uppercase(), 0o600);

        let held = claim(&sidecar, &public_key_of(&seed), &NoPause).unwrap();

        assert_eq!(held.secret_key().to_bytes(), seed);
    }

    #[test]
    fn claim_refuses_a_sidecar_whose_public_key_is_not_the_cards_spec_3_5() {
        let directory = fresh_directory("mismatch");
        let sidecar = directory.join("f.key");
        write_sidecar(
            &sidecar,
            &format!("{}\n", HEXLOWER.encode(&seed_of(6))),
            0o600,
        );

        let outcome = claim(&sidecar, &public_key_of(&seed_of(7)), &NoPause);

        assert!(matches!(
            outcome,
            Err(SidecarError::Invalid(
                InvalidSidecar::PublicKeyDiffersFromCard
            ))
        ));
    }

    #[test]
    fn claim_refuses_a_missing_sidecar_and_creates_none_spec_3_5() {
        let directory = fresh_directory("missing");
        let sidecar = directory.join("g.key");

        let outcome = claim(&sidecar, &public_key_of(&seed_of(8)), &NoPause);

        assert!(matches!(
            outcome,
            Err(SidecarError::Invalid(InvalidSidecar::Missing))
        ));
        assert!(!sidecar.exists());
    }

    #[test]
    fn claim_refuses_a_seed_of_sixty_three_hex_characters_spec_3_5() {
        let directory = fresh_directory("short_seed");
        let sidecar = directory.join("h.key");
        let sixty_three_hex: String = HEXLOWER.encode(&seed_of(9)).chars().skip(1).collect();
        write_sidecar(&sidecar, &sixty_three_hex, 0o600);

        let outcome = claim(&sidecar, &public_key_of(&seed_of(9)), &NoPause);

        assert!(matches!(
            outcome,
            Err(SidecarError::Invalid(
                InvalidSidecar::SeedNotThirtyTwoHexBytes
            ))
        ));
    }

    #[test]
    fn claim_refuses_bytes_after_the_final_newline_spec_3_5() {
        let directory = fresh_directory("trailing_bytes");
        let seed = seed_of(15);
        let sidecar = directory.join("n.key");
        write_sidecar(&sidecar, &format!("{}\nX", HEXLOWER.encode(&seed)), 0o600);

        let outcome = claim(&sidecar, &public_key_of(&seed), &NoPause);

        assert!(matches!(
            outcome,
            Err(SidecarError::Invalid(
                InvalidSidecar::SeedNotThirtyTwoHexBytes
            ))
        ));
    }

    #[test]
    fn probe_sees_a_held_card_and_leaves_a_free_card_free_spec_837() {
        let directory = fresh_directory("probe");
        let seed = seed_of(10);
        let public_key = public_key_of(&seed);
        let sidecar = directory.join("i.key");
        write_sidecar(&sidecar, &format!("{}\n", HEXLOWER.encode(&seed)), 0o600);
        let holder = claim(&sidecar, &public_key, &NoPause).unwrap();

        let held_while_claimed = is_held(&sidecar).unwrap();
        drop(holder);
        let held_after_release = is_held(&sidecar).unwrap();
        let reclaim = claim(&sidecar, &public_key, &NoPause);

        assert!(held_while_claimed);
        assert!(!held_after_release);
        reclaim.unwrap();
    }

    #[test]
    fn probe_of_a_missing_sidecar_is_invalid_spec_837() {
        let directory = fresh_directory("probe_missing");

        let outcome = is_held(&directory.join("j.key"));

        assert!(matches!(
            outcome,
            Err(SidecarError::Invalid(InvalidSidecar::Missing))
        ));
    }

    #[test]
    fn created_card_holds_handle_and_public_key_only_beside_a_claimable_0600_sidecar_spec_3_5() {
        let directory = fresh_directory("create");
        let seed = seed_of(11);
        let card = directory.join("k.yaml");
        let sidecar = directory.join("k.key");

        let returned_key = create_card(&card, &sidecar, "scout", seed).unwrap();

        let public_key = public_key_of(&seed);
        assert_eq!(returned_key.as_bytes(), &public_key);
        assert_eq!(
            std::fs::read_to_string(&card).unwrap(),
            format!(
                "handle: scout\npublic_key: {}\n",
                HEXLOWER.encode(&public_key)
            )
        );
        assert_eq!(
            std::fs::read_to_string(&sidecar).unwrap(),
            format!("{}\n", HEXLOWER.encode(&seed))
        );
        assert_eq!(
            std::fs::metadata(&sidecar).unwrap().permissions().mode() & PERMISSION_BITS_MASK,
            OWNER_READ_WRITE_ONLY
        );
        let held = claim(&sidecar, &public_key, &NoPause).unwrap();
        assert_eq!(held.secret_key().to_bytes(), seed);
    }

    #[test]
    fn create_refuses_an_existing_card_and_writes_no_sidecar_spec_3_5() {
        let directory = fresh_directory("create_card_exists");
        let card = directory.join("l.yaml");
        let sidecar = directory.join("l.key");
        std::fs::write(&card, "handle: old\n").unwrap();

        let outcome = create_card(&card, &sidecar, "scout", seed_of(12));

        assert!(matches!(outcome, Err(SidecarError::AlreadyExists)));
        assert_eq!(std::fs::read_to_string(&card).unwrap(), "handle: old\n");
        assert!(!sidecar.exists());
    }

    #[test]
    fn create_refuses_an_existing_sidecar_and_leaves_it_and_writes_no_card_spec_3_5() {
        let directory = fresh_directory("create_sidecar_exists");
        let card = directory.join("m.yaml");
        let sidecar = directory.join("m.key");
        let old_contents = format!("{}\n", HEXLOWER.encode(&seed_of(13)));
        write_sidecar(&sidecar, &old_contents, 0o600);

        let outcome = create_card(&card, &sidecar, "scout", seed_of(14));

        assert!(matches!(outcome, Err(SidecarError::AlreadyExists)));
        assert_eq!(std::fs::read_to_string(&sidecar).unwrap(), old_contents);
        assert!(!card.exists());
    }

    #[test]
    fn failed_card_write_leaves_no_sidecar_behind_spec_3_5() {
        let directory = fresh_directory("create_card_write_fails");
        let card = directory.join("absent-directory").join("o.yaml");
        let sidecar = directory.join("o.key");

        let outcome = create_card(&card, &sidecar, "scout", seed_of(16));

        assert!(matches!(outcome, Err(SidecarError::Io(_))));
        assert!(!sidecar.exists());
    }
}
