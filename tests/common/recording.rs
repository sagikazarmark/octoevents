//! A `tracing` layer that records every span and event as typed values.
//!
//! The crate's tracing contract is what it records: which spans, at which
//! level, with which fields, and each field as a string, an integer, a
//! boolean, a `Debug` rendering or an error. How a subscriber renders those
//! is that subscriber's business, so a test reads the values through this
//! layer rather than parsing what `tracing_subscriber::fmt` prints.

use std::collections::HashMap;
use std::error::Error;
use std::fmt;
use std::sync::{Arc, Mutex};

use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber, span};
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::layer::{Context, Layer, SubscriberExt as _};

/// One recorded field value, typed as the crate recorded it.
///
/// A string and an integer that read alike are different values here, as
/// they are to a dashboard: `Str("42")` and `U64(42)` never compare equal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    /// Recorded as a string.
    Str(String),
    /// Recorded as an unsigned integer.
    U64(u64),
    /// Recorded as a signed integer.
    I64(i64),
    /// Recorded as a boolean.
    Bool(bool),
    /// Recorded through `Debug`: what `tracing::field::display`,
    /// `tracing::field::debug` and an event's `message` arrive as.
    Debug(String),
    /// Recorded as an error value: its text and the chain of sources
    /// beneath it.
    Error(ErrorValue),
}

/// An error as a field value: what a subscriber is handed when a field is a
/// `dyn Error`, with the chain it would walk already walked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorValue {
    /// The error's `Display`.
    pub text: String,
    /// The `Display` of each error beneath it, following `source()` down to
    /// the last.
    pub sources: Vec<String>,
}

impl fmt::Display for Value {
    /// Every text the value carries, unescaped, so a check over the text of a
    /// recording misses nothing: a string or `Debug` rendering as it came, a
    /// number or boolean bare, an error as its text and each source beneath.
    /// The `Debug` form is the one that shows the type.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Str(text) | Self::Debug(text) => f.write_str(text),
            Self::U64(number) => write!(f, "{number}"),
            Self::I64(number) => write!(f, "{number}"),
            Self::Bool(flag) => write!(f, "{flag}"),
            Self::Error(error) => {
                f.write_str(&error.text)?;
                for source in &error.sources {
                    write!(f, ": {source}")?;
                }
                Ok(())
            }
        }
    }
}

/// The fields one span or event carries, by name, in recording order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Fields(Vec<(&'static str, Value)>);

impl Fields {
    /// The value of `name`, or `None` when it was never recorded.
    pub fn get(&self, name: &str) -> Option<&Value> {
        self.0
            .iter()
            .find_map(|(field, value)| (*field == name).then_some(value))
    }

    /// The text of the string field `name`, or `None` when it was never
    /// recorded.
    ///
    /// Panics when the field was recorded as anything but a string: a value
    /// of the wrong type is a failure, not an absence.
    #[track_caller]
    pub fn str(&self, name: &str) -> Option<&str> {
        match self.get(name)? {
            Value::Str(text) => Some(text),
            other => panic!("field {name} is {other:?}, not a string"),
        }
    }

    /// The text of the field `name` recorded through `Debug`, or `None` when
    /// it was never recorded.
    ///
    /// Panics when the field was recorded as anything else.
    #[track_caller]
    pub fn debug(&self, name: &str) -> Option<&str> {
        match self.get(name)? {
            Value::Debug(text) => Some(text),
            other => panic!("field {name} is {other:?}, not a Debug rendering"),
        }
    }

    /// The error value of the field `name`, or `None` when it was never
    /// recorded.
    ///
    /// Panics when the field was recorded as anything but an error.
    #[track_caller]
    pub fn error(&self, name: &str) -> Option<&ErrorValue> {
        match self.get(name)? {
            Value::Error(error) => Some(error),
            other => panic!("field {name} is {other:?}, not an error value"),
        }
    }

