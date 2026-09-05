use crate::{Action, EventKind};

/// The kinds and actions one dispatcher registration selects.
///
/// A matcher expands to a list of slots, each an event kind with an optional
/// action. Build one from any of the shapes below; every `Dispatcher::on`
/// call accepts `impl Into<EventMatcher>`, so a matcher is rarely named:
///
/// ```
/// use octoevents::{Action, EventKind, EventMatcher};
///
/// // One kind, every action.
/// let _ = EventMatcher::from(EventKind::PullRequest);
/// // Several kinds.
/// let _ = EventMatcher::from([EventKind::Issues, EventKind::IssueComment]);
/// // One kind and one action.
/// let _ = EventMatcher::from((EventKind::PullRequest, Action::Opened));
/// // One kind and several actions.
/// let _ = EventMatcher::from((
///     EventKind::PullRequest,
///     [Action::Opened, Action::Synchronize, Action::Reopened],
/// ));
/// // Heterogeneous kind/action pairs.
/// let _ = EventMatcher::from([
///     (EventKind::PullRequest, Action::Opened),
///     (EventKind::Issues, Action::Closed),
/// ]);
/// // Any mix, combined.
/// let _ = EventMatcher::from(EventKind::Push).or((EventKind::Release, Action::Published));
/// ```
///
/// There is deliberately no `|` operator: operator dispatch is on the left
/// operand's type, so `(kind, action) | (kind, action)` could never work, and
/// an operator that works depending on operand order is worse than none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventMatcher {
    slots: Vec<Slot>,
}

impl EventMatcher {
    /// Extends this matcher with the slots of another.
    #[must_use]
    pub fn or(mut self, other: impl Into<Self>) -> Self {
        self.slots.extend(other.into().slots);
        self
    }

    #[cfg_attr(
        not(feature = "octocrab"),
        allow(
            dead_code,
            reason = "consumed by `Dispatcher::on`, the one registration that takes a matcher \
                      and that the `octocrab` feature gates; the matcher itself stays core"
        )
    )]
    pub(crate) fn into_slots(self) -> Vec<Slot> {
        self.slots
    }
}

/// One kind, optionally narrowed to one action: the unit a matcher expands to
/// and the dispatcher registers a route under.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Slot {
    pub(crate) kind: EventKind,
    pub(crate) action: Option<Action>,
}

impl Slot {
    /// A slot for every action of `kind`.
    pub(crate) fn any_action(kind: EventKind) -> Self {
        Self { kind, action: None }
    }

    /// A slot for one action of `kind`.
    pub(crate) fn action(kind: EventKind, action: Action) -> Self {
        Self {
            kind,
            action: Some(action),
        }
    }
}

impl From<EventKind> for EventMatcher {
    fn from(kind: EventKind) -> Self {
        Self {
            slots: vec![Slot::any_action(kind)],
        }
    }
}

impl<const N: usize> From<[EventKind; N]> for EventMatcher {
    fn from(kinds: [EventKind; N]) -> Self {
        Self {
            slots: kinds.into_iter().map(Slot::any_action).collect(),
        }
    }
}

impl From<(EventKind, Action)> for EventMatcher {
    fn from((kind, action): (EventKind, Action)) -> Self {
        Self {
            slots: vec![Slot::action(kind, action)],
        }
    }
}

impl<const N: usize> From<(EventKind, [Action; N])> for EventMatcher {
    fn from((kind, actions): (EventKind, [Action; N])) -> Self {
        Self {
            slots: actions
                .into_iter()
                .map(|action| Slot::action(kind.clone(), action))
                .collect(),
        }
    }
}

impl<const N: usize> From<[(EventKind, Action); N]> for EventMatcher {
    fn from(pairs: [(EventKind, Action); N]) -> Self {
        Self {
            slots: pairs
                .into_iter()
                .map(|(kind, action)| Slot::action(kind, action))
                .collect(),
        }
    }
}
