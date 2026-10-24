use std::collections::HashMap;
use std::fmt;
use std::str::FromStr;

use serde_json::{Map, Number, Value};
use yaml_rust2::Yaml;
use yaml_rust2::parser::{Event, MarkedEventReceiver, Parser, Tag};
use yaml_rust2::scanner::{Marker, TScalarStyle};

const PUBLIC_MAX_BYTES: usize = 3072;
const HANDLE_FIELD: &str = "handle";
const PUBLIC_KEY_FIELD: &str = "public_key";
const NETWORK_FIELD: &str = "network";
const PUBLIC_FIELD: &str = "public";
const DESCRIPTION_FIELD: &str = "description";
const YAML_CORE_TAG_HANDLE: &str = "tag:yaml.org,2002:";
const YAML_BINARY_TAG_SUFFIX: &str = "binary";
const YAML_MERGE_KEY: &str = "<<";
const YEAR_DIGITS: usize = 4;
const DATE_PART_MAX_DIGITS: usize = 2;
const CLOCK_PART_DIGITS: usize = 2;
const BLANKS: [char; 2] = [' ', '\t'];
const NO_ANCHOR: usize = 0;
const NUMBER_SIGNS: [char; 2] = ['+', '-'];
const DATE_SEPARATOR: char = '-';
const CLOCK_SEPARATOR: char = ':';
const FRACTION_POINT: char = '.';
const DATE_TIME_SEPARATORS: [char; 2] = ['T', 't'];
const UTC_DESIGNATOR: &str = "Z";
const ZONE_SIGNS: [char; 2] = ['+', '-'];

/// The refusal of a card that cannot be claimed, mapped to `card_invalid` by the caller.
#[derive(Debug, PartialEq, Eq)]
pub enum CardInvalid {
    Unparsable(String),
    NotOneMappingDocument,
    MissingKey(&'static str),
    DuplicateCardKey(&'static str),
    KeyNotScalar(&'static str),
    PublicNotMapping,
    NumberNotFinite,
    Tag,
    Binary,
    Date,
    NonStringKey,
    MergeKey,
    DuplicatePublicKey(String),
    PublicOverCap { bytes: usize },
    AliasExpansionTooLarge,
    IntegerTooWide,
}

impl fmt::Display for CardInvalid {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unparsable(reason) => write!(formatter, "card is not valid YAML: {reason}"),
            Self::NotOneMappingDocument => {
                write!(
                    formatter,
                    "card must be exactly one YAML document holding a mapping"
                )
            }
            Self::MissingKey(key) => write!(formatter, "card has no `{key}`"),
            Self::DuplicateCardKey(key) => write!(formatter, "card names `{key}` more than once"),
            Self::KeyNotScalar(key) => write!(formatter, "card `{key}` must be a single value"),
            Self::PublicNotMapping => write!(formatter, "card `public` must be a mapping"),
            Self::NumberNotFinite => {
                write!(
                    formatter,
                    "card `public` numbers must be finite, not .inf or .nan"
                )
            }
            Self::Tag => write!(formatter, "card `public` must not carry a YAML tag"),
            Self::Binary => write!(formatter, "card `public` must not hold binary data"),
            Self::Date => write!(
                formatter,
                "card `public` must not hold a bare date or timestamp; quote it to keep it as text"
            ),
            Self::NonStringKey => write!(
                formatter,
                "card `public` mapping keys must be strings; quote a key such as 1, true or null"
            ),
            Self::MergeKey => write!(
                formatter,
                "card `public` must not use the merge key `<<`; quote it to keep it as a text key"
            ),
            Self::DuplicatePublicKey(key) => {
                write!(
                    formatter,
                    "card `public` names the key `{key}` more than once"
                )
            }
            Self::PublicOverCap { bytes } => write!(
                formatter,
                "card `public` serializes to {bytes} bytes; the cap is {PUBLIC_MAX_BYTES}"
            ),
            Self::AliasExpansionTooLarge => write!(
                formatter,
                "card aliases expand to more than {PUBLIC_MAX_BYTES} nodes"
            ),
            Self::IntegerTooWide => write!(
                formatter,
                "card `public` integers must fit 64 bits, from -9223372036854775808 to 18446744073709551615"
            ),
        }
    }
}

impl std::error::Error for CardInvalid {}

/// A parsed card: the four keys the binary reads, with `public` held as a JSON mapping under the cap.
#[derive(Debug, PartialEq, Eq)]
pub struct Card {
    pub handle: String,
    pub public_key: String,
    pub network: Option<String>,
    public: Map<String, Value>,
}

impl FromStr for Card {
    type Err = CardInvalid;

