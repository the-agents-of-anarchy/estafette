const FIELD_SEPARATOR: char = '\t';
const LINE_END: &str = "\n";

/// Joins the fields into one tab-separated line.
pub fn tab_separated_line(fields: &[&str]) -> String {
    let escaped: Vec<String> = fields.iter().map(|field| escape_controls(field)).collect();
    escaped.join(&FIELD_SEPARATOR.to_string()) + LINE_END
}

/// The field with every control character, tab and newline included, spelled out so it cannot end a column or a line.
fn escape_controls(field: &str) -> String {
    field
        .chars()
        .flat_map(|character| {
            let spelled: Vec<char> = if character.is_control() {
                character.escape_default().collect()
            } else {
                vec![character]
            };
            spelled
        })
        .collect()
}
