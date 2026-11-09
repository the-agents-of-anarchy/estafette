use serde::Deserialize;

const PERMISSION_MODE_ENTRY_TYPE: &str = "permission-mode";
const PERMISSION_MODE_ENTRY_MARKER: &str = "\"type\":\"permission-mode\"";
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
                && id
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric() || character == '-')
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
        .filter(|line| line.contains(PERMISSION_MODE_ENTRY_MARKER))
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
}