    /// Parses card YAML into a card, refusing every `public` content spec 3.5 rejects.
    fn from_str(text: &str) -> Result<Self, CardInvalid> {
        let root = read_single_document(text)?;
        let entries = mapping_entries(&root)?;
        Ok(Self {
            handle: required_scalar_text(entries, HANDLE_FIELD)?,
            public_key: required_scalar_text(entries, PUBLIC_KEY_FIELD)?,
            network: optional_scalar_text(entries, NETWORK_FIELD)?,
            public: public_mapping(entries)?,
        })
    }
}

impl Card {
    /// The `public` mapping as compact JSON bytes, exactly what travels in presence.
    #[must_use]
    pub fn public_json(&self) -> Vec<u8> {
        compact_json(&self.public)
    }

    /// Puts the description under `public.description`.
    ///
    /// # Errors
    ///
    /// Returns the byte size `public` would reach when it passes the cap.
    pub fn with_description(mut self, description: &str) -> Result<Self, DescriptionOverCap> {
        self.public.insert(
            DESCRIPTION_FIELD.to_owned(),
            Value::String(description.to_owned()),
        );
        self.public = refuse_over_cap(self.public).map_err(|bytes| DescriptionOverCap { bytes })?;
        Ok(self)
    }
}

/// The refusal of a description that pushes `public` past the cap, mapped to `bad_request` by the caller.
#[derive(Debug, PartialEq, Eq)]
pub struct DescriptionOverCap {
    pub bytes: usize,
}

impl fmt::Display for DescriptionOverCap {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "description makes `public` {} bytes; the cap is {PUBLIC_MAX_BYTES}",
            self.bytes
        )
    }
}

impl std::error::Error for DescriptionOverCap {}

/// One YAML node of a card, with its tag and with every alias already replaced by its anchored value.
#[derive(Clone)]
enum Node {
    Scalar {
        text: String,
        style: TScalarStyle,
        tag: Option<Tag>,
    },
    Sequence {
        tag: Option<Tag>,
        items: Vec<Self>,
    },
    Mapping {
        tag: Option<Tag>,
        entries: Vec<(Self, Self)>,
    },
}

/// A sequence or mapping whose closing event has not arrived yet, with its anchor id.
enum OpenNode {
    Sequence {
        anchor: usize,
        tag: Option<Tag>,
        items: Vec<Node>,
    },
    Mapping {
        anchor: usize,
        tag: Option<Tag>,
        entries: Vec<(Node, Node)>,
        pending_key: Option<Node>,
    },
}

impl OpenNode {
    /// The finished node and the anchor id it was opened with.
    fn close(self) -> (usize, Node) {
        match self {
            Self::Sequence { anchor, tag, items } => (anchor, Node::Sequence { tag, items }),
            Self::Mapping {
                anchor,
                tag,
                entries,
                pending_key: _,
            } => (anchor, Node::Mapping { tag, entries }),
        }
    }
}

/// Builds the document nodes from parser events, resolving aliases within the expansion bound.
#[derive(Default)]
struct DocumentCollector {
    open: Vec<OpenNode>,
    documents: Vec<Node>,
    anchored: HashMap<usize, AnchoredNode>,
    expanded_nodes: usize,
    refusal: Option<CardInvalid>,
}

/// A finished anchored node and the number of nodes it holds.
struct AnchoredNode {
    node: Node,
    node_count: usize,
}

impl DocumentCollector {
    /// Puts a finished node where the parser is: a sequence item, a mapping key or value, or a document.
    fn place(&mut self, node: Node) {
        match self.open.last_mut() {
            None => self.documents.push(node),
            Some(OpenNode::Sequence { items, .. }) => items.push(node),
            Some(OpenNode::Mapping {
                entries,
                pending_key,
                ..
            }) => match pending_key.take() {
                None => *pending_key = Some(node),
                Some(key) => entries.push((key, node)),
            },
        }
    }

    /// Places a finished node and remembers it when it carries an anchor.
    fn place_remembering_anchor(&mut self, anchor: usize, node: Node) {
        if anchor != NO_ANCHOR {
            let node_count = count_nodes(&node);
            self.anchored.insert(
                anchor,
                AnchoredNode {
                    node: node.clone(),
                    node_count,
                },
            );
        }
        self.place(node);
    }

