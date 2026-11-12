//! Every kind of export that Bimetal supports. The example's Python and
//! TypeScript tests call each kind.

pub mod columns;
pub mod shelf;
pub mod text;

use std::collections::{BTreeMap, HashMap};
use std::future;
use std::task::Poll;
use std::time::Duration;

use bimetal::Model;
use chrono::{DateTime, Datelike, Days, FixedOffset, NaiveDate, Utc, Weekday};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use showcase_geometry::{GeometryError, Point};

/// The version of this contract.
#[bimetal::export]
pub const CONTRACT_VERSION: u32 = 3;

/// The label that new records start with.
#[bimetal::export]
pub static DEFAULT_LABEL: &str = "untitled";

/// A stable identifier that hosts see as a plain string.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Model)]
#[serde(transparent)]
pub struct RecordId(pub String);

/// A score, shown to hosts as its number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Model)]
#[serde(transparent)]
pub struct Score(pub u8);

/// How urgent a record is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Model)]
#[serde(rename_all = "snake_case")]
pub enum Priority {
    /// Handle it when convenient.
    Low,
    /// Handle it in order.
    Normal,
    /// Handle it first.
    High,
}

/// Every integer and floating-point width.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Model)]
#[serde(rename_all = "camelCase")]
pub struct Numbers {
    pub flag: bool,
    pub tiny: u8,
    pub small: i16,
    pub medium: u32,
    pub signed: i32,
    /// 64-bit integers are `int` in Python and `bigint` in TypeScript.
    pub large: u64,
    pub signed_large: i64,
    pub single: f32,
    pub double: f64,
}

/// A record with strings, options, collections, dates, JSON, and bytes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Model)]
#[serde(rename_all = "camelCase")]
pub struct Record {
    pub id: RecordId,
    #[bimetal(min_length = 1, max_length = 40)]
    pub label: String,
    pub priority: Priority,
    pub score: Score,
    #[bimetal(ge = 1, le = 5)]
    pub stars: u8,
    /// A field that is always present, even though it may be empty.
    #[bimetal(required)]
    pub note: Option<String>,
    #[bimetal(max_length = 5)]
    pub tags: Vec<String>,
    pub counters: BTreeMap<String, u32>,
    /// Inclusive ranges by name.
    pub ranges: HashMap<String, (i32, i32)>,
    pub created_at: DateTime<Utc>,
    pub local_time: DateTime<FixedOffset>,
    pub metadata: Value,
    /// Inside a model, bytes are a list of integers.
    pub checksum: Vec<u8>,
    /// A model from the `geometry` namespace.
    pub location: Point,
    #[serde(default)]
    pub revision: u32,
}

/// Settings with defaults and a fixed value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Model)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    /// Always `"v1"`.
    #[bimetal(literal = "v1")]
    pub version: String,
    #[serde(default)]
    pub verbose: bool,
    #[serde(default)]
    pub prefix: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub retry: Retry,
    #[serde(default = "yes")]
    pub color: bool,
    #[serde(default = "default_icon")]
    pub icon: String,
    #[serde(default = "default_theme")]
    pub theme: Option<String>,
}

/// How often to try again, and how fast the wait grows.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Model)]
pub struct Retry {
    pub attempts: u32,
    pub factor: f64,
}

impl Default for Retry {
    fn default() -> Self {
        Self {
            attempts: 3,
            factor: 2.0,
        }
    }
}

fn yes() -> bool {
    true
}

fn default_icon() -> String {
    "brackets-curly".to_owned()
}

fn default_theme() -> Option<String> {
    Some("dark".to_owned())
}

/// Something that happened to a record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Model)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum Event {
    /// The record was created.
    Created { record_id: RecordId },
    /// The record moved.
    Moved { record_id: RecordId, to: Point },
    /// The record was deleted.
    Deleted {
        record_id: RecordId,
        reason: Option<String>,
    },
}

/// One step of a drawing, in Serde's default enum form.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Model)]
#[serde(rename_all = "snake_case")]
pub enum Stroke {
    /// Lift the pen.
    Lift,
    /// Move the pen to a point.
    MoveTo(Point),
    /// Draw a line to a point.
    LineTo {
        /// Where the line ends.
        to: Point,
        /// The line's width.
        width: f64,
    },
    /// Draw a stroke twice.
    Again(Box<Self>),
    /// Wait, for this many seconds when given.
    Pause(Option<f64>),
}