    /// Every field with its value.
    pub fn iter(&self) -> impl Iterator<Item = (&'static str, &Value)> {
        self.0.iter().map(|(name, value)| (*name, value))
    }

    /// Records `value` under the field's name, replacing an earlier value of
    /// the same field: a span records a field declared `Empty` once, later.
    fn record(&mut self, field: &Field, value: Value) {
        let name = field.name();
        match self.0.iter_mut().find(|(field, _)| *field == name) {
            Some(slot) => slot.1 = value,
            None => self.0.push((name, value)),
        }
    }
}

impl Visit for Fields {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.record(field, Value::Str(value.to_owned()));
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.record(field, Value::U64(value));
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.record(field, Value::I64(value));
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.record(field, Value::Bool(value));
    }

    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        self.record(field, Value::Debug(format!("{value:?}")));
    }

    fn record_error(&mut self, field: &Field, value: &(dyn Error + 'static)) {
        let mut sources = Vec::new();
        let mut next = value.source();
        while let Some(source) = next {
            sources.push(source.to_string());
            next = source.source();
        }
        self.record(
            field,
            Value::Error(ErrorValue {
                text: value.to_string(),
                sources,
            }),
        );
    }
}

impl fmt::Display for Fields {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("{")?;
        for (index, (name, value)) in self.0.iter().enumerate() {
            if index > 0 {
                f.write_str(" ")?;
            }
            write!(f, "{name}={value}")?;
        }
        f.write_str("}")
    }
}

/// One span the layer saw open and close.
#[derive(Debug, Clone)]
pub struct SpanRecord {
    pub name: &'static str,
    pub target: &'static str,
    pub level: Level,
    /// The fields the span was created with.
    pub at_open: Fields,
    /// The fields it carried when it closed: those it opened with and every
    /// one recorded since. Every span has closed by the time [`traced`]
    /// returns, since the call it ran in has.
    pub at_close: Fields,
}

/// One event the layer saw.
#[derive(Debug, Clone)]
pub struct EventRecord {
    pub target: &'static str,
    pub level: Level,
    /// The event's fields, its `message` among them.
    pub fields: Fields,
}

/// Everything one call recorded: spans in opening order, events in emission
/// order.
#[derive(Debug, Clone, Default)]
pub struct Recording {
    pub spans: Vec<SpanRecord>,
    pub events: Vec<EventRecord>,
}

impl Recording {
    /// The one span named `name`.
    ///
    /// Panics, showing the whole recording, when there is none or more than
    /// one: the tests run one delivery per recording.
    #[track_caller]
    pub fn span(&self, name: &str) -> &SpanRecord {
        let mut matching = self.spans.iter().filter(|span| span.name == name);
        let span = matching
            .next()
            .unwrap_or_else(|| panic!("no {name} span recorded:\n{self}"));
        assert!(
            matching.next().is_none(),
            "more than one {name} span recorded:\n{self}"
        );
        span
    }

    /// Whether any span named `name` was recorded.
    pub fn has_span(&self, name: &str) -> bool {
        self.spans.iter().any(|span| span.name == name)
    }

    /// The events recorded at exactly `level`, in emission order.
    pub fn events_at(&self, level: Level) -> Vec<&EventRecord> {
        self.events
            .iter()
            .filter(|event| event.level == level)
            .collect()
    }

    /// The one event recorded at exactly `level`.
    ///
    /// Panics, showing the whole recording, when there is none or more than
    /// one: a failed delivery is one event at ERROR, and a second one is a
    /// failure of that promise.
    #[track_caller]
    pub fn event_at(&self, level: Level) -> &EventRecord {
        let events = self.events_at(level);
        assert_eq!(
            events.len(),
            1,
            "one event at {level} expected, {} recorded:\n{self}",
            events.len()
        );
        events[0]
    }