    /// Places a copy of the anchored node, or records the refusal when it cannot be copied.
    fn place_alias_target(&mut self, anchor: usize) {
        let Some(target) = self.anchored.get(&anchor) else {
            self.refuse(CardInvalid::Unparsable(
                "an alias points at the node that contains it".to_owned(),
            ));
            return;
        };
        let expanded_nodes = self.expanded_nodes.saturating_add(target.node_count);
        if expanded_nodes > PUBLIC_MAX_BYTES {
            self.refuse(CardInvalid::AliasExpansionTooLarge);
            return;
        }
        let node = target.node.clone();
        self.expanded_nodes = expanded_nodes;
        self.place(node);
    }

    /// Keeps the first refusal and drops any later one.
    fn refuse(&mut self, refusal: CardInvalid) {
        self.refusal.get_or_insert(refusal);
    }
}

/// The number of nodes in the tree, the node itself included.
fn count_nodes(node: &Node) -> usize {
    match node {
        Node::Scalar { .. } => 1,
        Node::Sequence { items, .. } => {
            items.iter().map(count_nodes).fold(1, usize::saturating_add)
        }
        Node::Mapping { entries, .. } => entries
            .iter()
            .flat_map(|(key, value)| [key, value])
            .map(count_nodes)
            .fold(1, usize::saturating_add),
    }
}

impl MarkedEventReceiver for DocumentCollector {
    fn on_event(&mut self, event: Event, _mark: Marker) {
        match event {
            Event::Scalar(text, style, anchor, tag) => {
                self.place_remembering_anchor(anchor, Node::Scalar { text, style, tag });
            }
            Event::Alias(anchor) => self.place_alias_target(anchor),
            Event::SequenceStart(anchor, tag) => {
                self.open.push(OpenNode::Sequence {
                    anchor,
                    tag,
                    items: Vec::new(),
                });
            }
            Event::MappingStart(anchor, tag) => {
                self.open.push(OpenNode::Mapping {
                    anchor,
                    tag,
                    entries: Vec::new(),
                    pending_key: None,
                });
            }
            Event::SequenceEnd | Event::MappingEnd => {
                if let Some(open) = self.open.pop() {
                    let (anchor, node) = open.close();
                    self.place_remembering_anchor(anchor, node);
                }
            }
            Event::Nothing
            | Event::StreamStart
            | Event::StreamEnd
            | Event::DocumentStart
            | Event::DocumentEnd => {}
        }
    }
}

/// The single document of the text, or the refusal for unparsable text, an alias refusal, or a wrong document count.
fn read_single_document(text: &str) -> Result<Node, CardInvalid> {
    let mut collector = DocumentCollector::default();
    Parser::new_from_str(text)
        .load(&mut collector, true)
        .map_err(|error| CardInvalid::Unparsable(error.to_string()))?;
    if let Some(refusal) = collector.refusal {
        return Err(refusal);
    }
    let mut documents = collector.documents.into_iter();
    match (documents.next(), documents.next()) {
        (Some(document), None) => Ok(document),
        (None, _) | (Some(_), Some(_)) => Err(CardInvalid::NotOneMappingDocument),
    }
}

/// The key and value pairs of a mapping node, refusing any other node.
fn mapping_entries(node: &Node) -> Result<&[(Node, Node)], CardInvalid> {
    match node {
        Node::Mapping { entries, .. } => Ok(entries),
        Node::Scalar { .. } | Node::Sequence { .. } => Err(CardInvalid::NotOneMappingDocument),
    }
}

/// The text of a scalar node, or None for a sequence or mapping.
fn scalar_text_of(node: &Node) -> Option<&str> {
    match node {
        Node::Scalar { text, .. } => Some(text),
        Node::Sequence { .. } | Node::Mapping { .. } => None,
    }
}

/// The value under the one key of that name, None when absent, refusing a key named twice.
fn value_of_unique_key<'card>(
    entries: &'card [(Node, Node)],
    name: &'static str,
) -> Result<Option<&'card Node>, CardInvalid> {
    let mut matching = entries
        .iter()
        .filter(|(key, _)| scalar_text_of(key) == Some(name))
        .map(|(_, value)| value);
    let first = matching.next();
    match matching.next() {
        None => Ok(first),
        Some(_) => Err(CardInvalid::DuplicateCardKey(name)),
    }
}

/// The text of the scalar under the key, None when the key is absent, refusing a non-scalar value.
fn optional_scalar_text(
    entries: &[(Node, Node)],
    name: &'static str,
) -> Result<Option<String>, CardInvalid> {
    value_of_unique_key(entries, name)?
        .map(|value| {
            scalar_text_of(value)
                .map(str::to_owned)
                .ok_or(CardInvalid::KeyNotScalar(name))
        })
        .transpose()
}