/// Where every drawing starts.
#[bimetal::export]
pub const FIRST_STROKE: Stroke = Stroke::MoveTo(Point { x: 0.0, y: 0.0 });

/// Failures to parse text into a value.
#[derive(Debug, thiserror::Error, bimetal::Error)]
pub enum ParseError {
    /// The text was empty.
    #[error("the text is empty")]
    Empty,
    /// The text names no known value.
    #[error("unknown value `{text}`")]
    #[serde(rename = "unknown_value")]
    Unknown { text: String },
}

/// A failure with a single code.
#[derive(Debug, thiserror::Error, bimetal::Error)]
#[error("the ledger is locked")]
pub struct LockedError;

/// Return the numbers unchanged.
#[bimetal::export]
#[must_use]
pub fn echo_numbers(numbers: Numbers) -> Numbers {
    numbers
}

/// Return the record unchanged.
#[bimetal::export]
#[must_use]
pub fn echo_record(record: Record) -> Record {
    record
}

/// Return the settings unchanged.
#[bimetal::export]
#[must_use]
pub fn echo_settings(settings: Settings) -> Settings {
    settings
}

/// The theme of optional settings: none without them.
#[bimetal::export]
#[must_use]
pub fn theme_of(settings: Option<Settings>) -> Option<String> {
    settings.and_then(|settings| settings.theme)
}

/// Return a copy of the record with a new revision and creation time.
#[bimetal::export]
#[must_use]
pub fn touch(record: &Record, at: DateTime<Utc>) -> Record {
    Record {
        revision: record.revision + 1,
        created_at: at,
        ..record.clone()
    }
}

/// Count how often each tag appears across records.
#[bimetal::export]
#[must_use]
pub fn count_tags(records: Vec<Record>) -> BTreeMap<String, u32> {
    let mut counts = BTreeMap::new();
    for tag in records.iter().flat_map(|record| &record.tags) {
        *counts.entry(tag.clone()).or_insert(0) += 1;
    }
    counts
}

/// Describe an event.
#[bimetal::export]
#[must_use]
pub fn describe(event: Event) -> String {
    match event {
        Event::Created { record_id } => format!("created {}", record_id.0),
        Event::Moved { record_id, to } => format!("moved {} to {}, {}", record_id.0, to.x, to.y),
        Event::Deleted { record_id, reason } => format!(
            "deleted {}{}",
            record_id.0,
            reason
                .map(|reason| format!(": {reason}"))
                .unwrap_or_default()
        ),
    }
}

/// Return the day after `date`, or `None` past the last day a date can be.
#[bimetal::export]
#[must_use]
pub fn next_day(#[bimetal(default = "2028-02-28")] date: NaiveDate) -> Option<NaiveDate> {
    date.succ_opt()
}

/// A public holiday: the day it falls on, and the day it is observed when
/// that is another.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Model)]
pub struct Holiday {
    /// The day it falls on.
    pub date: NaiveDate,
    /// The day it is observed, when that is another.
    pub observed: Option<NaiveDate>,
}

/// Return `holiday` observed on the Monday after it when it falls on a
/// weekend.
#[bimetal::export]
#[must_use]
pub fn observe(holiday: Holiday) -> Holiday {
    let observed = match holiday.date.weekday() {
        Weekday::Sat => holiday.date.checked_add_days(Days::new(2)),
        Weekday::Sun => holiday.date.checked_add_days(Days::new(1)),
        _ => None,
    };
    Holiday {
        observed,
        ..holiday
    }
}

/// Return the longer of two waits, the second a second unless given.
#[bimetal::export]
#[must_use]
pub fn longer(first: Duration, #[bimetal(default = 1_000_000_000)] second: Duration) -> Duration {
    first.max(second)
}

/// Add up the waits that are given.
#[bimetal::export]
#[must_use]
pub fn total(waits: Vec<Option<Duration>>) -> Duration {
    waits.into_iter().flatten().sum()
}

/// Return the corners of a rectangle of `size` at the origin, counterclockwise.
#[bimetal::export]
#[must_use]
pub fn corners(size: [f64; 2]) -> [Point; 4] {
    let [width, height] = size;
    [
        Point { x: 0.0, y: 0.0 },
        Point { x: width, y: 0.0 },
        Point {
            x: width,
            y: height,
        },
        Point { x: 0.0, y: height },
    ]
}

