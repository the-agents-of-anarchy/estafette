use std::fmt;

use serde::Deserialize;

const BYPASS_HOOK_PERMISSION_MODE: &str = "bypassPermissions";
const BYPASS_CLASS_NAME: &str = "bypass";
const PROMPTING_CLASS_NAME: &str = "prompting";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PermissionClass {
    Bypass,
    Prompting,
}

#[derive(Deserialize)]
struct HookInput {
    permission_mode: Option<String>,
}

impl PermissionClass {
    /// Reads the served session's class from the `permission_mode` of the hook's JSON, taking prompting when it is missing or unreadable.
    #[must_use]
    pub fn from_hook_input(hook_input: &str) -> Self {
        serde_json::from_str::<HookInput>(hook_input)
            .ok()
            .and_then(|input| input.permission_mode)
            .map_or(Self::Prompting, |mode| {
                if mode == BYPASS_HOOK_PERMISSION_MODE {
                    Self::Bypass
                } else {
                    Self::Prompting
                }
            })
    }

    /// Reads a class back from the text `Display` wrote.
    #[must_use]
    pub fn from_class_name(text: &str) -> Option<Self> {
        match text.trim() {
            BYPASS_CLASS_NAME => Some(Self::Bypass),
            PROMPTING_CLASS_NAME => Some(Self::Prompting),
            _ => None,
        }
    }
}

impl fmt::Display for PermissionClass {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Bypass => BYPASS_CLASS_NAME,
            Self::Prompting => PROMPTING_CLASS_NAME,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn class_of_mode(mode: &str) -> PermissionClass {
        PermissionClass::from_hook_input(&format!(
            "{{\"session_id\":\"s\",\"permission_mode\":\"{mode}\",\"hook_event_name\":\"SessionStart\"}}"
        ))
    }

    #[test]
    fn hook_docs_bypass_permissions_mode_is_the_bypass_class() {
        assert_eq!(class_of_mode("bypassPermissions"), PermissionClass::Bypass);
    }

    #[test]
    fn hook_docs_every_other_documented_mode_is_the_prompting_class() {
        for mode in ["default", "plan", "acceptEdits", "auto", "dontAsk"] {
            assert_eq!(class_of_mode(mode), PermissionClass::Prompting, "{mode}");
        }
    }

    #[test]
    fn hook_docs_a_missing_unknown_or_unreadable_mode_is_the_prompting_class() {
        for input in [
            "{}",
            "{\"permission_mode\":null}",
            "{\"permission_mode\":7}",
            "{\"permission_mode\":\"BYPASSPERMISSIONS\"}",
            "not json",
            "",
        ] {
            assert_eq!(
                PermissionClass::from_hook_input(input),
                PermissionClass::Prompting,
                "{input}"
            );
        }
    }

    #[test]
    fn harness_names_the_classes_bypass_and_prompting_and_reads_them_back() {
        for (class, name) in [
            (PermissionClass::Bypass, "bypass"),
            (PermissionClass::Prompting, "prompting"),
        ] {
            assert_eq!(class.to_string(), name);
            assert_eq!(
                PermissionClass::from_class_name(&format!("{name}\n")),
                Some(class)
            );
        }
        assert_eq!(PermissionClass::from_class_name("default"), None);
    }
}