/// The text of the scalar under the key, refusing an absent key or a non-scalar value.
fn required_scalar_text(
    entries: &[(Node, Node)],
    name: &'static str,
) -> Result<String, CardInvalid> {
    optional_scalar_text(entries, name)?.ok_or(CardInvalid::MissingKey(name))
}

/// The `public` mapping as JSON, empty when absent, refusing a non-mapping or one over the cap.
fn public_mapping(entries: &[(Node, Node)]) -> Result<Map<String, Value>, CardInvalid> {
    let mapping = match value_of_unique_key(entries, PUBLIC_FIELD)? {
        None => Map::new(),
        Some(node) => match convert_value(node)? {
            Value::Object(mapping) => mapping,
            Value::Null
            | Value::Bool(_)
            | Value::Number(_)
            | Value::String(_)
            | Value::Array(_) => {
                return Err(CardInvalid::PublicNotMapping);
            }
        },
    };
    refuse_over_cap(mapping).map_err(|bytes| CardInvalid::PublicOverCap { bytes })
}

/// The compact JSON bytes of the mapping, without whitespace.
fn compact_json(mapping: &Map<String, Value>) -> Vec<u8> {
    Value::Object(mapping.clone()).to_string().into_bytes()
}

/// The mapping itself, or the byte size of its compact JSON when that passes the cap.
fn refuse_over_cap(mapping: Map<String, Value>) -> Result<Map<String, Value>, usize> {
    let bytes = compact_json(&mapping).len();
    if bytes > PUBLIC_MAX_BYTES {
        return Err(bytes);
    }
    Ok(mapping)
}

/// The tag of a node, None when untagged.
const fn tag_of(node: &Node) -> Option<&Tag> {
    match node {
        Node::Scalar { tag, .. } | Node::Sequence { tag, .. } | Node::Mapping { tag, .. } => {
            tag.as_ref()
        }
    }
}

/// Refuses a tagged node, naming binary data apart from every other tag.
fn refuse_tagged(node: &Node) -> Result<(), CardInvalid> {
    match tag_of(node) {
        None => Ok(()),
        Some(tag) if is_binary_tag(tag) => Err(CardInvalid::Binary),
        Some(_) => Err(CardInvalid::Tag),
    }
}

/// Whether the tag is the YAML core binary tag.
fn is_binary_tag(tag: &Tag) -> bool {
    tag.handle == YAML_CORE_TAG_HANDLE && tag.suffix == YAML_BINARY_TAG_SUFFIX
}

/// The JSON value of a node, refusing every content spec 3.5 does not allow in `public`.
fn convert_value(node: &Node) -> Result<Value, CardInvalid> {
    refuse_tagged(node)?;
    match node {
        Node::Scalar { text, style, .. } => convert_scalar(text, *style),
        Node::Sequence { items, .. } => items
            .iter()
            .map(convert_value)
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array),
        Node::Mapping { entries, .. } => convert_mapping(entries),
    }
}

/// Whether a plain scalar is a YAML 1.1 timestamp, which spec 3.5 refuses as a date.
fn is_yaml_timestamp(text: &str) -> bool {
    skip_timestamp(text).is_some()
}

/// Some when the whole text is a YAML 1.1 timestamp: a date, optionally with a time and a zone.
fn skip_timestamp(text: &str) -> Option<()> {
    let after_date = skip_date(text)?;
    if after_date.is_empty() {
        return Some(());
    }
    let time = after_date
        .strip_prefix(DATE_TIME_SEPARATORS)
        .or_else(|| strip_leading_blanks(after_date))?;
    let after_clock = skip_clock(time)?;
    let after_fraction = after_clock
        .strip_prefix(FRACTION_POINT)
        .map_or(after_clock, |fraction| {
            fraction.trim_start_matches(|digit: char| digit.is_ascii_digit())
        });
    let zone = after_fraction.trim_start_matches(BLANKS);
    (zone.is_empty() || zone == UTC_DESIGNATOR || skip_zone_offset(zone)?.is_empty()).then_some(())
}

/// The text after a year-month-day date, or None when the text does not open with one.
fn skip_date(text: &str) -> Option<&str> {
    let after_year = skip_digits(text, YEAR_DIGITS, YEAR_DIGITS)?;
    let after_month = skip_digits(
        after_year.strip_prefix(DATE_SEPARATOR)?,
        1,
        DATE_PART_MAX_DIGITS,
    )?;
    skip_digits(
        after_month.strip_prefix(DATE_SEPARATOR)?,
        1,
        DATE_PART_MAX_DIGITS,
    )
}