/// A drawing with no strokes.
#[bimetal::export]
pub const NO_STROKES: [Stroke; 0] = [];

/// How long a drawing waits between strokes.
#[bimetal::export]
pub const PAUSE: Duration = Duration::from_millis(1500);

/// Return strokes in the opposite order.
#[bimetal::export]
#[must_use]
pub fn retrace(mut strokes: Vec<Stroke>) -> Vec<Stroke> {
    strokes.reverse();
    strokes
}

/// End a drawing with `last`, which lifts the pen unless the caller says
/// otherwise.
#[bimetal::export]
#[must_use]
pub fn finish(mut strokes: Vec<Stroke>, #[bimetal(default = "lift")] last: Stroke) -> Vec<Stroke> {
    strokes.push(last);
    strokes
}

/// Parse a priority.
#[bimetal::export]
pub fn parse_priority(text: &str) -> Result<Priority, ParseError> {
    match text.trim() {
        "" => Err(ParseError::Empty),
        "low" => Ok(Priority::Low),
        "normal" => Ok(Priority::Normal),
        "high" => Ok(Priority::High),
        other => Err(ParseError::Unknown {
            text: other.to_owned(),
        }),
    }
}

/// The smallest and largest value, or nothing for an empty list.
#[bimetal::export]
#[must_use]
pub fn bounds(values: Vec<i32>) -> Option<(i32, i32)> {
    let minimum = *values.iter().min()?;
    let maximum = *values.iter().max()?;
    Some((minimum, maximum))
}

/// Add up totals by name.
#[bimetal::export]
#[must_use]
pub fn totals(entries: Vec<(String, u32)>) -> HashMap<String, u32> {
    let mut totals = HashMap::new();
    for (name, amount) in entries {
        *totals.entry(name).or_insert(0) += amount;
    }
    totals
}

/// Wrap a JSON value in an envelope.
#[bimetal::export]
#[must_use]
pub fn wrap(value: Value) -> Value {
    serde_json::json!({ "wrapped": value })
}

/// Do nothing and return nothing.
#[bimetal::export]
pub fn ping() {}

/// Reverse bytes. Large payloads use the `bytes` boundary instead of a list.
///
/// A parameter that shares its name with a Python builtin, such as `bytes`,
/// gains a trailing underscore in Python: `bytes_`.
#[bimetal::export]
#[bimetal(returns(bytes))]
#[must_use]
pub fn reverse(#[bimetal(bytes)] bytes: &[u8]) -> Vec<u8> {
    bytes.iter().rev().copied().collect()
}

/// Repeat a text. Its Python parameters are `str_` and `int_`.
#[bimetal::export]
#[must_use]
pub fn repeat(str: &str, int: u32) -> String {
    str.repeat(int.try_into().unwrap_or(usize::MAX))
}

/// A 4-byte fingerprint of some data.
#[bimetal::export]
#[bimetal(returns(bytes))]
#[must_use]
pub fn fingerprint(#[bimetal(bytes)] data: &[u8]) -> [u8; 4] {
    let mut digest = [0_u8; 4];
    for (index, byte) in data.iter().enumerate() {
        digest[index % 4] ^= byte;
    }
    digest
}

/// A running total. Buffers become NumPy arrays and typed arrays.
#[bimetal::export]
#[bimetal(returns(buffer))]
#[must_use]
pub fn running_total(#[bimetal(buffer)] values: &[i64]) -> Vec<i64> {
    values
        .iter()
        .scan(0, |total, value| {
            *total += value;
            Some(*total)
        })
        .collect()
}

/// Scale samples by a factor.
#[bimetal::export]
#[bimetal(returns(buffer))]
#[must_use]
pub fn scale(#[bimetal(buffer)] samples: &[f32], factor: f32) -> Vec<f32> {
    samples.iter().map(|sample| sample * factor).collect()
}

/// The length of a route. Uses an error declared in the `geometry` namespace.
#[bimetal::export]
pub fn route_length(route: Vec<Point>) -> Result<f64, GeometryError> {
    if route.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(GeometryError::SamePoint);
    }
    Ok(route
        .windows(2)
        .map(|pair| showcase_geometry::distance(pair[0], pair[1]))
        .sum())
}

