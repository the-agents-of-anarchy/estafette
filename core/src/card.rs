use std::collections::{BTreeSet, HashMap};
use std::fmt;
use std::mem;
use std::rc::Rc;
use std::str::FromStr;

use serde::de::{self, Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Number, Value};
use yaml_rust2::Yaml;
use yaml_rust2::parser::{Event, MarkedEventReceiver, Parser, Tag};
use yaml_rust2::scanner::{Marker, ScanError, TScalarStyle};

use crate::handle::{AnnouncedHandle, Handle, HandleError};
use crate::keys::{KeyError, parse_public_key};

const PUBLIC_MAX_BYTES: usize = 3072;
/// The deepest nesting of mappings and sequences a member's JSON parser receives: `serde_json`'s recursion limit of 128 admits 127 levels.
pub(crate) const PUBLIC_DEEPEST_NESTING: usize = 127;
const JSON_QUOTE: u8 = b'"';
const JSON_ESCAPE: u8 = b'\\';
const JSON_MINUS: u8 = b'-';
const JSON_NUMBER_BYTES: &[u8] = b"0123456789+-.eE";
const JSON_FRACTION_OR_EXPONENT: [char; 3] = ['.', 'e', 'E'];
const JSON_OPEN_MAPPING: u8 = b'{';
const JSON_OPEN_SEQUENCE: u8 = b'[';
const JSON_CLOSE_MAPPING: u8 = b'}';
const JSON_CLOSE_SEQUENCE: u8 = b']';
const ALIAS_EXPANSION_MAX_NODES: usize = 4096;
const CARD_DEEPEST_NESTING: usize = 256;
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
    PublicTooDeep { depth: usize },
    AliasExpansionTooLarge,
    NestingTooDeep,
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
            Self::PublicTooDeep { depth } => write!(
                formatter,
                "card `public` nests mappings and sequences {depth} deep; the most a member receives is {PUBLIC_DEEPEST_NESTING}"
            ),
            Self::AliasExpansionTooLarge => write!(
                formatter,
                "card aliases expand to more than {ALIAS_EXPANSION_MAX_NODES} nodes"
            ),
            Self::NestingTooDeep => write!(
                formatter,
                "card nests mappings and sequences more than {CARD_DEEPEST_NESTING} deep"
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

    /// The handle the card requests and its public key, once the card is usable on the network.
    ///
    /// # Errors
    ///
    /// Returns [`CardUnusable`] when the handle breaks the handle grammar, the public key is not an Ed25519 key, or the card names another network.
    pub fn check_usable_on(&self, network: &str) -> Result<UsableCard, CardUnusable> {
        let handle: Handle = self.handle.parse().map_err(CardUnusable::Handle)?;
        let requested_handle = handle.as_str().parse().map_err(CardUnusable::Handle)?;
        let public_key = parse_public_key(&self.public_key).map_err(CardUnusable::PublicKey)?;
        match &self.network {
            Some(named) if named != network => Err(CardUnusable::OtherNetwork {
                named: named.clone(),
            }),
            Some(_) | None => Ok(UsableCard {
                requested_handle,
                public_key,
            }),
        }
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

/// What a card holds once it is usable on a network.
#[derive(Debug)]
pub struct UsableCard {
    pub requested_handle: AnnouncedHandle,
    pub public_key: iroh::PublicKey,
}

/// Why a card cannot be used on a network, mapped to `card_invalid` by the MCP server.
#[derive(Debug)]
pub enum CardUnusable {
    Handle(HandleError),
    PublicKey(KeyError),
    OtherNetwork { named: String },
}

impl fmt::Display for CardUnusable {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Handle(source) => write!(formatter, "handle refused: {source}"),
            Self::PublicKey(source) => {
                write!(formatter, "public key refused: {source}")
            }
            Self::OtherNetwork { named } => {
                write!(formatter, "names network {named}, not this one")
            }
        }
    }
}

impl std::error::Error for CardUnusable {}

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

/// Whether `public` of that many serialized bytes is within the cap.
const fn is_within_public_cap(bytes: usize) -> bool {
    bytes <= PUBLIC_MAX_BYTES
}

/// A node held by its parent and by every alias to it, so an alias copies a pointer, not a subtree.
type Shared = Rc<Node>;