/// The text after an hour-minute-second clock, or None when the text does not open with one.
fn skip_clock(text: &str) -> Option<&str> {
    let after_hour = skip_digits(text, 1, DATE_PART_MAX_DIGITS)?;
    let after_minute = skip_digits(
        after_hour.strip_prefix(CLOCK_SEPARATOR)?,
        CLOCK_PART_DIGITS,
        CLOCK_PART_DIGITS,
    )?;
    skip_digits(
        after_minute.strip_prefix(CLOCK_SEPARATOR)?,
        CLOCK_PART_DIGITS,
        CLOCK_PART_DIGITS,
    )
}

/// The text after a zone offset of sign, hours and optional minutes, or None when the text does not open with one.
fn skip_zone_offset(text: &str) -> Option<&str> {
    let after_hours = skip_digits(text.strip_prefix(ZONE_SIGNS)?, 1, DATE_PART_MAX_DIGITS)?;
    after_hours
        .strip_prefix(CLOCK_SEPARATOR)
        .map_or(Some(after_hours), |minutes| {
            skip_digits(minutes, CLOCK_PART_DIGITS, CLOCK_PART_DIGITS)
        })
}

/// The text after its leading spaces and tabs, or None when it has none.
fn strip_leading_blanks(text: &str) -> Option<&str> {
    let stripped = text.trim_start_matches(BLANKS);
    (stripped.len() < text.len()).then_some(stripped)
}

/// The text after a run of digits, or None when the run is shorter than the least or longer than the most.
fn skip_digits(text: &str, least: usize, most: usize) -> Option<&str> {
    let count = text.bytes().take_while(u8::is_ascii_digit).count();
    if count < least || count > most {
        return None;
    }
    text.split_at_checked(count).map(|(_, rest)| rest)
}

/// The JSON value of a scalar: quoted text stays a string, plain text resolves as YAML resolves it.
fn convert_scalar(text: &str, style: TScalarStyle) -> Result<Value, CardInvalid> {
    if style != TScalarStyle::Plain {
        return Ok(Value::String(text.to_owned()));
    }
    if is_yaml_timestamp(text) {
        return Err(CardInvalid::Date);
    }
    match Yaml::from_str(text) {
        Yaml::Null => Ok(Value::Null),
        Yaml::Boolean(flag) => Ok(Value::Bool(flag)),
        Yaml::Integer(number) => Ok(Value::Number(Number::from(number))),
        Yaml::Real(number) if is_integer_text(&number) => number
            .parse::<u64>()
            .ok()
            .map(|wide| Value::Number(Number::from(wide)))
            .ok_or(CardInvalid::IntegerTooWide),
        Yaml::Real(number) => number
            .parse::<f64>()
            .ok()
            .and_then(Number::from_f64)
            .map(Value::Number)
            .ok_or(CardInvalid::NumberNotFinite),
        Yaml::String(string) => Ok(Value::String(string)),
        Yaml::Array(_) | Yaml::Hash(_) | Yaml::Alias(_) | Yaml::BadValue => {
            Ok(Value::String(text.to_owned()))
        }
    }
}

/// Whether the node is an untagged plain `<<` scalar, the YAML merge key.
fn is_merge_key(key: &Node) -> bool {
    match key {
        Node::Scalar {
            text,
            style,
            tag: None,
        } => *style == TScalarStyle::Plain && text == YAML_MERGE_KEY,
        Node::Scalar { tag: Some(_), .. } | Node::Sequence { .. } | Node::Mapping { .. } => false,
    }
}

/// Whether the text is an optional sign followed by digits alone.
fn is_integer_text(text: &str) -> bool {
    let digits = text.strip_prefix(NUMBER_SIGNS).unwrap_or(text);
    !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())
}

