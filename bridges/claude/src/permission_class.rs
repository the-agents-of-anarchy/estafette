use std::fmt;

const BYPASS_PERMISSION_MODE: &str = "bypassPermissions";
const BYPASS_CLASS_NAME: &str = "bypass";
const PROMPTING_CLASS_NAME: &str = "prompting";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PermissionClass {
    Bypass,
    Prompting,
}

impl PermissionClass {
    /// Maps a Claude Code permission mode name to the class the harness compares senders by.
    #[must_use]
    pub fn from_permission_mode(mode: &str) -> Self {
        if mode == BYPASS_PERMISSION_MODE {
            Self::Bypass
        } else {
            Self::Prompting
        }
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

    #[test]
    fn spec_3_12_harness_2_1_319_bypass_permissions_mode_is_the_bypass_class() {
        assert_eq!(
            PermissionClass::from_permission_mode("bypassPermissions"),
            PermissionClass::Bypass
        );
    }

    #[test]
    fn spec_3_12_harness_2_1_319_every_other_mode_is_the_prompting_class() {
        for mode in [
            "default",
            "plan",
            "acceptEdits",
            "auto",
            "dontAsk",
            "BYPASSPERMISSIONS",
            "",
        ] {
            assert_eq!(
                PermissionClass::from_permission_mode(mode),
                PermissionClass::Prompting,
                "{mode}"
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