/// The distance of a point from `origin`, or from `(0, 0)` without one.
#[bimetal::export]
#[must_use]
pub fn distance_from(point: Point, origin: Option<Point>) -> f64 {
    showcase_geometry::distance(origin.unwrap_or(Point { x: 0.0, y: 0.0 }), point)
}

/// Join a package and a class name, as in `geometry.Point`.
///
/// JavaScript reserves `package` and `class`, so TypeScript names these
/// parameters `package_` and `class_`, and Python names the second one
/// `class_value`.
#[bimetal::export]
#[must_use]
pub fn qualify(package: &str, class: &str) -> String {
    format!("{package}.{class}")
}

/// The name of this example. An export may have the name of its own package.
#[bimetal::export]
#[must_use]
pub fn showcase() -> String {
    "showcase".to_owned()
}

/// The item at a position.
///
/// # Panics
///
/// Panics when `index` is out of range. Python raises the panic as an
/// exception, and TypeScript throws an `Error` with the panic's message. The
/// TypeScript package's WebAssembly module is unusable after a panic.
#[bimetal::export]
#[must_use]
pub fn item_at(items: Vec<String>, index: u32) -> String {
    let count = items.len();
    usize::try_from(index)
        .ok()
        .and_then(|position| items.into_iter().nth(position))
        .unwrap_or_else(|| panic!("index {index} is out of range for {count} items"))
}

/// One change to a ledger balance.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Model)]
pub struct Entry {
    pub amount: i64,
    pub balance: i64,
}

/// A summary of a ledger, which only the `statements` feature declares.
#[cfg(feature = "statements")]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Model)]
pub struct Statement {
    pub owner: String,
    pub balance: i64,
    pub entries: u32,
}

/// A balance kept in Rust between host calls.
pub struct Ledger {
    owner: String,
    entries: Vec<Entry>,
    locked: bool,
}

#[bimetal::export]
impl Ledger {
    /// Open an empty ledger.
    #[bimetal(constructor)]
    pub fn new(owner: String) -> Result<Self, ParseError> {
        if owner.trim().is_empty() {
            return Err(ParseError::Empty);
        }
        Ok(Self {
            owner,
            entries: Vec::new(),
            locked: false,
        })
    }

    /// Open a ledger with a starting balance.
    #[bimetal(constructor)]
    #[must_use]
    pub fn with_balance(owner: String, balance: i64) -> Self {
        Self {
            owner,
            entries: vec![Entry {
                amount: balance,
                balance,
            }],
            locked: false,
        }
    }

    /// Add an amount and return the new balance.
    pub fn deposit(&mut self, amount: i64) -> Result<i64, LockedError> {
        if self.locked {
            return Err(LockedError);
        }
        let balance = self.balance() + amount;
        self.entries.push(Entry { amount, balance });
        Ok(balance)
    }

    /// Refuse further deposits.
    pub fn lock(&mut self) {
        self.locked = true;
    }

    /// The current balance.
    #[must_use]
    pub fn balance(&self) -> i64 {
        self.entries.last().map_or(0, |entry| entry.balance)
    }

    /// The owner's name.
    #[must_use]
    pub fn owner(&self) -> &str {
        &self.owner
    }

    /// Every change so far.
    #[must_use]
    pub fn history(&self) -> &[Entry] {
        &self.entries
    }

    /// Sum up the ledger, with the `statements` feature.
    #[cfg(feature = "statements")]
    #[must_use]
    pub fn statement(&self) -> Statement {
        Statement {
            owner: self.owner.clone(),
            balance: self.balance(),
            entries: u32::try_from(self.entries.len()).unwrap_or(u32::MAX),
        }
    }

    /// Count the entries in an async call that does not wait, with the
    /// `statements` feature. Such a call finishes on the caller's thread.
    #[cfg(feature = "statements")]
    pub async fn count_entries_at_once(&self) -> u32 {
        u32::try_from(self.entries.len()).unwrap_or(u32::MAX)
    }

