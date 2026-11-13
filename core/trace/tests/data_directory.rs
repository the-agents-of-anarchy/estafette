use std::ffi::OsString;

use estafette_trace::TraceDirectory;

const SPEC_DATA_REFUSAL: &str = "XDG_DATA_HOME and HOME are both unset or empty: set one of them";
const EVERY_VARIABLE_UNSET: fn(&str) -> Option<OsString> = |_name| None;
const EVERY_VARIABLE_EMPTY: fn(&str) -> Option<OsString> = |_name| Some(OsString::new());

#[test]
fn spec_3_3_a_trace_directory_is_refused_naming_the_data_variable_when_neither_variable_names_a_directory()
 {
    for (case, environment) in [
        ("every variable unset", EVERY_VARIABLE_UNSET),
        ("every variable empty", EVERY_VARIABLE_EMPTY),
    ] {
        let refusal = TraceDirectory::resolve(&environment).unwrap_err();

        assert_eq!(refusal.to_string(), SPEC_DATA_REFUSAL, "{case}");
    }
}
