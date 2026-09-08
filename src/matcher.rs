use crate::{Action, EventKind, Payload};

/// The kinds and actions one dispatcher registration selects.
///
/// A matcher expands to a list of slots, each an event kind with an optional
/// action. `Dispatcher::on` accepts any [`IntoMatcher`], so a matcher is
/// rarely named; the shapes below build one and say their kinds outright.
/// A handler over a [`Payload`] may instead give actions alone, an
/// [`Action`], an array of them or [`AnyAction`], and take the kind from
/// its payload type; those are `IntoMatcher` impls, not `From` impls, since
/// they need the input type to know the kind.
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

    pub(crate) fn into_slots(self) -> Vec<Slot> {
        self.slots
    }
}

/// What `Dispatcher::on` accepts as the matcher for a handler over `I`.
///
/// Two families implement it. Every shape that converts into an
/// [`EventMatcher`] says its kinds and works for any input: a kind, several
/// kinds, a kind with one action or several, kind/action pairs, or an
/// `EventMatcher` built with [`or`](EventMatcher::or). The other family says
/// actions alone and takes the kind from the input, so it is implemented
/// only where `I` is a [`Payload`]: one [`Action`], an array of them, or
/// [`AnyAction`] for every action of the declared kind. With those the kind
/// is said once, on the payload type, and the handler cannot be registered
/// under another.
///
#[cfg_attr(feature = "derive", doc = "```")]
#[cfg_attr(not(feature = "derive"), doc = "```ignore")]
/// use octoevents::{Action, AnyAction, Dispatcher, EventKind, EventMeta, Payload};
/// # use octoevents::DecodeError;
/// # struct AppError;
/// # impl From<DecodeError> for AppError { fn from(_: DecodeError) -> Self { Self } }
///
/// #[derive(serde::Deserialize, Payload)]
/// #[payload(EventKind::Issues)]
/// struct IssueOpened { issue: Issue }
/// #[derive(serde::Deserialize)]
/// struct Issue { number: u64 }
///
/// async fn label(issue: IssueOpened) -> Result<(), AppError> { Ok(()) }
/// async fn notify(issue: IssueOpened) -> Result<(), AppError> { Ok(()) }
/// async fn revoke(meta: EventMeta) -> Result<(), AppError> { Ok(()) }
///
/// let dispatcher = Dispatcher::<AppError>::builder()
///     .on(Action::Opened, label)                              // `issues`, from `IssueOpened`
///     .on(AnyAction, notify)                                  // every `issues` action
///     .on((EventKind::Installation, Action::Deleted), revoke) // `EventMeta` declares no kind
///     .build();
/// # let _ = dispatcher;
/// ```
///
/// Actions alone under an input that declares no kind are refused at
/// compile time, as "`EventMeta` is not a payload", whose first note says to
/// register such a handler with a matcher that says the kind:
///
/// ```compile_fail,E0277
/// use octoevents::{Action, Dispatcher, EventMeta};
/// # use octoevents::DecodeError;
/// # struct AppError;
/// # impl From<DecodeError> for AppError { fn from(_: DecodeError) -> Self { Self } }
///
/// async fn revoke(meta: EventMeta) -> Result<(), AppError> { Ok(()) }
///
/// let dispatcher = Dispatcher::<AppError>::builder()
///     .on(Action::Deleted, revoke)
///     .build();
/// ```
///
/// `AnyAction` is refused the same way; a handler over the
/// [`Envelope`](crate::Envelope) for
/// every action of one kind names the kind, `on(EventKind::Push, forward)`:
///
/// ```compile_fail,E0277
/// use octoevents::{AnyAction, Dispatcher, Envelope};
/// # use octoevents::DecodeError;
/// # struct AppError;
/// # impl From<DecodeError> for AppError { fn from(_: DecodeError) -> Self { Self } }
///
/// async fn forward(envelope: Envelope) -> Result<(), AppError> { Ok(()) }
///
/// let dispatcher = Dispatcher::<AppError>::builder()
///     .on(AnyAction, forward)
///     .build();
/// ```
///
/// The trait is open. A consumer's own matcher implements it for any input
/// through `From<_> for EventMatcher`, which the blanket picks up, or for
/// payload inputs alone by implementing `IntoMatcher<I>` under `I: Payload`
/// and reading `I::KIND`, as the shipped relative shapes do:
///
/// ```
/// use octoevents::{Action, EventMatcher, IntoMatcher, Payload};
///
/// /// The two actions that open an issue or a pull request, for any payload.
/// struct Opening;
///
/// impl<I: Payload> IntoMatcher<I> for Opening {
///     fn into_matcher(self) -> EventMatcher {
///         (I::KIND, [Action::Opened, Action::Reopened]).into()
///     }
/// }
/// ```
// `{I}` is not named in the text: rustc checks this bound before it has
// inferred the input from the handler, so it would render as `_`. Actions
// alone under an input that is no `Payload` never reach this message; rustc
// descends into the relative impls' `I: Payload` bound and reports the
// `Payload` trait's own, whose first note says to spell the kind.
#[diagnostic::on_unimplemented(
    message = "`{Self}` is not a matcher",
    label = "expected a kind, a `(kind, action)`, a `(kind, [actions])`, `[(kind, action)]` pairs or an `EventMatcher`, or, for a handler over a `Payload`, an `Action`, `[Action; N]` or `AnyAction`",
    note = "there is no string form: `issues.opened` is `(EventKind::Issues, Action::Opened)`, or `Action::Opened` alone for a handler over an `issues` payload type"
)]
pub trait IntoMatcher<I> {
    /// The matcher, with every kind filled in.
    fn into_matcher(self) -> EventMatcher;
}

// Every shape that says its kinds is a matcher for any input. The blanket is
// disjoint from the `Payload`-only impls below because `Action`, `[Action; N]`
// and `AnyAction` do not convert into an `EventMatcher`, which they cannot
// without knowing the kind; `do_not_recommend` keeps rustc from explaining a
// failed relative matcher as "not `Into<EventMatcher>`".
#[diagnostic::do_not_recommend]
impl<I, M: Into<EventMatcher>> IntoMatcher<I> for M {
    fn into_matcher(self) -> EventMatcher {
        self.into()
    }
}

/// Every action of the kind a handler's payload type declares, as the
/// matcher of a `Dispatcher::on` registration.
///
/// `on(AnyAction, notify)` registers `notify` for every action of
/// `P::KIND`, where `P` is the [`Payload`] `notify` receives, directly or as
/// `Event<P>`. It is the whole-kind counterpart of one [`Action`] or an
/// array of them, and like them it is accepted only for a handler over a
/// payload, since only a payload declares a kind to take.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct AnyAction;

impl<I: Payload> IntoMatcher<I> for AnyAction {
    fn into_matcher(self) -> EventMatcher {
        EventMatcher::from(I::KIND)
    }
}

impl<I: Payload> IntoMatcher<I> for Action {
    fn into_matcher(self) -> EventMatcher {
        EventMatcher::from((I::KIND, self))
    }
}

impl<I: Payload, const N: usize> IntoMatcher<I> for [Action; N] {
    fn into_matcher(self) -> EventMatcher {
        EventMatcher::from((I::KIND, self))
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