    /// Name the owner with `label` in an async call that does not wait, with
    /// the `statements` feature, or return why `label` failed. A call that
    /// takes a host function starts on a worker on the native hosts all the
    /// same.
    #[cfg(feature = "statements")]
    pub async fn label_owner_at_once(&self, label: bimetal::Callback<(String,), String>) -> String {
        label
            .call((self.owner.clone(),))
            .unwrap_or_else(|error| error.to_string())
    }

    /// Clear the history. Rust code can call this; hosts cannot.
    #[bimetal(skip)]
    pub fn clear(&mut self) {
        self.entries.clear();
    }
}

/// Yield to the executor `times` times. It stands in for real async work, such
/// as a request, while keeping this example free of any one runtime, so the
/// same code runs on Tokio in Python and on the JavaScript event loop.
pub(crate) async fn pause(times: u32) {
    for _ in 0..times {
        let mut yielded = false;
        future::poll_fn(|context| {
            if yielded {
                Poll::Ready(())
            } else {
                yielded = true;
                context.waker().wake_by_ref();
                Poll::Pending
            }
        })
        .await;
    }
}

/// Count the words of a text asynchronously.
#[bimetal::export]
pub async fn count_words(text: String) -> u32 {
    pause(1).await;
    text.split_whitespace()
        .count()
        .try_into()
        .unwrap_or(u32::MAX)
}

/// Count the words of a text in an async call that does not wait, as a read
/// from memory does. Such a call finishes on the caller's thread.
#[bimetal::export]
pub async fn count_words_at_once(text: String) -> u32 {
    text.split_whitespace()
        .count()
        .try_into()
        .unwrap_or(u32::MAX)
}

/// Parse a priority in an async call that does not wait, as
/// [`parse_priority`] does.
#[bimetal::export]
pub async fn parse_priority_at_once(text: String) -> Result<Priority, ParseError> {
    parse_priority(&text)
}

/// The item at a position, found in an async call that does not wait.
///
/// # Panics
///
/// Panics when `index` is out of range, as [`item_at`] does.
#[bimetal::export]
pub async fn find_item_at_once(items: Vec<String>, index: u32) -> String {
    item_at(items, index)
}

/// The item at a position, found asynchronously.
///
/// # Panics
///
/// Panics when `index` is out of range, as [`item_at`] does. Python raises
/// the panic from `await`, and TypeScript rejects the promise.
#[bimetal::export]
pub async fn find_item(items: Vec<String>, index: u32) -> String {
    pause(1).await;
    item_at(items, index)
}

/// Count `depth` nested calls.
///
/// A depth of a million overflows the stack. In TypeScript, that is a trap
/// without a panic, which aborts the WebAssembly module as a panic does.
#[bimetal::export]
#[must_use]
pub fn nest(depth: u32) -> u32 {
    if depth == 0 {
        return 0;
    }
    // The opaque result keeps the recursion from becoming a loop.
    std::hint::black_box(nest(depth - 1)) + 1
}

/// Count `depth` nested calls asynchronously, as [`nest`] does.
#[bimetal::export]
pub async fn nest_later(depth: u32) -> u32 {
    pause(1).await;
    nest(depth)
}

/// Messages for one recipient, which async calls deliver and search.
pub struct Mailbox {
    recipient: String,
    messages: Vec<String>,
}

#[bimetal::export]
impl Mailbox {
    /// Open a mailbox. Opening is async, so the class has no constructor.
    #[bimetal(constructor)]
    pub async fn open(recipient: String) -> Result<Self, ParseError> {
        pause(1).await;
        if recipient.trim().is_empty() {
            return Err(ParseError::Empty);
        }
        Ok(Self {
            recipient,
            messages: Vec::new(),
        })
    }

    /// File a message, which takes a few steps, and return how many are filed.
    /// Other calls wait until it finishes.
    pub async fn deliver(&mut self, message: String) -> u32 {
        pause(3).await;
        self.messages.push(message);
        self.count()
    }

    /// The messages that contain a word. Searches can run together.
    pub async fn search(&self, word: String) -> Vec<String> {
        pause(1).await;
        self.messages
            .iter()
            .filter(|message| message.contains(&word))
            .cloned()
            .collect()
    }

    /// The recipient's name.
    #[must_use]
    pub fn recipient(&self) -> &str {
        &self.recipient
    }

    /// How many messages are filed.
    #[must_use]
    pub fn count(&self) -> u32 {
        self.messages.len().try_into().unwrap_or(u32::MAX)
    }
}