    /// Every field any span or event recorded, with its name: what a hygiene
    /// check walks. A span's fields are listed as it opened and as it closed;
    /// a field recorded more than once after opening keeps its last value
    /// only.
    pub fn fields(&self) -> impl Iterator<Item = (&'static str, &Value)> {
        let spans = self
            .spans
            .iter()
            .flat_map(|span| span.at_open.iter().chain(span.at_close.iter()));
        let events = self.events.iter().flat_map(|event| event.fields.iter());
        spans.chain(events)
    }
}

impl fmt::Display for Recording {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for span in &self.spans {
            writeln!(
                f,
                "span {} {} ({}): opened {} closed {}",
                span.level, span.name, span.target, span.at_open, span.at_close
            )?;
        }
        for event in &self.events {
            writeln!(
                f,
                "event {} {}: {}",
                event.level, event.target, event.fields
            )?;
        }
        Ok(())
    }
}

#[derive(Default)]
struct LayerState {
    recording: Recording,
    /// The index into `recording.spans` of each span still open, by ID. IDs
    /// are the registry's to reuse once a span closes, so a closed span is
    /// dropped from here.
    open: HashMap<span::Id, usize>,
}

/// The layer: a handle on shared state, cloned into the subscriber.
#[derive(Clone, Default)]
struct RecordingLayer(Arc<Mutex<LayerState>>);

impl<S: Subscriber> Layer<S> for RecordingLayer {
    fn on_new_span(&self, attrs: &span::Attributes<'_>, id: &span::Id, _: Context<'_, S>) {
        let mut fields = Fields::default();
        attrs.record(&mut fields);
        let metadata = attrs.metadata();
        let mut state = self.0.lock().unwrap();
        let index = state.recording.spans.len();
        state.recording.spans.push(SpanRecord {
            name: metadata.name(),
            target: metadata.target(),
            level: *metadata.level(),
            at_open: fields.clone(),
            at_close: fields,
        });
        state.open.insert(id.clone(), index);
    }

    fn on_record(&self, id: &span::Id, values: &span::Record<'_>, _: Context<'_, S>) {
        let mut state = self.0.lock().unwrap();
        let index = state.open[id];
        values.record(&mut state.recording.spans[index].at_close);
    }

    fn on_event(&self, event: &Event<'_>, _: Context<'_, S>) {
        let mut fields = Fields::default();
        event.record(&mut fields);
        let metadata = event.metadata();
        self.0.lock().unwrap().recording.events.push(EventRecord {
            target: metadata.target(),
            level: *metadata.level(),
            fields,
        });
    }

    fn on_close(&self, id: span::Id, _: Context<'_, S>) {
        self.0.lock().unwrap().open.remove(&id);
    }
}

/// Runs `call` under a fresh recording subscriber and returns everything it
/// recorded alongside what the call returned.
///
/// Everything down to TRACE is recorded; [`traced_at`] is the same
/// subscriber with a ceiling.
pub fn traced<F, T>(call: F) -> (Recording, T)
where
    F: Future<Output = T>,
{
    traced_at(Level::TRACE, call)
}

/// [`traced`] with the subscriber's maximum level set to `level`, so a test
/// can see what an operator filtering at that level would see.
pub fn traced_at<F, T>(level: Level, call: F) -> (Recording, T)
where
    F: Future<Output = T>,
{
    traced_with_filter(LevelFilter::from_level(level), call)
}

/// [`traced`] with a subscriber filter, for selectively disabling an operation.
pub fn traced_with_filter<F, T>(
    filter: impl Layer<tracing_subscriber::Registry> + Send + Sync + 'static,
    call: F,
) -> (Recording, T)
where
    F: Future<Output = T>,
{
    let layer = RecordingLayer::default();
    let subscriber = tracing_subscriber::registry()
        .with(filter)
        .with(layer.clone());

    // A current-thread runtime keeps the whole call on the thread that holds
    // the subscriber default, which `with_default` does not carry across awaits.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let returned = tracing::subscriber::with_default(subscriber, || runtime.block_on(call));
    let recording = std::mem::take(&mut layer.0.lock().unwrap().recording);
    (recording, returned)
}
