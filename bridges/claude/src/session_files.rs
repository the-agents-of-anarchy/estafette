use serde::Deserialize;

const PERMISSION_MODE_ENTRY_TYPE: &str = "permission-mode";
const SESSION_ID_SEPARATOR: char = '-';
const TRANSCRIPT_EXTENSION: &str = "jsonl";

#[derive(Deserialize)]
struct RegistryEntry {
    #[serde(rename = "sessionId")]
    session_id: String,
}

#[derive(Deserialize)]
struct TranscriptEntry {
    #[serde(rename = "type")]
    entry_type: Option<String>,
    #[serde(rename = "permissionMode")]
    permission_mode: Option<String>,
}

/// Reads the session id out of the harness's session registry entry, refusing one that could name another path.
#[must_use]
pub fn session_id_of_registry_entry(registry_text: &str) -> Option<String> {
    serde_json::from_str::<RegistryEntry>(registry_text)
        .ok()
        .map(|entry| entry.session_id)
        .filter(|id| {
            !id.is_empty()
                && id.chars().all(|character| {
                    character.is_ascii_alphanumeric() || character == SESSION_ID_SEPARATOR
                })
        })
}

/// Names the transcript file the harness keeps for a session id.
#[must_use]
pub fn transcript_file_name(session_id: &str) -> String {
    format!("{session_id}.{TRANSCRIPT_EXTENSION}")
}

/// Finds the permission mode the session's latest `permission-mode` transcript entry records.
#[must_use]
pub fn latest_permission_mode(transcript_text: &str) -> Option<String> {
    transcript_text
        .lines()
        .rev()
        .filter(|line| line.contains(PERMISSION_MODE_ENTRY_TYPE))
        .filter_map(|line| serde_json::from_str::<TranscriptEntry>(line).ok())
        .find(|entry| entry.entry_type.as_deref() == Some(PERMISSION_MODE_ENTRY_TYPE))
        .and_then(|entry| entry.permission_mode)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TRANSCRIPT_WITH_A_SWITCH: &str = concat!(
        "{\"type\":\"permission-mode\",\"permissionMode\":\"bypassPermissions\",\"sessionId\":\"s\"}\n",
        "{\"type\":\"user\",\"permissionMode\":\"bypassPermissions\"}\n",
        "{\"type\":\"permission-mode\",\"permissionMode\":\"auto\",\"sessionId\":\"s\"}\n",
        "{\"type\":\"user\",\"permissionMode\":\"default\",\"message\":\"{\\\"type\\\":\\\"permission-mode\\\"}\"}\n",
    );

    #[test]
    fn harness_2_1_319_the_latest_permission_mode_entry_names_the_live_mode_after_a_switch() {
        assert_eq!(
            latest_permission_mode(TRANSCRIPT_WITH_A_SWITCH).as_deref(),
            Some("auto")
        );
    }

    #[test]
    fn harness_2_1_319_a_transcript_without_a_permission_mode_entry_names_no_mode() {
        for transcript in [
            "",
            "not json\n",
            "{\"type\":\"user\",\"permissionMode\":\"auto\"}\n",
            "{\"type\":\"permission-mode\"}\n",
        ] {
            assert_eq!(latest_permission_mode(transcript), None, "{transcript}");
        }
    }

    #[test]
    fn harness_2_1_319_the_registry_entry_gives_the_session_id_of_the_transcript() {
        let entry =
            "{\"pid\":1,\"sessionId\":\"33a25ba8-1545-4166-9762-1dc6bbf59e05\",\"name\":\"x\"}";
        assert_eq!(
            session_id_of_registry_entry(entry).as_deref(),
            Some("33a25ba8-1545-4166-9762-1dc6bbf59e05")
        );
        assert_eq!(
            transcript_file_name("33a25ba8-1545-4166-9762-1dc6bbf59e05"),
            "33a25ba8-1545-4166-9762-1dc6bbf59e05.jsonl"
        );
    }

    #[test]
    fn harness_2_1_319_a_registry_entry_naming_another_path_or_no_session_gives_no_id() {
        for entry in [
            "{\"sessionId\":\"../x\"}",
            "{\"sessionId\":\"a/b\"}",
            "{\"sessionId\":\"\"}",
            "{\"pid\":1}",
            "nope",
        ] {
            assert_eq!(session_id_of_registry_entry(entry), None, "{entry}");
        }
    }

    const GENERATED_CASES: u64 = 300;
    const MODES: [&str; 4] = ["bypassPermissions", "auto", "default", "plan"];

    fn transcript_line_of_kind(kind: usize, mode: &str) -> String {
        match kind {
            0 => format!("{{\"type\":\"permission-mode\",\"permissionMode\":\"{mode}\"}}"),
            1 => format!("{{\"type\":\"user\",\"permissionMode\":\"{mode}\"}}"),
            2 => "not json {".to_owned(),
            3 => "{\"type\":\"user\",\"message\":\"{\\\"type\\\":\\\"permission-mode\\\",\\\"permissionMode\\\":\\\"x\\\"}\"}".to_owned(),
            _ => String::new(),
        }
    }

    fn next_below(state: &mut u64, bound: usize) -> usize {
        *state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        usize::try_from(*state >> 33)
            .unwrap_or(0)
            .checked_rem(bound)
            .unwrap_or(0)
    }

    #[test]
    fn harness_2_1_319_the_latest_permission_mode_entry_wins_whatever_surrounds_it() {
        let mut seeds_with_an_entry = 0;
        for seed in 0..GENERATED_CASES {
            let mut state = seed;
            let mut transcript = String::new();
            let mut expected = None;
            for _ in 0..next_below(&mut state, 12) {
                let kind = next_below(&mut state, 5);
                let mode = MODES
                    .get(next_below(&mut state, MODES.len()))
                    .copied()
                    .unwrap_or("");
                transcript.push_str(&transcript_line_of_kind(kind, mode));
                transcript.push('\n');
                if kind == 0 {
                    expected = Some(mode.to_owned());
                }
            }
            seeds_with_an_entry += usize::from(expected.is_some());
            assert_eq!(latest_permission_mode(&transcript), expected, "seed {seed}");
        }
        assert!(seeds_with_an_entry > 100, "{seeds_with_an_entry}");
    }

    #[test]
    fn harness_2_1_319_a_session_id_with_a_path_character_is_never_returned() {
        const ID_ALPHABET: [char; 8] = ['a', 'b', '-', '1', '/', '.', '\\', ' '];
        for seed in 0..GENERATED_CASES {
            let mut state = seed;
            let id: String = (0..next_below(&mut state, 7))
                .map(|_| {
                    ID_ALPHABET
                        .get(next_below(&mut state, ID_ALPHABET.len()))
                        .copied()
                        .unwrap_or('a')
                })
                .collect();
            let entry = serde_json::json!({ "sessionId": id }).to_string();
            if let Some(found) = session_id_of_registry_entry(&entry) {
                assert!(
                    !found.is_empty()
                        && found.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'),
                    "{id:?}"
                );
                assert_eq!(found, id);
            } else {
                assert!(
                    id.is_empty() || id.chars().any(|c| !(c.is_ascii_alphanumeric() || c == '-')),
                    "{id:?}"
                );
            }
        }
    }
}