/// A mapping's key and its value.
type Entry = (Shared, Shared);

/// One YAML node of a card, with its tag and with every alias already replaced by its anchored value.
enum Node {
    Scalar {
        text: String,
        style: TScalarStyle,
        tag: Option<Tag>,
    },
    Sequence {
        tag: Option<Tag>,
        items: Vec<Shared>,
    },
    Mapping {
        tag: Option<Tag>,
        entries: Vec<Entry>,
    },
}

impl Node {
    /// The children, moved out of the node.
    fn take_children(&mut self) -> Vec<Shared> {
        match self {
            Self::Scalar { .. } => Vec::new(),
            Self::Sequence { items, .. } => mem::take(items),
            Self::Mapping { entries, .. } => mem::take(entries)
                .into_iter()
                .flat_map(<[Shared; 2]>::from)
                .collect(),
        }
    }
}

impl Drop for Node {
    /// Releases the descendants one at a time, so a deeply nested node cannot recurse the stack.
    fn drop(&mut self) {
        let mut pending = self.take_children();
        while let Some(shared) = pending.pop() {
            if let Ok(mut node) = Rc::try_unwrap(shared) {
                pending.append(&mut node.take_children());
            }
        }
    }
}

/// A sequence or mapping whose closing event has not arrived yet, with its anchor id.
enum OpenNode {
    Sequence {
        anchor: usize,
        tag: Option<Tag>,
        items: Vec<Shared>,
    },
    Mapping {
        anchor: usize,
        tag: Option<Tag>,
        entries: Vec<Entry>,
        pending_key: Option<Shared>,
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
    documents: Vec<Shared>,
    anchored: HashMap<usize, AnchoredNode>,
    expanded_nodes: usize,
    refusal: Option<CardInvalid>,
    nesting_exceeded: bool,
}

/// A finished anchored node and the number of nodes it holds.
struct AnchoredNode {
    node: Shared,
    node_count: usize,
}

impl DocumentCollector {
    /// Puts a finished node where the parser is: a sequence item, a mapping key or value, or a document.
    fn place(&mut self, node: Shared) {
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
        let node = Rc::new(node);
        if anchor != NO_ANCHOR {
            let node_count = count_nodes(&node);
            self.anchored.insert(
                anchor,
                AnchoredNode {
                    node: Rc::clone(&node),
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
        if expanded_nodes > ALIAS_EXPANSION_MAX_NODES {
            self.refuse(CardInvalid::AliasExpansionTooLarge);
            return;
        }
        let node = Rc::clone(&target.node);
        self.expanded_nodes = expanded_nodes;
        self.place(node);
    }

    /// Opens a sequence or mapping; past the bound inside `public` it refuses and stops reading.
    fn open_within_nesting_bound(&mut self, node: OpenNode) {
        if self.open.len() >= CARD_DEEPEST_NESTING && self.is_inside_public() {
            self.nesting_exceeded = true;
            self.refuse(CardInvalid::NestingTooDeep);
        } else {
            self.open.push(node);
        }
    }

    /// Whether the node being read is the value of the document's `public` key.
    fn is_inside_public(&self) -> bool {
        matches!(
            self.open.first(),
            Some(OpenNode::Mapping { pending_key: Some(key), .. })
                if matches!(&**key, Node::Scalar { text, .. } if text == PUBLIC_FIELD)
        )
    }

    /// Keeps the first refusal and drops any later one.
    fn refuse(&mut self, refusal: CardInvalid) {
        self.refusal.get_or_insert(refusal);
    }
}

/// The number of nodes in the tree, the node itself included, counted without recursion.
fn count_nodes(root: &Node) -> usize {
    let mut counted: usize = 0;
    let mut pending = vec![root];
    while let Some(node) = pending.pop() {
        counted = counted.saturating_add(1);
        match node {
            Node::Scalar { .. } => {}
            Node::Sequence { items, .. } => pending.extend(items.iter().map(|item| &**item)),
            Node::Mapping { entries, .. } => pending.extend(
                entries
                    .iter()
                    .flat_map(|(key, value)| [key, value])
                    .map(|child| &**child),
            ),
        }
    }
    counted
}

/// The most mappings and sequences one inside another in the tree, counted without recursion.
fn deepest_nesting(root: &Node) -> usize {
    let mut deepest: usize = 0;
    let mut pending = vec![(root, 0_usize)];
    while let Some((node, enclosing)) = pending.pop() {
        let children: Vec<&Shared> = match node {
            Node::Scalar { .. } => continue,
            Node::Sequence { items, .. } => items.iter().collect(),
            Node::Mapping { entries, .. } => entries
                .iter()
                .flat_map(|(key, value)| [key, value])
                .collect(),
        };
        let level = enclosing.saturating_add(1);
        deepest = deepest.max(level);
        pending.extend(children.into_iter().map(|child| (&**child, level)));
    }
    deepest
}

impl MarkedEventReceiver for DocumentCollector {
    fn on_event(&mut self, event: Event, _mark: Marker) {
        match event {
            Event::Scalar(text, style, anchor, tag) => {
                self.place_remembering_anchor(anchor, Node::Scalar { text, style, tag });
            }
            Event::Alias(anchor) => self.place_alias_target(anchor),
            Event::SequenceStart(anchor, tag) => {
                self.open_within_nesting_bound(OpenNode::Sequence {
                    anchor,
                    tag,
                    items: Vec::new(),
                });
            }
            Event::MappingStart(anchor, tag) => {
                self.open_within_nesting_bound(OpenNode::Mapping {
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

/// Hands the parser's events to the collector one at a time, stopping at the end of the stream or at the first nesting past the bound.
fn feed_events_until_end_or_nesting_exceeded(
    text: &str,
    collector: &mut DocumentCollector,
) -> Result<(), ScanError> {
    let mut parser = Parser::new_from_str(text);
    loop {
        let (event, mark) = parser.next_token()?;
        let stream_ended = event == Event::StreamEnd;
        collector.on_event(event, mark);
        if stream_ended || collector.nesting_exceeded {
            return Ok(());
        }
    }
}

/// The single document of the text, or the refusal for unparsable text, an alias refusal, or a wrong document count.
fn read_single_document(text: &str) -> Result<Shared, CardInvalid> {
    let mut collector = DocumentCollector::default();
    feed_events_until_end_or_nesting_exceeded(text, &mut collector)
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
fn mapping_entries(node: &Node) -> Result<&[Entry], CardInvalid> {
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
    entries: &'card [Entry],
    name: &'static str,
) -> Result<Option<&'card Node>, CardInvalid> {
    let mut matching = entries
        .iter()
        .filter(|(key, _)| scalar_text_of(key) == Some(name))
        .map(|(_, value)| &**value);
    let first = matching.next();
    match matching.next() {
        None => Ok(first),
        Some(_) => Err(CardInvalid::DuplicateCardKey(name)),
    }
}

/// The text of the scalar under the key, None when the key is absent, refusing a non-scalar value.
fn optional_scalar_text(
    entries: &[Entry],
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
fn required_scalar_text(entries: &[Entry], name: &'static str) -> Result<String, CardInvalid> {
    optional_scalar_text(entries, name)?.ok_or(CardInvalid::MissingKey(name))
}

/// The `public` mapping as JSON, empty when absent, refusing a non-mapping or one over the cap.
fn public_mapping(entries: &[Entry]) -> Result<Map<String, Value>, CardInvalid> {
    let mapping = match value_of_unique_key(entries, PUBLIC_FIELD)? {
        None => Map::new(),
        Some(node) => match convert_value(refuse_nesting_beyond_bound(node)?)? {
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
    let mapping = refuse_over_cap(mapping).map_err(|bytes| CardInvalid::PublicOverCap { bytes })?;
    refuse_too_deep(mapping)
}

/// The node itself, or the refusal when it nests deeper than the bound the conversion can follow.
fn refuse_nesting_beyond_bound(node: &Node) -> Result<&Node, CardInvalid> {
    if deepest_nesting(node) > CARD_DEEPEST_NESTING {
        return Err(CardInvalid::NestingTooDeep);
    }
    Ok(node)
}

/// The mapping itself, or the refusal naming its depth when it nests deeper than a member receives.
fn refuse_too_deep(mapping: Map<String, Value>) -> Result<Map<String, Value>, CardInvalid> {
    let depth = json_nesting_depth(&compact_json(&mapping));
    if depth > PUBLIC_DEEPEST_NESTING {
        return Err(CardInvalid::PublicTooDeep { depth });
    }
    Ok(mapping)
}

/// The compact JSON bytes of the mapping, without whitespace.
fn compact_json(mapping: &Map<String, Value>) -> Vec<u8> {
    Value::Object(mapping.clone()).to_string().into_bytes()
}

/// Whether `public` bytes a peer sent are what a card sends: a JSON mapping within the cap and the deepest nesting whose mappings name each key once and whose integers fit 64 bits, judged on the bytes as they arrived (spec 3.5).
#[must_use]
pub(crate) fn is_card_public(bytes: &[u8]) -> bool {
    is_within_public_cap(bytes.len())
        && serde_json::from_slice::<ReceivedPublicMapping>(bytes).is_ok()
        && every_integer_fits_64_bits(bytes)
}

/// A received `public` that is a JSON mapping in which no mapping, at any depth, names a key twice.
struct ReceivedPublicMapping;

impl<'de> Deserialize<'de> for ReceivedPublicMapping {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_map(UniqueKeys).map(|()| Self)
    }
}

/// A JSON value inside a received `public` in which no mapping names a key twice.
struct ReceivedPublicValue;

impl<'de> Deserialize<'de> for ReceivedPublicValue {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(UniqueKeys).map(|()| Self)
    }
}

/// The visitor that takes any JSON value a card holds and refuses a mapping that names a key twice.
struct UniqueKeys;

impl<'de> Visitor<'de> for UniqueKeys {
    type Value = ();

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("JSON whose mappings name each key once")
    }

    fn visit_bool<E: de::Error>(self, _flag: bool) -> Result<(), E> {
        Ok(())
    }

    fn visit_i64<E: de::Error>(self, _number: i64) -> Result<(), E> {
        Ok(())
    }

    fn visit_u64<E: de::Error>(self, _number: u64) -> Result<(), E> {
        Ok(())
    }

    fn visit_f64<E: de::Error>(self, _number: f64) -> Result<(), E> {
        Ok(())
    }

    fn visit_str<E: de::Error>(self, _text: &str) -> Result<(), E> {
        Ok(())
    }

    fn visit_unit<E: de::Error>(self) -> Result<(), E> {
        Ok(())
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut items: A) -> Result<(), A::Error> {
        while items.next_element::<ReceivedPublicValue>()?.is_some() {}
        Ok(())
    }

    fn visit_map<A: MapAccess<'de>>(self, mut entries: A) -> Result<(), A::Error> {
        let mut keys = BTreeSet::new();
        while let Some(key) = entries.next_key::<String>()? {
            entries.next_value::<ReceivedPublicValue>()?;
            if !keys.insert(key.clone()) {
                return Err(de::Error::custom(CardInvalid::DuplicatePublicKey(key)));
            }
        }
        Ok(())
    }
}

/// Whether every integer a valid JSON text writes outside its strings fits 64 bits, signed or unsigned, as every integer a card holds does (spec 3.5).
fn every_integer_fits_64_bits(json: &[u8]) -> bool {
    json_number_tokens(json)
        .iter()
        .all(|token| token.contains(JSON_FRACTION_OR_EXPONENT) || fits_64_bits(token))
}

/// Whether the integer text parses as a signed or an unsigned 64-bit integer.
fn fits_64_bits(integer: &str) -> bool {
    integer.parse::<i64>().is_ok() || integer.parse::<u64>().is_ok()
}

/// The number tokens of a valid JSON text, in order, leaving out whatever its strings hold.
fn json_number_tokens(json: &[u8]) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut bytes = json.iter().copied().peekable();
    while let Some(byte) = bytes.next() {
        if byte == JSON_QUOTE {
            skip_rest_of_json_string(&mut bytes);
        } else if byte == JSON_MINUS || byte.is_ascii_digit() {
            let mut token = String::from(char::from(byte));
            while let Some(next) = bytes.next_if(|next| JSON_NUMBER_BYTES.contains(next)) {
                token.push(char::from(next));
            }
            tokens.push(token);
        }
    }
    tokens
}

/// The deepest nesting of mappings and sequences in a valid JSON text, the outermost counting one.
fn json_nesting_depth(json: &[u8]) -> usize {
    let mut depth: usize = 0;
    let mut deepest = 0;
    let mut bytes = json.iter().copied();
    while let Some(byte) = bytes.next() {
        match byte {
            JSON_QUOTE => skip_rest_of_json_string(&mut bytes),
            JSON_OPEN_MAPPING | JSON_OPEN_SEQUENCE => {
                depth = depth.saturating_add(1);
                deepest = deepest.max(depth);
            }
            JSON_CLOSE_MAPPING | JSON_CLOSE_SEQUENCE => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    deepest
}

/// Consumes a JSON string's bytes up to and including its closing quote.
fn skip_rest_of_json_string(bytes: &mut impl Iterator<Item = u8>) {
    let mut escaped = false;
    for byte in bytes {
        if escaped {
            escaped = false;
        } else if byte == JSON_ESCAPE {
            escaped = true;
        } else if byte == JSON_QUOTE {
            return;
        }
    }
}

/// The mapping itself, or the byte size of its compact JSON when that passes the cap.
fn refuse_over_cap(mapping: Map<String, Value>) -> Result<Map<String, Value>, usize> {
    let bytes = compact_json(&mapping).len();
    if !is_within_public_cap(bytes) {
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
            .map(|item| convert_value(item))
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
fn convert_mapping(entries: &[Entry]) -> Result<Value, CardInvalid> {
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
pub(crate) mod tests {
    use super::*;
    use crate::seeded_draws::Draws;

    pub const SPEC_PUBLIC_CAP_BYTES: usize = 3072;
    const MAPPING_OF_ONE_EMPTY_TEXT: &str = r#"{"k":""}"#;

    pub fn public_mapping_of_bytes(bytes: usize) -> Vec<u8> {
        let filler = "x".repeat(bytes.checked_sub(MAPPING_OF_ONE_EMPTY_TEXT.len()).unwrap());
        let public = format!(r#"{{"k":"{filler}"}}"#).into_bytes();
        assert_eq!(public.len(), bytes);
        public
    }

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

    const VALID_PUBLIC_KEY: &str =
        "ea4a6c63e29c520abef5507b132ec5f9954776aebebe7b92421eea691446d22c";
    const HANDLE_OF_32_CHARACTERS: &str = "abcdefghijklmnopqrstuvwxyzabcdef";
    const HANDLE_OF_34_CHARACTERS: &str = "abcdefghijklmnopqrstuvwxyzabcdef-2";

    fn card_with_key_and(card_lines: &str) -> Card {
        parse_card(&format!("public_key: {VALID_PUBLIC_KEY}\n{card_lines}")).unwrap()
    }

    fn usable_on(network: &str, card_lines: &str) -> Result<UsableCard, CardUnusable> {
        card_with_key_and(card_lines).check_usable_on(network)
    }

    #[test]
    fn check_usable_on_hands_back_the_requested_handle_and_key_of_a_card_of_this_network_or_of_none_spec_3_5()
     {
        for card_lines in ["handle: ada\n", "handle: ada\nnetwork: lab\n"] {
            let usable = usable_on("lab", card_lines).unwrap();
            assert_eq!(usable.requested_handle.as_str(), "ada");
            assert_eq!(
                crate::keys::render_public_key(&usable.public_key),
                VALID_PUBLIC_KEY
            );
        }
    }

    #[test]
    fn check_usable_on_refuses_a_handle_past_the_32_characters_of_the_handle_grammar_though_it_is_a_suffixed_announced_handle_spec_3_1()
     {
        let card_lines = format!("handle: {HANDLE_OF_34_CHARACTERS}\n");
        assert!(matches!(
            usable_on("lab", &card_lines),
            Err(CardUnusable::Handle(_))
        ));
        let at_the_limit = format!("handle: {HANDLE_OF_32_CHARACTERS}\n");
        usable_on("lab", &at_the_limit).unwrap();
    }

    #[test]
    fn check_usable_on_refuses_a_card_naming_another_network_spec_3_5() {
        match usable_on("lab", "handle: ada\nnetwork: acme\n") {
            Err(CardUnusable::OtherNetwork { named }) => assert_eq!(named, "acme"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn check_usable_on_refuses_a_public_key_that_is_not_an_ed25519_key_spec_3_5() {
        let card = parse_card("handle: ada\npublic_key: abc\n").unwrap();
        assert!(matches!(
            card.check_usable_on("lab"),
            Err(CardUnusable::PublicKey(_))
        ));
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

    const ALIAS_EXPANSION_LIMIT_NODES: usize = 4096;

    #[test]
    fn parse_bounds_alias_expansion_at_4096_nodes_apart_from_the_public_cap_spec_3_5() {
        let aliases = |count: usize| {
            parse_card(&format!(
                "handle: ada\npublic_key: abc\nanchor: &a x\nextra: [{}]\n",
                vec!["*a"; count].join(",")
            ))
        };
        assert_eq!(aliases(ALIAS_EXPANSION_LIMIT_NODES).map(|_| ()), Ok(()));
        assert_eq!(
            aliases(ALIAS_EXPANSION_LIMIT_NODES + 1),
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
            parse_card("handle: ada\npublic_key: abc\nnetwork: [lab]\n"),
            Err(CardInvalid::KeyNotScalar("network"))
        );
        assert_eq!(
            parse_card("handle: ada\npublic_key: abc\nnetwork: lab\nnetwork: lab\n"),
            Err(CardInvalid::DuplicateCardKey("network"))
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

    #[test]
    fn a_received_public_the_card_check_refuses_is_not_a_card_public_spec_3_5() {
        let over_cap = public_mapping_of_bytes(SPEC_PUBLIC_CAP_BYTES + 1);
        let refused: [&[u8]; 13] = [
            br#"{"a":1,"a":2}"#,
            br#"{"a":{"b":1,"b":2}}"#,
            br#"{"a":[{"b":1,"b":2}]}"#,
            br#"{"a":1,"\u0061":2}"#,
            br#"{"a":18446744073709551616}"#,
            br#"{"a":-9223372036854775809}"#,
            br#"{"a":[1,18446744073709551616]}"#,
            br#"{"a":1e400}"#,
            br#"["a"]"#,
            br#""a""#,
            b"not json",
            b"",
            &over_cap,
        ];
        for public in refused {
            assert!(
                !is_card_public(public),
                "{}",
                String::from_utf8_lossy(public)
            );
        }
    }

    #[test]
    fn a_received_public_at_the_edges_the_card_check_admits_is_a_card_public_spec_3_5() {
        let at_cap = public_mapping_of_bytes(SPEC_PUBLIC_CAP_BYTES);
        let admitted: [&[u8]; 9] = [
            b"{}",
            br#"{"a":18446744073709551615}"#,
            br#"{"a":-9223372036854775808}"#,
            br#"{"a":1e19,"b":-2.5e-3,"c":1.5}"#,
            br#"{"a":{"a":{"a":1}},"b":[{"a":1},{"a":2}]}"#,
            br#"{"a":"18446744073709551616","b":"1,\"a\":2"}"#,
            br#"{"<<":"a merge key text","a":null,"b":true}"#,
            br#"{ "a" : [ 1 , 2 ] }"#,
            &at_cap,
        ];
        for public in admitted {
            assert!(
                is_card_public(public),
                "{}",
                String::from_utf8_lossy(public)
            );
        }
    }

    fn card_with_ignored_key_nested(depth: usize) -> Result<Card, CardInvalid> {
        parse_card(&format!(
            "handle: ada\npublic_key: abc\nignored:\n{}1\n",
            "- ".repeat(depth)
        ))
    }

    #[test]
    fn a_card_keeps_an_ignored_key_nested_beyond_the_nesting_bound_valid_spec_3_5() {
        for depth in [CARD_DEEPEST_NESTING, 300, 2000, FAR_BEYOND_BOUND_LEVELS] {
            let card = card_with_ignored_key_nested(depth).unwrap();
            assert_eq!(card.public_json(), b"{}");
        }
    }

    const DEEP_ANCHORED_LEVELS: usize = 1000;
    const FAR_BEYOND_BOUND_LEVELS: usize = 100_000;

    fn deep_anchored_container() -> String {
        format!(
            "&anchor\n{}{}7",
            " ".repeat(CARD_DEEPEST_NESTING * 2),
            "- ".repeat(DEEP_ANCHORED_LEVELS)
        )
    }

    fn card_aliasing_into_public_an_anchor_beyond_the_bound(
        anchored_node: &str,
    ) -> Result<Card, CardInvalid> {
        let beyond_bound = "- ".repeat(CARD_DEEPEST_NESTING);
        parse_card(&format!(
            "handle: ada\npublic_key: abc\nignored:\n{beyond_bound}{anchored_node}\npublic:\n  seen: *anchor\n"
        ))
    }

    #[test]
    fn an_alias_to_a_scalar_anchored_beyond_the_nesting_bound_resolves_to_its_value_spec_3_5() {
        let card = card_aliasing_into_public_an_anchor_beyond_the_bound("&anchor 7").unwrap();
        assert_eq!(card.public_json(), br#"{"seen":7}"#);
    }

    #[test]
    fn an_alias_to_a_container_anchored_beyond_the_nesting_bound_resolves_to_its_value_spec_3_5() {
        let card = card_aliasing_into_public_an_anchor_beyond_the_bound("&anchor [7]").unwrap();
        assert_eq!(card.public_json(), br#"{"seen":[7]}"#);
    }

    #[test]
    fn an_alias_to_a_container_nested_past_the_bound_is_refused_in_public_without_aborting_spec_3_5()
     {
        assert_eq!(
            card_aliasing_into_public_an_anchor_beyond_the_bound(&deep_anchored_container()),
            Err(CardInvalid::NestingTooDeep)
        );
    }

    #[test]
    fn an_alias_to_a_deep_container_stays_valid_in_an_ignored_key_spec_3_5() {
        let beyond_bound = "- ".repeat(CARD_DEEPEST_NESTING);
        let deep_container = deep_anchored_container();
        let card = parse_card(&format!(
            "handle: ada\npublic_key: abc\nignored:\n{beyond_bound}{deep_container}\nalso: *anchor\n"
        ))
        .unwrap();
        assert_eq!(card.public_json(), b"{}");
    }

    #[test]
    fn an_alias_bomb_beyond_the_nesting_bound_is_refused_by_the_expansion_bound_spec_3_5() {
        let beyond_bound = "- ".repeat(CARD_DEEPEST_NESTING);
        let aliases = "- *anchor\n".repeat(ALIAS_EXPANSION_MAX_NODES);
        assert_eq!(
            parse_card(&format!(
                "handle: ada\npublic_key: abc\nignored:\n{beyond_bound}&anchor [1, 2]\nalso:\n{aliases}"
            )),
            Err(CardInvalid::AliasExpansionTooLarge)
        );
    }

    #[test]
    fn a_public_nested_beyond_the_nesting_bound_is_refused_spec_3_5() {
        let beyond_bound = "- ".repeat(CARD_DEEPEST_NESTING);
        assert_eq!(
            parse_card(&format!(
                "handle: ada\npublic_key: abc\npublic:\n  k:\n  {beyond_bound}1\n"
            )),
            Err(CardInvalid::NestingTooDeep)
        );
    }

    fn card_nested(depth: usize) -> Result<Card, CardInvalid> {
        let opening = "{a: ".repeat(depth);
        let closing = "}".repeat(depth);
        parse_card(&format!(
            "handle: ada\npublic_key: abc\npublic: {opening}1{closing}\n"
        ))
    }

    #[test]
    fn a_card_public_nested_at_the_deepest_bound_parses_and_is_received_one_deeper_is_refused_spec_3_5()
     {
        let at_bound = card_nested(PUBLIC_DEEPEST_NESTING).unwrap();
        let one_deeper = card_nested(PUBLIC_DEEPEST_NESTING + 1);

        assert!(is_card_public(&at_bound.public_json()));
        assert_eq!(
            one_deeper,
            Err(CardInvalid::PublicTooDeep {
                depth: PUBLIC_DEEPEST_NESTING + 1
            })
        );
    }

    #[test]
    fn the_deepest_nesting_bound_is_the_deepest_the_json_parser_receives_spec_3_5() {
        let nested = |depth: usize| format!("{}1{}", r#"{"a":"#.repeat(depth), "}".repeat(depth));

        let at_bound = serde_json::from_str::<Value>(&nested(PUBLIC_DEEPEST_NESTING));
        let one_deeper = serde_json::from_str::<Value>(&nested(PUBLIC_DEEPEST_NESTING + 1));

        at_bound.unwrap();
        one_deeper.unwrap_err();
    }

    #[test]
    fn a_received_public_one_level_deeper_than_the_bound_is_not_a_card_public_spec_3_5() {
        let opening = r#"{"a":"#.repeat(PUBLIC_DEEPEST_NESTING + 1);
        let closing = "}".repeat(PUBLIC_DEEPEST_NESTING + 1);

        assert!(!is_card_public(format!("{opening}1{closing}").as_bytes()));
    }

    const PROPERTY_CASES: u64 = 2000;
    const LARGEST_MAPPING: u64 = 5;
    const LARGEST_SEQUENCE: u64 = 4;
    const DEEPEST_NESTING: u32 = 3;
    const VALUE_KINDS: u64 = 9;
    const FLOAT_TEXTS: [&str; 5] = ["1.5", "-2.5e-3", "1.0e19", "0.0", "123456789.125"];
    const WIDE_INTEGER_TEXTS: [&str; 4] = [
        "18446744073709551615",
        "9223372036854775808",
        "-9223372036854775808",
        "9223372036854775807",
    ];
    const SCALAR_TEXTS: [&str; 4] = ["true", "false", "null", "'18446744073709551616'"];
    const QUOTED_TEXT_WITH_JSON_PUNCTUATION: &str = r#""a, \"b\": [1e400] }""#;

    impl Draws {
        fn pick<'a>(&mut self, texts: &[&'a str]) -> &'a str {
            texts[usize::try_from(self.below(u64::try_from(texts.len()).unwrap())).unwrap()]
        }

        fn yaml_mapping(&mut self, depth: u32) -> String {
            let entries: Vec<String> = (0..self.below(LARGEST_MAPPING))
                .map(|index| format!("k{index}: {}", self.yaml_value(depth)))
                .collect();
            format!("{{{}}}", entries.join(", "))
        }

        fn yaml_sequence(&mut self, depth: u32) -> String {
            let items: Vec<String> = (0..self.below(LARGEST_SEQUENCE))
                .map(|_| self.yaml_value(depth))
                .collect();
            format!("[{}]", items.join(", "))
        }

        fn yaml_value(&mut self, depth: u32) -> String {
            let deeper = depth.checked_add(1).unwrap();
            match self.below(VALUE_KINDS) {
                0 => format!("\"text {}\"", self.below(u64::MAX)),
                1 => format!("{}", self.below(u64::MAX)),
                2 => format!("-{}", self.below(u64::try_from(i64::MAX).unwrap())),
                3 => self.pick(&WIDE_INTEGER_TEXTS).to_owned(),
                4 => self.pick(&FLOAT_TEXTS).to_owned(),
                5 => self.pick(&SCALAR_TEXTS).to_owned(),
                6 if depth < DEEPEST_NESTING => self.yaml_sequence(deeper),
                7 if depth < DEEPEST_NESTING => self.yaml_mapping(deeper),
                _ => QUOTED_TEXT_WITH_JSON_PUNCTUATION.to_owned(),
            }
        }
    }

    #[test]
    fn every_public_a_valid_card_serializes_is_a_card_public_spec_3_5() {
        let mut checked: u64 = 0;
        for case in 0..PROPERTY_CASES {
            let mut draws = Draws::seeded(case);
            let mapping = draws.yaml_mapping(0);
            let Ok(card) = parse_card(&format!(
                "handle: ada\npublic_key: abc\npublic: {mapping}\n"
            )) else {
                continue;
            };
            let public = card.public_json();
            assert!(
                is_card_public(&public),
                "case {case}: {}",
                String::from_utf8_lossy(&public)
            );
            checked = checked.checked_add(1).unwrap();
        }
        assert!(checked > PROPERTY_CASES / 2, "only {checked} cards parsed");
    }
}