/// The JSON object of a mapping, refusing a merge key, a non-string key and a duplicate key.
fn convert_mapping(entries: &[(Node, Node)]) -> Result<Value, CardInvalid> {
    let mut mapping = Map::new();
    for (key, value) in entries {
        if is_merge_key(key) {
            return Err(CardInvalid::MergeKey);
        }
        let key_text = match convert_value(key)? {
            Value::String(text) => text,
            Value::Null
            | Value::Bool(_)
            | Value::Number(_)
            | Value::Array(_)
            | Value::Object(_) => {
                return Err(CardInvalid::NonStringKey);
            }
        };
        if mapping.contains_key(&key_text) {
            return Err(CardInvalid::DuplicatePublicKey(key_text));
        }
        mapping.insert(key_text, convert_value(value)?);
    }
    Ok(Value::Object(mapping))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_card(text: &str) -> Result<Card, CardInvalid> {
        text.parse()
    }

    #[test]
    fn parse_reads_the_four_keys_and_renders_public_as_compact_json_spec_3_5() {
        let text = "handle: ada\npublic_key: abc\nnetwork: lab\nignored: [1, 2]\npublic:\n  role: builder\n  tags: [a, 2]\n";
        let card = parse_card(text).unwrap();
        assert_eq!(card.handle, "ada");
        assert_eq!(card.public_key, "abc");
        assert_eq!(card.network.as_deref(), Some("lab"));
        assert_eq!(card.public_json(), br#"{"role":"builder","tags":["a",2]}"#);
    }

    fn card_with_public(public: &str) -> Result<Card, CardInvalid> {
        parse_card(&format!("handle: ada\npublic_key: abc\npublic:\n{public}"))
    }

    #[test]
    fn parse_refuses_a_date_in_public_but_keeps_a_quoted_date_text_spec_3_5() {
        for date in [
            "2026-10-24",
            "2001-12-14t21:59:43.10-05:00",
            "2001-12-14T21:59:43.10Z",
            "2001-12-14 21:59:43.10 -5",
            "2001-1-1",
        ] {
            assert_eq!(
                card_with_public(&format!("  a: {date}\n")),
                Err(CardInvalid::Date),
                "{date}"
            );
            assert_eq!(
                card_with_public(&format!("  {date}: a\n")),
                Err(CardInvalid::Date),
                "{date}"
            );
        }
        let card =
            card_with_public("  a: \"2026-10-24\"\n  b: 2026-10-24x\n  c: 12-10-24\n").unwrap();
        assert_eq!(
            card.public_json(),
            br#"{"a":"2026-10-24","b":"2026-10-24x","c":"12-10-24"}"#
        );
    }

    #[test]
    fn parse_caps_serialized_public_at_3072_bytes_spec_3_5() {
        let at_cap = |filler: &str, count: usize| {
            card_with_public(&format!("  k: '{}'\n", filler.repeat(count)))
        };
        let json_overhead = br#"{"k":""}"#.len();
        let filler_bytes_at_cap = PUBLIC_MAX_BYTES - json_overhead;

        let card = at_cap("x", filler_bytes_at_cap).unwrap();
        assert_eq!(card.public_json().len(), 3072);
        assert_eq!(
            at_cap("x", filler_bytes_at_cap + 1),
            Err(CardInvalid::PublicOverCap { bytes: 3073 })
        );

        let two_byte_chars = filler_bytes_at_cap / 2;
        assert_eq!(
            at_cap("é", two_byte_chars).unwrap().public_json().len(),
            3072
        );
        assert_eq!(
            at_cap("é", two_byte_chars + 1),
            Err(CardInvalid::PublicOverCap { bytes: 3074 })
        );

        let escaped_quotes = filler_bytes_at_cap / 2;
        assert_eq!(
            at_cap("\"", escaped_quotes).unwrap().public_json().len(),
            3072
        );
        assert_eq!(
            at_cap("\"", escaped_quotes + 1),
            Err(CardInvalid::PublicOverCap { bytes: 3074 })
        );
    }

    #[test]
    fn parse_refuses_a_duplicate_key_in_public_spec_3_5() {
        assert_eq!(
            card_with_public("  a: 1\n  a: 2\n"),
            Err(CardInvalid::DuplicatePublicKey("a".to_owned()))
        );
        assert_eq!(
            card_with_public("  outer:\n    b: 1\n    \"b\": 2\n"),
            Err(CardInvalid::DuplicatePublicKey("b".to_owned()))
        );
    }

    #[test]
    fn parse_refuses_a_merge_key_in_public_but_keeps_a_quoted_one_spec_3_5() {
        assert_eq!(
            card_with_public("  base: {a: 1}\n  <<: {b: 2}\n"),
            Err(CardInvalid::MergeKey)
        );
        assert_eq!(
            card_with_public("  outer:\n    <<: {b: 2}\n"),
            Err(CardInvalid::MergeKey)
        );
        let card = card_with_public("  \"<<\": 1\n").unwrap();
        assert_eq!(card.public_json(), br#"{"<<":1}"#);
    }

    #[test]
    fn parse_refuses_a_non_string_key_in_public_spec_3_5() {
        for key in ["1", "true", "null", "~", "1.5", "[a]", "{b: c}", "0x10"] {
            assert_eq!(
                card_with_public(&format!("  ? {key}\n  : v\n")),
                Err(CardInvalid::NonStringKey),
                "{key}"
            );
        }
        let card = card_with_public("  \"1\": a\n  'true': b\n").unwrap();
        assert_eq!(card.public_json(), br#"{"1":"a","true":"b"}"#);
    }

    #[test]
    fn with_description_inserts_under_public_description_and_replaces_an_existing_one_spec_3_10_init()
     {
        let bare = parse_card("handle: ada\npublic_key: abc\n").unwrap();
        let described = bare.with_description("builds things").unwrap();
        assert_eq!(
            described.public_json(),
            br#"{"description":"builds things"}"#
        );

        let existing = card_with_public("  description: old\n  role: builder\n").unwrap();
        let replaced = existing.with_description("new").unwrap();
        assert_eq!(
            replaced.public_json(),
            br#"{"description":"new","role":"builder"}"#
        );
        assert_eq!(
            (replaced.handle.as_str(), replaced.public_key.as_str()),
            ("ada", "abc")
        );
    }

    #[test]
    fn with_description_caps_public_after_insertion_at_3072_bytes_spec_3_10_init() {
        let json_overhead = br#"{"description":""}"#.len();
        let filler_bytes_at_cap = PUBLIC_MAX_BYTES - json_overhead;
        let bare = || parse_card("handle: ada\npublic_key: abc\n").unwrap();

        let at_cap = bare()
            .with_description(&"x".repeat(filler_bytes_at_cap))
            .unwrap();
        assert_eq!(at_cap.public_json().len(), 3072);
        assert_eq!(
            bare().with_description(&"x".repeat(filler_bytes_at_cap + 1)),
            Err(DescriptionOverCap { bytes: 3073 })
        );

        let near_cap = card_with_public(&format!("  k: '{}'\n", "y".repeat(3000))).unwrap();
        assert_eq!(
            near_cap.with_description(&"x".repeat(100)),
            Err(DescriptionOverCap {
                bytes: 3000 + br#"{"k":"","description":""}"#.len() + 100
            })
        );
    }

    #[test]
    fn parse_resolves_an_alias_to_its_value_before_the_public_checks_spec_3_5() {
        let card = parse_card(
            "handle: ada\npublic_key: abc\nshared: &outside 5\npublic:\n  base: &list [1, x]\n  copy: *list\n  map: &map {k: v}\n  again: *map\n  out: *outside\n",
        )
        .unwrap();
        assert_eq!(
            card.public_json(),
            br#"{"again":{"k":"v"},"base":[1,"x"],"copy":[1,"x"],"map":{"k":"v"},"out":5}"#
        );

        let refused = |anchored: &str, alias_line: &str| {
            parse_card(&format!(
                "handle: ada\npublic_key: abc\npublic:\n  base: &a {anchored}\n{alias_line}"
            ))
        };
        assert_eq!(
            refused("2026-10-24", "  copy: *a\n"),
            Err(CardInvalid::Date)
        );
        assert_eq!(refused("!custom x", "  copy: *a\n"), Err(CardInvalid::Tag));
        assert_eq!(
            refused("!!binary aGk=", "  copy: *a\n"),
            Err(CardInvalid::Binary)
        );
        assert_eq!(
            refused("1", "  ? *a\n  : v\n"),
            Err(CardInvalid::NonStringKey)
        );
        assert_eq!(refused("<<", "  ? *a\n  : v\n"), Err(CardInvalid::MergeKey));
        assert_eq!(
            parse_card(
                "handle: ada\npublic_key: abc\npublic:\n  key: 1\n  x: &a key\n  ? *a\n  : v\n"
            ),
            Err(CardInvalid::DuplicatePublicKey("key".to_owned()))
        );
        assert!(matches!(
            parse_card("handle: ada\npublic_key: abc\npublic: &a\n  self: *a\n"),
            Err(CardInvalid::Unparsable(_))
        ));
    }

    #[test]
    fn parse_bounds_alias_expansion_by_the_public_cap_in_nodes_spec_3_5() {
        let aliases = |count: usize| {
            parse_card(&format!(
                "handle: ada\npublic_key: abc\nanchor: &a x\nextra: [{}]\n",
                vec!["*a"; count].join(",")
            ))
        };
        assert_eq!(aliases(PUBLIC_MAX_BYTES).map(|_| ()), Ok(()));
        assert_eq!(
            aliases(PUBLIC_MAX_BYTES + 1),
            Err(CardInvalid::AliasExpansionTooLarge)
        );

        let nested_levels = (0..7)
            .zip(1..8)
            .map(|(previous, level)| {
                let fan_out = vec![format!("*l{previous}"); 10].join(",");
                format!("l{level}: &l{level} [{fan_out}]\n")
            })
            .collect::<Vec<_>>()
            .concat();
        let nested = format!(
            "handle: ada\npublic_key: abc\nl0: &l0 [x, x, x, x, x, x, x, x, x, x]\n{nested_levels}"
        );
        assert_eq!(
            parse_card(&nested),
            Err(CardInvalid::AliasExpansionTooLarge)
        );
    }

    #[test]
    fn parse_keeps_integers_within_u64_exact_and_refuses_wider_ones_spec_3_5() {
        let card = card_with_public(
            "  max: 18446744073709551615\n  min: -9223372036854775808\n  plus: +18446744073709551615\n  float: 1.5e3\n",
        )
        .unwrap();
        assert_eq!(
            card.public_json(),
            br#"{"float":1500.0,"max":18446744073709551615,"min":-9223372036854775808,"plus":18446744073709551615}"#
        );
        for wide in [
            "18446744073709551616",
            "-9223372036854775809",
            "12345678901234567890123",
        ] {
            assert_eq!(
                card_with_public(&format!("  n: {wide}\n")),
                Err(CardInvalid::IntegerTooWide),
                "{wide}"
            );
        }
    }

    #[test]
    fn parse_refuses_a_number_json_cannot_hold_in_public_spec_3_5() {
        for number in [".inf", "-.inf", ".nan"] {
            assert_eq!(
                card_with_public(&format!("  n: {number}\n")),
                Err(CardInvalid::NumberNotFinite),
                "{number}"
            );
        }
        let card = card_with_public("  i: -7\n  f: 1.5\n  h: 0x10\n").unwrap();
        assert_eq!(card.public_json(), br#"{"f":1.5,"h":16,"i":-7}"#);
    }

    #[test]
    fn parse_refuses_a_card_that_is_not_one_mapping_with_its_required_keys_spec_3_5() {
        assert!(matches!(
            parse_card("handle: [unclosed"),
            Err(CardInvalid::Unparsable(_))
        ));
        for text in [
            "",
            "- a\n- b\n",
            "just text\n",
            "handle: ada\npublic_key: abc\n---\nhandle: bob\n",
        ] {
            assert_eq!(
                parse_card(text),
                Err(CardInvalid::NotOneMappingDocument),
                "{text:?}"
            );
        }
        assert_eq!(
            parse_card("public_key: abc\n"),
            Err(CardInvalid::MissingKey("handle"))
        );
        assert_eq!(
            parse_card("handle: ada\n"),
            Err(CardInvalid::MissingKey("public_key"))
        );
        assert_eq!(
            parse_card("handle: ada\nhandle: bob\npublic_key: abc\n"),
            Err(CardInvalid::DuplicateCardKey("handle"))
        );
        assert_eq!(
            parse_card("handle: [ada]\npublic_key: abc\n"),
            Err(CardInvalid::KeyNotScalar("handle"))
        );
        assert_eq!(
            parse_card("handle: ada\npublic_key: abc\npublic: [1]\n"),
            Err(CardInvalid::PublicNotMapping)
        );
        assert_eq!(
            parse_card("handle: ada\npublic_key: abc\npublic:\n"),
            Err(CardInvalid::PublicNotMapping)
        );
    }

    #[test]
    fn parse_ignores_every_key_beyond_the_four_even_when_odd_spec_3_5() {
        let text = "handle: ada\npublic_key: abc\nextra: !custom 2026-10-24\n<<: x\n? [a]\n: b\nextra: 1\n";
        let card = parse_card(text).unwrap();
        assert_eq!(card.public_json(), b"{}");
        assert_eq!(card.network, None);
    }

    #[test]
    fn parse_refuses_binary_data_in_public_spec_3_5() {
        assert_eq!(
            card_with_public("  a: !!binary aGVsbG8=\n"),
            Err(CardInvalid::Binary)
        );
        assert_eq!(
            card_with_public("  a: !!binary \"aGVsbG8=\"\n"),
            Err(CardInvalid::Binary)
        );
    }

    #[test]
    fn parse_refuses_a_tag_anywhere_in_public_spec_3_5() {
        assert_eq!(card_with_public("  a: !custom x\n"), Err(CardInvalid::Tag));
        assert_eq!(card_with_public("  a: !!str x\n"), Err(CardInvalid::Tag));
        assert_eq!(
            card_with_public("  a: !custom [x]\n"),
            Err(CardInvalid::Tag)
        );
        assert_eq!(
            card_with_public("  a:\n    b: !custom {c: d}\n"),
            Err(CardInvalid::Tag)
        );
        assert_eq!(card_with_public("  !custom a: x\n"), Err(CardInvalid::Tag));
    }
}
