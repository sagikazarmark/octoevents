# Webhook vocabulary across specs, providers, and libraries

**Question.** What do specs, GitHub itself, and webhook-receiving libraries
call the things `octoevents` currently names `Envelope` (meta beside signed
bytes), `EventMeta` (routing metadata), `Payload` (one kind's decoded body),
`Event<P>` (meta beside decoded body), and `Probe` (best-effort partial
parse of the body)? Is there a standard, and where does the ecosystem agree
or split?

**Date.** 2026-09-07.

**Method.** Primary sources only: spec texts at tagged versions where they
exist, official provider docs, source files, and docs.rs pages, as fetched on
the date above. Type, field, and function names are quoted exactly from
source; prose is quoted where the wording matters. Where a source has no word
for a concept the section says so. Vocabulary only; mechanics (routing, error
handling, verification behaviour) are in the companion. No names are
recommended here.

**Companion.** [webhook-libraries.md](./webhook-libraries.md) surveys the
same libraries for mechanics (API surface, routing tiers, error handling).

---

## Concepts surveyed

Each section records the source's word, if any, for:

- **(a)** the raw received unit: headers plus signed body bytes, before decoding
- **(b)** the routing/context metadata: delivery id, event name, etc.
- **(c)** the JSON body as bytes or string
- **(d)** the decoded, typed body for one kind (an issues struct)
- **(e)** the pairing of metadata with the decoded body
- **(f)** a partial, best-effort extraction of fields from the body
- **(g)** the sub-classification within a kind (GitHub's `action`)
- **(h)** the unparsed wire name versus the parsed kind
- **(i)** the word "envelope", and what it wraps

---

## 1. CloudEvents specification v1.0.2

The only source here that defines its vocabulary with RFC 2119 language. The
"Notations and Terminology" section of `spec.md` at tag `v1.0.2` defines
(quoted):

- **Occurrence**: "the capture of a statement of fact during the operation
  of a software system."
- **Event**: "a data record expressing an occurrence and its context. ...
  Events will contain two types of information: the Event Data representing
  the Occurrence and Context metadata providing contextual information about
  the Occurrence."
- **Context**: "Context metadata will be encapsulated in the Context
  Attributes."
- **Data**: "Domain-specific information about the occurrence (i.e. the
  payload)."
- **Event Format**: "specifies how to serialize a CloudEvent as a sequence
  of bytes."
- **Message**: "Events are transported from a source to a destination via
  messages. A 'structured-mode message' is one where the event is fully
  encoded using a stand-alone event format and stored in the message body. A
  'binary-mode message' is one where the event data is stored in the message
  body, and event attributes are stored as part of message meta-data."
- **Protocol Binding**: "describes how events are sent and received over a
  given protocol. Protocol bindings MAY choose to use an Event Format to map
  an event directly to the transport envelope body, or MAY provide additional
  formatting and structure to the envelope."

Two sentences bear directly on (b) versus (f). On context attributes: "These
attributes, while descriptive of the event, are designed such that they can
be serialized independent of the event data. This allows for them to be
inspected at the destination without having to deserialize the event data."
On extension attributes: "If such identity attributes happen to be part of
the event 'data', the event producer would also add the identity attributes
to the 'context attributes' so that event consumers can easily access this
information without needing to decode and examine the event data." The spec
places the duty of lifting routing fields out of the body on the *producer*
and has no verb for a consumer doing it.

`type` is "a value describing the type of event related to the originating
occurrence. ... The format of this is producer defined"; the spec's own
example is `com.github.pull_request.opened`, folding GitHub's kind and action
into one string. There is no separate action or sub-type attribute; `subject`
is the nearest ("identifies the subject of the event in the context of the
event producer").

"Design Goals" is in the non-normative `primer.md`, not `spec.md`. It says:
"CloudEvents, at its core, defines a set of metadata, called attributes, about
the event being transferred between systems ... This metadata is meant to be
the minimal set of information needed to route the request to the proper
component and to facilitate proper processing of the event by that component.
So, while this might mean that some of the application data of the event
itself might be duplicated as part of the CloudEvent's set of attributes, this
is to be done solely for the purpose of proper delivery, and processing, of
the message." Also: "Events represent facts and therefore do not include a
destination, whereas messages convey intent." The primer's "Roles" section
describes the action-routing problem without naming it: a framework wanting
"to handle reports of 'goal' differently than reports of 'substitution' ...
will need a suitable metadata discriminator that frees it from having to
understand the event details."

**"Envelope".** Appears three times. In `spec.md`: "transport envelope body"
and "the envelope" (Protocol Binding, above), and "an event rendered using
the JSON envelope format" (under `datacontenttype`). In `json-format.md` §3,
titled "Envelope": "Each CloudEvents event can be wholly represented as a JSON
object"; §1 calls it "a JSON container for CloudEvents attributes and an
associated media type." §4.2 "Envelope" is the batch media type. In every use
the envelope is the outermost serialized container of the *whole* event
(attributes and data together), or the transport frame around it.

**HTTP protocol binding** (main branch, 1.0.3-wip wording; the mode
definitions are unchanged from 1.0.2): "In the _binary_ content mode, the
value of the event `data` is placed into the HTTP request, or response, body
as-is, with the `datacontenttype` attribute value declaring its media type in
the HTTP `Content-Type` header; all other event attributes are mapped to HTTP
headers." Headers are `ce-` prefixed (`ce-id`, `ce-type`, `ce-source`). "In
the _structured_ content mode, event metadata attributes and event data are
placed into the HTTP request or response body using an event format." §3.2.3:
"Implementations MAY include the same HTTP headers as defined for the binary
mode. All CloudEvents metadata attributes MUST be mapped into the payload,
even if they are also mapped into HTTP headers." Mode is detected by
`Content-Type`: `application/cloudevents` prefix means structured, otherwise
binary.

**GitHub adapter** (`adapters/github.md`, non-normative): maps every GitHub
event with `id` = "X-GitHub-Delivery HTTP header value", `type` =
`com.github.issue.` + "action" value (and for kinds without an action,
`com.github.push`, or `com.github.create.` + "ref_type" value), `source` =
"repository.url" value, `subject` = e.g. "issue.number" value, `data` =
"Content of HTTP request body". The adapter reads the body to build routing
attributes; it does not name that step.

Per concept: (a) message; (b) context attributes; (c) data ("i.e. the
payload"), a byte sequence in binary mode; (d) none, `data` is opaque; (e)
event; (f) none, rationale only; (g) none, folded into `type`; (h) none,
`type` is a string; (i) transport envelope / JSON Envelope, wrapping the
whole event.

Sources: [spec.md @ v1.0.2](https://github.com/cloudevents/spec/blob/v1.0.2/cloudevents/spec.md),
[spec.md @ main](https://github.com/cloudevents/spec/blob/main/cloudevents/spec.md),
[primer.md @ v1.0.2](https://github.com/cloudevents/spec/blob/v1.0.2/cloudevents/primer.md) (Design Goals, Roles),
[formats/json-format.md @ v1.0.2](https://github.com/cloudevents/spec/blob/v1.0.2/cloudevents/formats/json-format.md) (§3 Envelope),
[bindings/http-protocol-binding.md @ main](https://github.com/cloudevents/spec/blob/main/cloudevents/bindings/http-protocol-binding.md),
[adapters/github.md @ v1.0.2](https://github.com/cloudevents/spec/blob/v1.0.2/cloudevents/adapters/github.md).

---

## 2. `cloudevents-sdk` (Rust, 0.9.0)

The crate root: "The `Event` data structure, to represent CloudEvent (version
1.0 and 0.3)"; "Traits and utilities in `message` to implement Protocol
Bindings".

```rust
// src/event/mod.rs
pub struct Event {
    pub(crate) attributes: Attributes,
    pub(crate) data: Option<Data>,
    pub(crate) extensions: HashMap<String, ExtensionValue>,
}
pub enum Data { Binary(Vec<u8>), String(String), Json(Value) }
pub trait AttributesReader { fn id(&self) -> &str; fn source(&self) -> &UriReference;
    fn specversion(&self) -> SpecVersion; fn ty(&self) -> &str; /* datacontenttype, dataschema, subject, time */ }
```

Doc on `Event`: "Data structure that represents a CloudEvent. It provides
methods to get the attributes through `AttributesReader` and write them
through `AttributesWriter`. It also provides methods to read and write the
event data." `Data` is the "Event data attribute representation"; its
variants are documented "Event has a binary payload", "Event has a non-json
string payload", "Event has a json payload". `ty()` is the accessor for
`type` (Rust keyword). `EventBuilder` / `EventBuilderV10` build events.

The `message` module: "Provides facilities to implement Protocol Bindings."
`Encoding { STRUCTURED, BINARY, UNKNOWN }` "Represents one of the possible
message encodings/modes"; `UNKNOWN` "Represents a non-CloudEvent or a
malformed CloudEvent that cannot be recognized by the SDK."
`BinaryDeserializer`: "Deserializer trait for a Message that can be encoded as
binary mode"; `StructuredDeserializer` likewise for structured;
`MessageDeserializer`: "Deserializer trait for a Message that can be encoded
both in structured mode or binary mode"; `into_event()`: "Convert this Message
to `Event`." `Event` implements `FromRequest` for axum, actix, and poem ("So
that an actix-web handler may take an Event parameter").

Per concept: (a) Message (trait vocabulary only; no struct); (b)
`Attributes` via `AttributesReader`; (c) `Data::Binary` / `Data::String`; (d)
none, `Data::Json(serde_json::Value)`; (e) `Event { attributes, data,
extensions }`; (f) none; (g) none; (h) none, `ty() -> &str`; (i) absent.

Sources: [docs.rs crate root](https://docs.rs/cloudevents-sdk/latest/cloudevents/),
[`Event`](https://docs.rs/cloudevents-sdk/latest/cloudevents/event/struct.Event.html),
[`Data`](https://docs.rs/cloudevents-sdk/latest/cloudevents/event/enum.Data.html),
[`message` module](https://docs.rs/cloudevents-sdk/latest/cloudevents/message/index.html),
[`Encoding`](https://docs.rs/cloudevents-sdk/latest/cloudevents/message/enum.Encoding.html),
[`src/event/mod.rs`](https://github.com/cloudevents/sdk-rust/blob/main/src/event/mod.rs).

---

## 3. Standard Webhooks specification 1.0.0

Self-described as "a set of conventions to be followed by webhook producers
(senders) to provide webhook consumers (receivers) a secure, consistent, and
interoperable interface". It "does not dictate the structure, or impose any
requirements, on the shape, format, and content of the payload" but
recommends one.

**Payload**: "The payload is the core part of every webhook. It is the actual
data being sent as part of the webhook ... The payload should be passed in
the HTTP body." Recommended structure: `type` "a full-stop delimited type
associated with the event. The type indicates the type of the event being
sent (e.g 'user.created' or 'invoice.paid'), indicates the schema of the
payload (passed in data), and it should be grouped hierarchically";
`timestamp` "of when the event occurred (not necessarily the same of when it
was delivered)"; `data` "the actual event data associated with the event. It
can either be passed as part of the data property, or squashed as part of the
top-level object." And: "Additional metadata can be added both as top-level
properties or as part of data, depending on personal preference."

**Webhook metadata** (its heading): "In addition to the payload itself,
webhook implementations often include two important pieces of metadata: the
timestamp of the attempt, and a unique identifier associated with the
webhook." Headers, under "Webhook headers (sending metadata to consumers)":
`webhook-id` "the unique webhook identifier", `webhook-timestamp` "integer
unix timestamp", `webhook-signature` "the signature(s) of this webhook".

**Signed content**: "the message's: ID, timestamp and body are concatenated
(delimited by full-stops) and then signed. The content to be signed is
therefore: `msg_id.timestamp.payload`." Example id
`msg_2KWPBgLlAfxdpx2AI54pPJ85f4W`. "Message" recurs for the unit ("stop
sending it messages", "list failed messages", "message delivery"), as does
"webhook" itself ("processing the same webhook more than once"). **Attempt**:
"The timestamp of the attempt is the timestamp of when the webhook attempt
has been made ... Every time an attempt is retried the timestamp of the
attempt is updated, while the timestamp of the original event remains the
same." **Event types**: "identifiers ... formatted as an hierarchical, and
full-stop delimited, list".

Per concept: (a) webhook / message; (b) "webhook metadata" (`webhook-id`,
`webhook-timestamp`); (c) payload, "body"; (d) none; (e) none, `{type,
timestamp, data}` is one object; (f) none; (g) none, folded into dotted
`type`; (h) none; (i) absent.

Source: [spec/standard-webhooks.md](https://github.com/standard-webhooks/standard-webhooks/blob/main/spec/standard-webhooks.md).

---

## 4. GitHub's own vocabulary

**About webhooks**: "Webhooks let you subscribe to events happening in a
software system and automatically receive a delivery of data to your server
whenever those events occur." "When an event that your webhook is subscribed
to occurs, GitHub will send an HTTP request with data about the event to the
URL that you specified. If your server is set up to listen for webhook
deliveries at that URL, it can take action when it receives one." Also
"configuring your server to take an action in response to a payload
delivery".

**Webhook events and payloads**: "Each webhook event on this page includes a
description of the webhook properties for that event. If the event has
multiple actions, the properties corresponding to each action are included."
Every event section has the headings "### Webhook payload object", "**Action
type:** `assigned`, `closed`, ..." and "#### Webhook payload object
parameters". The same page also calls actions "event types": "To receive the
rerequested and requested_action event types, the app must have at least
write-level access" and "Repository and organization webhooks only receive
payloads for the created and completed event types in repositories."

"Delivery headers": `X-GitHub-Hook-ID` "The unique identifier of the
webhook"; `X-GitHub-Event` "The name of the event that triggered the
delivery"; `X-GitHub-Delivery` "A globally unique identifier (GUID) to
identify the event"; `X-Hub-Signature-256` "the HMAC hex digest of the
request body"; `X-GitHub-Hook-Installation-Target-Type` / `-ID`. "Example
webhook delivery" shows the POST. "Payloads are capped at 25 MB." "Common
payload parameters": `action` (listed **Required**, no description),
`enterprise`, `installation`, `organization`, `repository`, `sender`, "Most
webhook events include these standard parameters."

**Validating webhook deliveries**: "webhook delivery", "webhook payloads",
"the payload contents", "request body"; code samples name the bytes
`payload_body` (Ruby, Python) and `body` (JS: `const body = await
req.text(); webhooks.verify(body, signature)`).

**Does GitHub call the JSON body an "event"?** Not on the webhooks pages:
"event" is the subscription category and the occurrence (`issues`), "payload"
is the body, and the two are kept apart in the page title itself. But the
`X-GitHub-Delivery` GUID is described as identifying "the event", and in
GitHub Actions the body is the event: the `github.event` context is the
payload (`github.event.issue.number`), and the Actions trigger tables head
the event-name column "Webhook event payload".

**Three words for `action`.** Webhooks docs: "action", "Action type:". Same
page, `check_run`/`check_suite` prose: "event types". GitHub Actions: "Some
events have multiple activity types. For these events, you can specify which
activity types will trigger a workflow run. For more information about what
each activity type means, see Webhook events and payloads" (`on: issues:
types: [opened, edited]`).

**REST API delivery object** (GitHub's own model of one delivery). "List
deliveries" returns `Simple webhook delivery`: `id`, `guid`, `delivered_at`,
`redelivery`, `duration`, `status`, `status_code`, `event`, `action`,
`installation_id`, `repository_id`, `throttled_at`. "Get a delivery" adds
`url`, `request: { headers, payload }`, `response: { headers, payload }`.
Redelivery is `POST .../deliveries/{delivery_id}/attempts`. GitHub thus lifts
`event`, `action`, `installation_id`, `repository_id` out of headers and body
and places them beside `request.payload`, without naming the lifting.

Per concept: (a) delivery; (b) delivery headers; REST `guid`, `event`,
`action`, `installation_id`, `repository_id`; (c) payload / request body; (d)
"webhook payload object" (per event); (e) none as a named thing; the REST
delivery object is the nearest; (f) none named; (g) action / "Action type" /
"event types" / "activity type"; (h) "the name of the event"; no parsed form;
(i) absent. "Message" is also absent.

Sources: [About webhooks](https://docs.github.com/en/webhooks/about-webhooks),
[Webhook events and payloads](https://docs.github.com/en/webhooks/webhook-events-and-payloads),
[Validating webhook deliveries](https://docs.github.com/en/webhooks/using-webhooks/validating-webhook-deliveries),
[Events that trigger workflows](https://docs.github.com/en/actions/reference/workflows-and-actions/events-that-trigger-workflows),
[REST: repository webhooks (deliveries)](https://docs.github.com/en/rest/repos/webhooks?apiVersion=2022-11-28#get-a-delivery-for-a-repository-webhook).

---

## 5. `@octokit/webhooks` and `@octokit/webhooks-types` (JavaScript)

`src/types.ts`:

```ts
export type EmitterWebhookEventWithStringPayloadAndSignature = {
  id: string; name: string; payload: string; signature: string; };
interface BaseWebhookEvent<TName extends WebhookEventName> {
  id: string; name: TName; payload: EventPayloadMap[TName]; }
export type EmitterWebhookEvent<TEmitterEvent extends EmitterWebhookEventName = ...> =
  TEmitterEvent extends `${infer TWebhookEvent}.${infer TAction}`
    ? BaseWebhookEvent<...> & { payload: { action: TAction } } : BaseWebhookEvent<...>;
export type WebhookEventName = keyof EventPayloadMap;        // "issues"
export type EmitterWebhookEventName = (typeof emitterEventNames)[number]; // "issues" | "issues.opened"
export type WebhookEvents = ExtractEvents<EmitterWebhookEventName>;       // undotted only
export type WebhookEventDefinition<TEventName> = OpenAPIWebhooks[TEventName]["post"]["requestBody"]["content"]["application/json"];
```

`verifyAndReceive(state, event: EmitterWebhookEventWithStringPayloadAndSignature)`
verifies `event.payload` (a string) against `event.signature` (error text:
"signature does not match event payload and secret"), then `payload =
JSON.parse(event.payload)` and `receive({ id: event.id, name: event.name,
payload })`. `receive.ts` defines `type EventAction = Extract<EmitterWebhookEvent["payload"], { action: string }>["action"]`
and reads `"action" in event.payload ? event.payload.action : null`. Both the
string-bodied and the parsed unit are an "event"; the field is `payload` in
both; `name` is the wire string, typed as a literal union but never parsed.

`@octokit/webhooks-types` (`payload-types/schema.d.ts`, generated):

```ts
export interface IssuesOpenedEvent { action: "opened"; changes?: {...}; issue: Issue & {...};
  repository: Repository; sender: User; installation?: InstallationLite; ... }
export type IssuesEvent = IssuesAssignedEvent | IssuesClosedEvent | ... ;
export interface PushEvent { ... }
export type Schema = BranchProtectionConfigurationEvent | ... | WorkflowRunEvent;
export interface EventPayloadMap { issues: IssuesEvent; push: PushEvent; ... }
export type WebhookEvent = Schema;
export type WebhookEventMap = EventPayloadMap;
export type WebhookEventName = keyof EventPayloadMap;
```

README: `import { WebhookEvent, IssuesOpenedEvent } from
"@octokit/webhooks-types"; const handleWebhookEvent = (event: WebhookEvent) =>
{ if ("action" in event && event.action === "completed") ... }`. So the
decoded body type is `*Event`, but the map from name to it is
`EventPayloadMap` and the field that holds it is `payload`: one object is an
"event" in the types package and a "payload" in webhooks.js.

Per concept: (a) `EmitterWebhookEventWithStringPayloadAndSignature`; (b)
`id`, `name`; (c) `payload: string`; (d) `IssuesOpenedEvent`, `PushEvent`,
`WebhookEvent`; (e) `EmitterWebhookEvent { id, name, payload }`; (f) none,
`action` read after a full parse; (g) `action`, `EventAction`, dotted
`issues.opened`; (h) `name: string`, literal-typed, not parsed; (i) absent.

Sources: [`src/types.ts`](https://github.com/octokit/webhooks.js/blob/main/src/types.ts),
[`src/verify-and-receive.ts`](https://github.com/octokit/webhooks.js/blob/main/src/verify-and-receive.ts),
[`src/event-handler/receive.ts`](https://github.com/octokit/webhooks.js/blob/main/src/event-handler/receive.ts),
[`src/generated/webhook-identifiers.ts`](https://github.com/octokit/webhooks.js/blob/main/src/generated/webhook-identifiers.ts),
[`payload-types/schema.d.ts`](https://github.com/octokit/webhooks/blob/main/payload-types/schema.d.ts),
[`payload-types/README.md`](https://github.com/octokit/webhooks/blob/main/payload-types/README.md).

---

## 6. Probot (JavaScript)

`src/context.ts` imports `EmitterWebhookEvent as WebhookEvent` and
`EmitterWebhookEventName as WebhookEvents` from `@octokit/webhooks`:

```ts
/** The context of the event that was triggered, including the payload and
 *  helpers for extracting information can be passed to GitHub API calls. */
export class Context<Event extends WebhookEvents = WebhookEvents> {
  public name: WebhookEvents;
  public id: string;
  public payload: {...}[Event]["payload"];
  public octokit: ProbotOctokit;   // "in the context of the webhook event"
  public log: Logger;              // "A logger with context about the event."
  constructor(event: WebhookEvent<Event>, octokit, log) {
    this.name = event.name; this.id = event.id; this.payload = event.payload; ... }
  repo<T>(object?: T)        // "Return the `owner` and `repo` params ..."; throws
                             // "context.repo() is not supported for this webhook event."
  issue<T>(object?: T); pullRequest<T>(object?: T);
  get isBot(): boolean       // "Returns a boolean if the actor on the event was a bot."
}
```

Source comment: "set `x-github-delivery` header on all requests sent in
response to the current event. This allows GitHub Support to correlate the
request with the event." The `repo()`/`issue()`/`isBot` accessors are
described as "helpers for extracting information"; they read the already
parsed `payload`.

Per concept: (a) none (delegated to octokit); (b) `context.name`,
`context.id`; (c) none; (d) `context.payload`; (e) `Context<E>`, the pairing
plus ambient services (`octokit`, `log`); (f) none as a pre-parse; post-parse
"helpers for extracting information"; (g) `payload.action`; (h) none; (i)
absent.

Source: [`src/context.ts`](https://github.com/probot/probot/blob/master/src/context.ts).

---

## 7. octocrab `models::webhook_events` (Rust)

Module doc: "Serde mappings from GitHub's Webhook payloads to structs." "The
main entry point is to read the 'event type' from the HTTP request header
sent by github ... and then pass the value, along with the payload, to
`WebhookEvent::try_from_header_and_body` which will validate the payload and
return a valid `WebhookEvent`." Example comment: `// Value picked from the
X-GitHub-Event header` / `let event_name = "ping";`.

```rust
/// A GitHub webhook event.
///
/// The structure is separated in common fields and specific fields, so you can
/// always access the common values without needing to match the exact variant.
pub struct WebhookEvent {
    pub sender: Option<Author>, pub repository: Option<Repository>,
    pub organization: Option<Organization>, pub installation: Option<EventInstallation>,
    #[serde(skip)] pub kind: WebhookEventType,
    #[serde(flatten)] pub specific: WebhookEventPayload,
}
/// Deserialize the body of a webhook event according to the category in the header of the request.
pub fn try_from_header_and_body<B: AsRef<[u8]> + ?Sized>(header: &str, body: &B) -> Result<Self, serde_json::Error>
/// Kind of webhook event.
pub enum WebhookEventType { BranchProtectionRule, ..., #[serde(untagged)] Unknown(String) }
/// The specific part of the payload in a webhook event
pub enum WebhookEventPayload { Issues(Box<IssuesWebhookEventPayload>), ... }
// payload/issues.rs
pub struct IssuesWebhookEventPayload { pub action: IssuesWebhookEventAction, pub issue: Issue, ... }
pub enum IssuesWebhookEventAction { Assigned, Closed, ..., Opened, ... }
```

Inside `try_from_header_and_body`, a private struct does the partial
extraction: "// Intermediate structure allows to separate the common fields
from the event specific one." `struct Intermediate { sender, repository,
organization, installation, #[serde(flatten)] specific: serde_json::Value }`,
followed by `kind.parse_specific_payload(specific)` ("Parse (and verify) the
payload for the specific event kind."). One source file uses "event type",
"category", and "kind" for the header value. The delivery ID is not modelled.

Per concept: (a) the `(header, body)` parameters; (b) `kind` plus the lifted
"common fields" `sender`, `repository`, `organization`, `installation`; (c)
`body: &B`; (d) `IssuesWebhookEventPayload`, `WebhookEventPayload` ("The
specific part of the payload"); (e) `WebhookEvent`; (f) `Intermediate`
(private), two-pass via `serde_json::Value`; (g) `IssuesWebhookEventAction`;
(h) "event type"/"category" in prose, `WebhookEventType` ("Kind of webhook
event") with `Unknown(String)`; (i) absent.

Sources: [`src/models/webhook_events.rs`](https://github.com/XAMPPRocky/octocrab/blob/main/src/models/webhook_events.rs),
[`src/models/webhook_events/payload.rs`](https://github.com/XAMPPRocky/octocrab/blob/main/src/models/webhook_events/payload.rs),
[`src/models/webhook_events/payload/issues.rs`](https://github.com/XAMPPRocky/octocrab/blob/main/src/models/webhook_events/payload/issues.rs).

---

## 8. `google/go-github` (Go)

`github/messages.go` header: "This file provides functions for validating
payloads from GitHub Webhooks."

```go
// EventTypeHeader is the GitHub header key used to pass the event type.
EventTypeHeader = "X-Github-Event"
// DeliveryIDHeader is the GitHub header key used to pass the unique ID for the webhook event.
DeliveryIDHeader = "X-Github-Delivery"
// eventTypeMapping maps webhooks types to their corresponding go-github struct types.
eventTypeMapping = map[string]any{ "issues": &IssuesEvent{}, ... }
// Forward mapping of event types to the string names of the structs.
messageToTypeName = ...; typeToMessageMapping = ...

// ValidatePayload validates an incoming GitHub Webhook event request
// and returns the (JSON) payload.
func ValidatePayload(r *http.Request, secretToken []byte) (payload []byte, err error)
// ValidateSignature validates the signature for the given payload. ...
// payload is the JSON payload sent by GitHub Webhooks.
func ValidateSignature(signature string, payload, secretToken []byte) error
// genMAC generates the HMAC signature for a message provided the secret key and hashFunc.
func genMAC(message, key []byte, hashFunc func() hash.Hash) []byte
// WebHookType returns the event type of webhook request r.
func WebHookType(r *http.Request) string
// DeliveryID returns the unique delivery ID of webhook request r.
func DeliveryID(r *http.Request) string
// ParseWebHook parses the event payload. For recognized event types, a
// value of the corresponding struct type will be returned (as returned
// by [Event.ParsePayload]). An error will be returned for unrecognized event types.
func ParseWebHook(messageType string, payload []byte) (any, error) {
	eventType, ok := messageToTypeName[messageType]
	if !ok { return nil, fmt.Errorf("unknown X-Github-Event in message: %v", messageType) }
	event := Event{ Type: &eventType, RawPayload: (*json.RawMessage)(&payload) }
	return event.ParsePayload()
}
// MessageTypes returns a sorted list of all the known GitHub event type strings ...
func MessageTypes() []string
```

`github/event.go`: "// Event represents a GitHub event." `type Event struct {
Type *string `json:"type"`; Public *bool; RawPayload *json.RawMessage
`json:"payload"`; Repo *Repository; Actor *User; Org *Organization; CreatedAt
*Timestamp; ID *string }` (the Events-API timeline type, reused for webhooks);
`ParsePayload()`: "parses the event payload. For recognized event types, a
value of the corresponding struct type will be returned." with the comment "It
would be nice if e.Type were the snake_case name of the event, but the
existing interface uses the struct name instead."

`github/event_types.go`: "// IssuesEvent is triggered when an issue is
opened, edited, ... The Webhook event name is 'issues'." `Action *string` "//
Action is the action that was performed. Possible values are: ..."; "// The
following fields are only populated by Webhook events." `Repo`, `Sender`,
`Installation`.

Per concept: (a) "webhook request" (`*http.Request`); (b) accessor functions
`WebHookType(r)`, `DeliveryID(r)`; no struct; (c) `payload []byte` ("the
(JSON) payload"), "message" only inside the HMAC helpers; (d) `IssuesEvent`,
`PushEvent` ("struct types"); (e) `Event { Type, RawPayload, Repo, Actor,
Org, ID, ... }`; (f) `RawPayload *json.RawMessage` (Go: "can be used to delay
JSON decoding"); (g) `Action *string`; (h) distinguished: "message type"
(`messageType`, `MessageTypes()`) is the wire string, "event type"
(`eventType`) is the struct name, mapped by `messageToTypeName`; (i) absent.

Sources: [`github/messages.go`](https://github.com/google/go-github/blob/master/github/messages.go),
[`github/event.go`](https://github.com/google/go-github/blob/master/github/event.go),
[`github/event_types.go`](https://github.com/google/go-github/blob/master/github/event_types.go),
[`encoding/json` `RawMessage`](https://github.com/golang/go/blob/master/src/encoding/json/stream.go).

---

## 9. `go-playground/webhooks` v6 (Go)

```go
// Event defines a GitHub hook event type
type Event string
// GitHub hook types
const ( IssuesEvent Event = "issues"; PushEvent Event = "push"; PingEvent Event = "ping"; ... )
// Parse verifies and parses the events specified and returns the payload object or an error
func (hook Webhook) Parse(r *http.Request, events ...Event) (interface{}, error) {
	event := r.Header.Get("X-GitHub-Event"); gitHubEvent := Event(event)
	...
	payload, err := io.ReadAll(r.Body)
	...
	case IssuesEvent: var pl IssuesPayload; err = json.Unmarshal([]byte(payload), &pl); return pl, err
}
// IssuesPayload contains the information for GitHub's issues hook event
type IssuesPayload struct { Action string `json:"action"`; Issue struct {...}; ... }
```

Errors: `ErrEventNotFound = "event not defined to be parsed"`,
`ErrParsingPayload = "error parsing payload"`, `ErrMissingGithubEventHeader`.
The opposite of go-github: `Event` is the kind, `*Payload` is the decoded
body. The delivery ID is not read.

Per concept: (a) `*http.Request` ("hook"); (b) none; (c) `payload` (`[]byte`);
(d) `IssuesPayload`, `PushPayload`; (e) none; (f) none; (g) `Action string`;
(h) `Event` is the wire string by type conversion (`Event(event)`), not
parsed or validated; (i) absent.

Sources: [`github/github.go`](https://github.com/go-playground/webhooks/blob/master/github/github.go),
[`github/payload.go`](https://github.com/go-playground/webhooks/blob/master/github/payload.go).

---

## 10. gidgethub (Python, sans-I/O)

```python
class Event:
    """Details of a GitHub webhook event."""
    def __init__(self, data: Any, *, event: str, delivery_id: str) -> None:
        self.data = data
        # Event is not an enum as GitHub provides the string. This allows them
        # to add new events without having to mirror them here. ...
        self.event = event
        self.delivery_id = delivery_id
    @classmethod
    def from_http(cls, headers, body: bytes, *, secret=None) -> "Event":
        """Construct an event from HTTP headers and JSON body data. ..."""
        ...
        data = _decode_body(headers["content-type"], body, strict=True)
        return cls(data, event=headers["x-github-event"], delivery_id=headers["x-github-delivery"])

def validate_event(payload: bytes, *, signature: str, secret: str) -> None:
    """Validate the signature of a webhook event."""
```

`docs/sansio.rst`: `Event(data, *, event, delivery_id)` "Representation of a
GitHub webhook event."; `data` "The payload of the event."; `event` "The
string representation of the triggering event."; `delivery_id` "The unique ID
of the event." `docs/routing.rst`: `add(func, event_type, **data_detail)`,
"The *event_type* argument corresponds to the `gidgethub.sansio.Event.event`
attribute ... The arbitrary keyword arguments is used as a key/value pair to
compare against what is provided in `gidgethub.sansio.Event.data`", example
`router.add(callback, "issues", action="opened")`; `fetch()` reads
`event.data[data_key]`.

Per concept: (a) the `(headers, body)` parameters of `from_http`; (b)
`event`, `delivery_id`; (c) `body: bytes` / `payload: bytes`; (d) none,
`data` is a dict; (e) `Event(data, *, event, delivery_id)`; (f) none; (g)
`action` is one possible `data_detail` key, not a named concept; (h) `event:
str`, deliberately "not an enum"; (i) absent.

Sources: [`gidgethub/sansio.py`](https://github.com/gidgethub/gidgethub/blob/master/gidgethub/sansio.py),
[`gidgethub/routing.py`](https://github.com/gidgethub/gidgethub/blob/master/gidgethub/routing.py),
[docs/sansio.rst](https://github.com/gidgethub/gidgethub/blob/master/docs/sansio.rst),
[docs/routing.rst](https://github.com/gidgethub/gidgethub/blob/master/docs/routing.rst).

---

## 11. Stripe (docs, stripe-python, stripe-node)

Docs: "Stripe uses HTTPS to send webhook events to your app as a JSON payload
that includes event information." "Stripe requires the raw body of the
request to perform signature verification." "Retrieve the event by verifying
the signature using the raw body and the endpoint secret":
`event = Stripe::Webhook.construct_event(payload, signature, endpoint_secret)`;
then `case event.type when 'payment_intent.succeeded' payment_intent =
event.data.object`; else "Unhandled event type". Manual verification: "The
`signed_payload` string is created by concatenating: The timestamp (as a
string), The character `.`, The actual JSON payload (that is, the request
body)". Dedup: "Track event IDs to identify duplicate deliveries". Deliveries
are "Event deliveries" with a "delivery attempt".

The Event object: `id`, `object: "event"`, `api_version`, `created`, `data`
"Object containing data associated with the event" (`data.object`), `livemode`,
`pending_webhooks`, `request` "Information on the API request that triggers
the event", `type` "Description of the event (for example, `invoice.created`
or `charge.refunded`)", `context`, `account`. Metadata and data share one
JSON object; the only header is `Stripe-Signature` (`t=...,v1=...`). Thin
events add `EventNotification` classes:
`client.parse_event_notification(webhook_body, sig_header, webhook_secret)`,
`event_notif.type`, `fetch_related_object()`, `fetch_event()`,
`UnknownEventNotification`.

stripe-python `stripe/_webhook.py`:

```python
WebhookPayload = Union[str, bytes, bytearray]
"""The raw body of an incoming webhook, as read off the request body. ..."""
class Webhook:
    def construct_event(payload: WebhookPayload, sig_header, secret, tolerance=...) -> Event:
        """Constructs a snapshot event from an incoming webhook after verifying its authenticity. ..."""
class WebhookSignature:
    def verify_header(payload, sig_header, secret, tolerance):
        """Verifies the authenticity (and recency) of a webhook ..."""
def maybe_extract_from_cloud_provider_envelope(payload: WebhookPayload):
    """Internal helper to extract the inner type from a cloud provider envelope (regardless of what's in there).
    If the payload is already a raw Stripe event (object is 'event' or 'v2.core.event'), returns the parsed dict as-is."""
    # AWS: data["detail"]; Azure: "specversion" in data and data["data"]
    raise ValueError("Unrecognized event format. The payload must be an AWS EventBridge/Azure Event Grid event envelope or a Stripe webhook (thin event notification or snapshot).")
```

stripe-node `Webhooks.ts`: `constructEvent(payload: WebhookPayload, header:
WebhookHeader, secret: string, tolerance?, cryptoProvider?, receivedAt?):
Event` and `constructEventWithoutVerification: (payload: string) => Event`.

Per concept: (a) "incoming webhook"; (b) `Stripe-Signature` header only; `id`,
`type`, `created`, `request` live in the body; (c) `WebhookPayload` "The raw
body of an incoming webhook"; (d) `Event` (one class for all snapshot types,
`type` a string, `data.object` polymorphic); per-type `*EventNotification`
for thin events; (e) `Event` is the whole body (metadata and data in one
object); (f) none; (g) none, folded into dotted `type`; (h) none; (i) "cloud
provider envelope": the AWS EventBridge (`detail`) or Azure Event Grid
(CloudEvents `data`) wrapper around the whole Stripe event.

Sources: [Receive Stripe events in your webhook endpoint](https://docs.stripe.com/webhooks),
[The Event object](https://docs.stripe.com/api/events/object),
[`stripe/_webhook.py`](https://github.com/stripe/stripe-python/blob/master/stripe/_webhook.py),
[`src/Webhooks.ts`](https://github.com/stripe/stripe-node/blob/master/src/Webhooks.ts).

---

## 12. Slack Events API and Socket Mode

**Events API over HTTP.** "Your server receives a JSON payload describing
that event." The POST body is:

```json
{ "type": "event_callback", "token": "...", "team_id": "T...", "api_app_id": "A...",
  "event": { "type": "name_of_event", "event_ts": "...", "user": "U...", ... },
  "event_context": "EC...", "event_id": "Ev...", "event_time": 1234567890,
  "authorizations": [ ... ], "is_ext_shared_channel": false, "context_team_id": "T...", "context_enterprise_id": null }
```

Heading "Callback field overview": "Also referred to as the 'outer event', or
the JSON object containing the event that happened". Field `type`: "This
reflects the type of callback you're receiving. Typically, that is
`event_callback`. ... The `event` field's 'inner event' will also contain a
`type` field indicating which event type lurks within". Field `event`:
"Contains the inner set of fields representing the event type that's
happening. The event wrapper is an event envelope of sorts, and the event
field represents the contents of that envelope. Learn more about the event
wrapper". Field `event_id`: "A unique identifier for this specific event,
globally unique across all workspaces." Heading "Event type structure": "the
inner `event` structure is identical to corresponding events, but are wrapped
in a kind of event envelope in the callbacks we send". Inner `type`: "The
specific name of the event described by its adjacent fields ... Examples:
`reaction_added`, `message.channels`, `team_join`". Retries carry
`x-slack-retry-num` and `x-slack-retry-reason`; a failed send is an "event
delivery attempt".

**Socket Mode.** "Event payloads sent to your app via Socket Mode are
identical to the typical Events API payloads, with some additional metadata:"

```json
{ "payload": <event_payload>, "envelope_id": <unique_identifier_string>,
  "type": <event_type_enum>, "accepts_response_payload": <accepts_response_payload_bool> }
```

"Use the `envelope_id` field in the object you receive from your WebSocket to
send a response back to Slack acknowledging that you've received the event":
`{ "envelope_id": ..., "payload": ... // optional }`. The frame's `type` is
the channel (`events_api`, `interactive`, `slash_commands`), not the event
type. So Socket Mode has three layers: the envelope (`payload`,
`envelope_id`, `type`), whose `payload` is the Events API outer event
(`event_callback`), whose `event` is the inner event.

Per concept: (a) HTTP: "JSON payload"; Socket Mode: the envelope; (b) outer
event fields `event_id`, `event_time`, `team_id`, `api_app_id`,
`event_context`, `authorizations`; Socket `envelope_id`; (c) "JSON payload",
`payload`; (d) the inner `event` (untyped JSON, `event.type`); (e) "outer
event" / "event wrapper" / `event_callback`; (f) none; (g) none, folded into
dotted names (`message.channels`); (h) none; (i) two uses: the Events API's
metaphor for the outer event ("an event envelope of sorts") and Socket Mode's
first-class `envelope_id` on the WebSocket frame that wraps the entire Events
API payload.

Sources: [The Events API](https://api.slack.com/apis/events-api) (served from docs.slack.dev/apis/events-api),
[Using Socket Mode](https://api.slack.com/apis/socket-mode) (served from docs.slack.dev/apis/events-api/using-socket-mode).

---

## 13. Svix

"Entities Overview": "Messages are the webhooks being sent. They can have a
content, event type, and a few other properties." "Messages can have an
associated `eventId`." "Attempts represent an attempt that has been made to
send a message to an endpoint. Attempts also record the response content of
the attempt, the response HTTP status code". "Event types are identifiers on
messages that describe the message being sent and implies its associated
schema. Each message has exactly one event type". "Endpoints represent a
target for messages sent to a specific application." Sending:
`svix.message.create('some-user', { application: ..., payload: {/* ... */} })`.

Receiving: "The payload is the raw (string) body of the request, and the
headers are the headers passed in the request." Headers `svix-id:
msg_p5jXN8AQM9LWM0D4loKWxJek`, `svix-timestamp`, `svix-signature` are "the
Svix-branded aliases of the spec's `webhook-id`, `webhook-timestamp`, and
`webhook-signature` headers" (Standard Webhooks, which Svix "co-created").
`wh.verify(payload, headers)`, then `msg = JSON.parse(payload); // Do
something with the message...`.

Per concept: (a) message (identified by `svix-id: msg_...`); (b) `svix-id`,
`svix-timestamp`, `svix-signature`; (c) payload, "the raw (string) body of
the request"; (d) none; (e) none (`msg` after parse); (f) none; (g) none, the
event type is hierarchical and dotted; (h) none; (i) absent.

Sources: [Entities Overview](https://docs.svix.com/overview),
[How to Verify Webhooks](https://docs.svix.com/receiving/verifying-payloads/how).

---

## 14. `http::request::Parts` (Rust)

```rust
/// Represents an HTTP request.
///
/// An HTTP request consists of a head and a potentially optional body. The body
/// component is generic, enabling arbitrary types to represent the HTTP body.
/// For example, the body could be `Vec<u8>`, a `Stream` of byte chunks, or a
/// value that has been deserialized.
pub struct Request<T> { head: Parts, body: T }

/// Component parts of an HTTP `Request`
///
/// The HTTP request head consists of a method, uri, version, and a set of
/// header fields.
pub struct Parts { pub method: Method, pub uri: Uri, pub version: Version, pub headers: HeaderMap, pub extensions: Extensions }

/// Consumes the request returning the head and body parts.
pub fn into_parts(self) -> (Parts, T)
/// Creates a new `Request` with the given components parts and body.
pub fn from_parts(parts: Parts, body: T) -> Request<T>
```

The prose word for the metadata half is "head"; the type is `Parts`; the
private field is `head`. The body's type parameter is documented as ranging
from bytes to "a value that has been deserialized", so `Request<T>` covers
both the raw and the decoded pairing with one type.

Per concept: (a) `Request<T>`; (b) `Parts` / "head"; (c) `body: T` as
`Vec<u8>`; (d) `T` deserialized; (e) `Request<T>`; (f)–(i) none.

Sources: [`http::request::Parts`](https://docs.rs/http/latest/http/request/struct.Parts.html),
[`src/request.rs`](https://github.com/hyperium/http/blob/master/src/request.rs).

---

## 15. `serde_json::value::RawValue` (Rust)

Doc: "Reference to a range of bytes encompassing a single valid JSON value in
the input data. A `RawValue` can be used to defer parsing parts of a payload
until later, or to avoid parsing it at all in the case that part of the
payload just needs to be transferred verbatim into a different output object.
When serializing, a value of this type will retain its original formatting
and will not be minified or pretty-printed." The example struct is `struct
Input<'a> { code: u32, #[serde(borrow)] payload: &'a RawValue }` ("The
correct range of bytes is borrowed from the input data and pasted verbatim
into the output."). "# Ownership: The typical usage of `RawValue` will be in
the borrowed form"; the boxed form "involves buffering the raw value from the
I/O stream into memory."

Vocabulary: "defer parsing", "raw", "borrowed", "verbatim", and "payload" for
the containing document. There is no noun for the enclosing shallow struct
that reads a few fields and defers the rest. Adjacent conventions: serde's
`IgnoredAny`, "An efficient way of discarding data from a deserializer";
Go's `json.RawMessage`, "a raw encoded JSON value ... can be used to delay
JSON decoding or precompute a JSON encoding" (what go-github's `RawPayload`
is). The crate's private `Probe<'a>` is this idiom exactly: `#[serde(borrow)]
action: Option<&'a RawValue>` and so on (`src/envelope.rs`).

Per concept: (c) `RawValue`, "a range of bytes encompassing a single valid
JSON value"; (f) "defer parsing parts of a payload until later"; no noun.
Others: none.

Sources: [`RawValue`](https://docs.rs/serde_json/latest/serde_json/value/struct.RawValue.html),
[`src/raw.rs`](https://github.com/serde-rs/json/blob/master/src/raw.rs),
[serde `IgnoredAny`](https://github.com/serde-rs/serde/blob/master/serde_core/src/de/ignored_any.rs),
[Go `encoding/json` `RawMessage`](https://github.com/golang/go/blob/master/src/encoding/json/stream.go).

---

## 16. Synthesis

### One row per source, one column per concept

"—" means the source has no word for it.

| Source | (a) raw unit | (b) metadata | (c) body bytes | (d) decoded typed body | (e) meta + decoded | (f) partial extraction | (g) action | (h) wire name vs kind | (i) "envelope" |
|---|---|---|---|---|---|---|---|---|---|
| CloudEvents spec | message (binary-/structured-mode) | context attributes | data ("i.e. the payload") | — (`data` opaque) | event | — (rationale: attributes "inspected ... without having to deserialize the event data") | — (folded into `type`: `com.github.pull_request.opened`) | — (`type` is a producer-defined string) | "transport envelope"; JSON "Envelope" = whole structured event |
| cloudevents-sdk | Message (`BinaryDeserializer`, `StructuredDeserializer`, `Encoding`) | `Attributes` / `AttributesReader` | `Data::Binary`, `Data::String` | — (`Data::Json(Value)`) | `Event { attributes, data, extensions }` | — | — | — (`ty() -> &str`) | — |
| Standard Webhooks | webhook / message (`msg_` id) | "webhook metadata": `webhook-id`, `webhook-timestamp` | payload, "body" | — | — (`{type, timestamp, data}` one object) | — | — (dotted `type`) | — | — |
| GitHub docs + REST | delivery; REST `request: {headers, payload}` | "delivery headers"; REST `guid`, `event`, `action`, `installation_id`, `repository_id` | payload / "request body" | "webhook payload object" | — (REST delivery object nearest) | — (done, unnamed, in REST delivery) | action / "Action type" / "event types" / "activity type" | "name of the event"; no parsed form | — |
| @octokit/webhooks (+types) | `EmitterWebhookEventWithStringPayloadAndSignature` | `id`, `name` | `payload: string` | `IssuesOpenedEvent`, `PushEvent`, `WebhookEvent` | `EmitterWebhookEvent { id, name, payload }` | — | `action`, `EventAction`, dotted `issues.opened` | `name` literal-typed, unparsed | — |
| Probot | — (octokit) | `context.name`, `context.id` | — | `context.payload` | `Context<E>` (+ `octokit`, `log`) | — (post-parse `repo()`, `issue()`, `isBot`) | `payload.action` | — | — |
| octocrab | `(header, body)` params | `kind` + common `sender`, `repository`, `organization`, `installation` | `body: &B` | `IssuesWebhookEventPayload`, `WebhookEventPayload` | `WebhookEvent` | `Intermediate` (private) | `IssuesWebhookEventAction` | "event type"/"category" prose; `WebhookEventType` + `Unknown(String)` | — |
| go-github | "webhook request" | `WebHookType(r)`, `DeliveryID(r)` | `payload []byte`; "message" in HMAC | `IssuesEvent`, `PushEvent` | `Event { Type, RawPayload, ... }` | `RawPayload *json.RawMessage` | `Action *string` | "message type" (wire) vs "event type" (struct name) | — |
| go-playground | `*http.Request` ("hook") | — | `payload []byte` | `IssuesPayload`, `PushPayload` | — | — | `Action string` | `Event` string by conversion | — |
| gidgethub | `(headers, body)` params | `event`, `delivery_id` | `body: bytes` / `payload: bytes` | — (dict) | `Event(data, *, event, delivery_id)` | — | `action=` as a `data_detail` key | `event: str`, "not an enum" | — |
| Stripe | "incoming webhook" | `Stripe-Signature`; `id`/`type`/`created` in body | `WebhookPayload` "raw body" | `Event` (one class); `*EventNotification` (thin) | `Event` (whole body) | — | — (dotted `type`) | — | "cloud provider envelope" (EventBridge/Event Grid) around the event |
| Slack | HTTP: "JSON payload"; Socket: envelope | outer `event_id`, `team_id`, `api_app_id`, `event_time`, `authorizations`; `envelope_id` | "JSON payload", `payload` | inner `event` (untyped) | "outer event" / "event wrapper" / `event_callback` | — | — (dotted `message.channels`) | — | "an event envelope of sorts" (outer event); Socket Mode `envelope_id` frame |
| Svix | message (`svix-id: msg_`) | `svix-id`, `svix-timestamp`, `svix-signature` | payload, "raw (string) body" | — | — | — | — (dotted event type) | — | — |
| `http` crate | `Request<T>` | `Parts` / "head" | `body: T` | `T` deserialized | `Request<T>` | — | — | — | — |
| `serde_json` | — | — | `RawValue` | — | — | "defer parsing parts of a payload" | — | — | — |

### Where the ecosystem agrees

- **(c) The bytes are the "payload".** GitHub, go-github, go-playground,
  gidgethub (`validate_event(payload)`), Stripe (`WebhookPayload`), Svix,
  Standard Webhooks, octokit (`payload: string`), and CloudEvents ("i.e. the
  payload") all use it. "Body" is the secondary word (octocrab
  `try_from_header_and_body`, gidgethub `from_http(headers, body)`, the `http`
  crate, Stripe's "raw body"). "Message" for the bytes appears only inside
  go-github's HMAC helpers (`genMAC(message, ...)`); elsewhere "message" is
  the whole unit (Standard Webhooks, Svix, CloudEvents).
- **(g) Only GitHub has a separate action field.** Every other source folds
  the sub-classification into a dotted type string: CloudEvents
  `com.github.pull_request.opened`, Standard Webhooks `user.created`, Stripe
  `payment_intent.succeeded`, Slack `message.channels`, Svix event types.
  octokit's `EmitterWebhookEventName` (`issues.opened`) bridges the two.
  Within GitHub the field is called three things: "action" / "Action type"
  (webhooks docs), "event types" (`check_run` prose on the same page), and
  "activity type" (GitHub Actions).
- **(i) "Envelope", where it appears, is the outermost transport layer around
  the whole event, metadata and data together**: CloudEvents' transport
  envelope and JSON Envelope, Slack's Socket Mode frame (`envelope_id` wraps
  the entire Events API payload) and its "event envelope of sorts" metaphor
  for the outer event, and Stripe's "cloud provider envelope" (EventBridge /
  Event Grid wrapper around the Stripe event). No source uses "envelope" for a
  receiver-side pairing of metadata with still-undecoded bytes. It is absent
  from GitHub, Standard Webhooks, Svix, octokit, Probot, octocrab, go-github,
  go-playground, gidgethub, cloudevents-sdk, `http`, and `serde_json`.
- **Delivery vocabulary.** GitHub ("webhook delivery", `X-GitHub-Delivery`,
  REST "deliveries" and "attempts"), Stripe ("Event deliveries", "delivery
  attempt"), Slack ("event delivery attempt"), Standard Webhooks and Svix
  ("attempt") agree that a delivery/attempt is one send, distinct from the
  event.

### Where it splits

- **(d) Is the decoded typed body an "Event" or a "Payload"?** Split down
  the middle. *Event*: `@octokit/webhooks-types` (`IssuesOpenedEvent`,
  `WebhookEvent`), go-github (`IssuesEvent`), Stripe (`Event`), Slack (inner
  `event`). *Payload*: octocrab (`IssuesWebhookEventPayload`), go-playground
  (`IssuesPayload`), webhooks.js (`payload` field, `EventPayloadMap`), Probot
  (`context.payload`), GitHub docs ("webhook payload object"), gidgethub
  (`data`, "The payload of the event"). The same object is `IssuesOpenedEvent`
  in the types package and `event.payload` in webhooks.js; go-github and
  go-playground are exact mirror images (`IssuesEvent` vs `IssuesPayload`,
  `WebHookType()` string vs `Event` string type).
- **Is "event" the whole thing, the body, or the kind?** All three senses
  are live. *Whole unit*: gidgethub `Event(data, event, delivery_id)`, octokit
  `EmitterWebhookEvent {id, name, payload}`, octocrab `WebhookEvent`,
  go-github `Event{Type, RawPayload}`, CloudEvents `Event`, Stripe `Event`,
  Slack "outer event". *Body*: webhooks-types, go-github per-kind structs,
  Slack inner event, GitHub Actions `github.event`. *Kind/name*: go-playground
  `Event` string, gidgethub's `.event` attribute, GitHub's `X-GitHub-Event`
  "name of the event", GitHub docs' "event" as the subscription category.
  gidgethub uses two senses in one class (`Event.event`).
- **(b) No shared noun for the metadata half.** CloudEvents: "context
  attributes". Standard Webhooks: "webhook metadata". GitHub: "delivery
  headers" and the unnamed REST delivery object. `http`: `Parts` / "head".
  Slack: "outer event" / "event wrapper". Probot: `Context` (with services
  mixed in). Everyone else: loose fields (`id`/`name`; `event`/`delivery_id`;
  `kind` + common fields) or accessor functions (`WebHookType(r)`,
  `DeliveryID(r)`).
- **(h) Wire name vs parsed kind: mostly not distinguished.** gidgethub
  refuses on purpose ("not an enum"); go-playground converts the string to a
  named string type; octokit literal-types the string; GitHub says "the name
  of the event". Two sources distinguish: octocrab (`WebhookEventType` enum
  with `Unknown(String)`; prose uses "event type", "category", and "kind" in
  one file) and go-github ("message type" = wire string, "event type" =
  struct name, joined by `messageToTypeName`; comment: "It would be nice if
  e.Type were the snake_case name of the event, but the existing interface
  uses the struct name instead").
- **(e) The pairing is either the same "event" type as the raw unit or a
  framework `Context`.** octokit and gidgethub make no type distinction
  between "meta + string" and "meta + parsed": the same shape, `payload`
  string versus object. `http::Request<T>` does the same with a type
  parameter. Only Probot names the pairing differently (`Context`), and it
  adds ambient services.

### Who has a name for the partial extraction (f)

Nobody has a public noun or verb for it. The words "probe", "peek", "sniff",
and "preview" do not occur in any source fetched.

- octocrab: private `struct Intermediate`, comment "Intermediate structure
  allows to separate the common fields from the event specific one"; a full
  parse into `serde_json::Value` followed by `parse_specific_payload`.
- go-github: `RawPayload *json.RawMessage`, Go's "can be used to delay JSON
  decoding".
- serde_json: `RawValue` "can be used to defer parsing parts of a payload
  until later"; the borrowed idiom the crate's `Probe` already uses.
- CloudEvents: states the goal (context attributes "inspected at the
  destination without having to deserialize the event data"; identity fields
  duplicated into attributes "so that event consumers can easily access this
  information without needing to decode and examine the event data") but
  assigns the lifting to the producer and names no consumer-side step. Its
  GitHub adapter performs the extraction (`type` from "action", `source` from
  `repository.url`, `subject` from `issue.number`) without a word for it.
- GitHub: the REST delivery object lifts `action`, `installation_id`,
  `repository_id` beside `request.payload`, unnamed.
- Probot: "helpers for extracting information" (`repo()`, `issue()`,
  `isBot`), but these read an already parsed payload.

### Normative status, and GitHub versus CloudEvents' two modes

CloudEvents is the only spec here that defines terms normatively (RFC 2119),
and it does so for its own model, event / context attributes / data / message
/ protocol binding / event format, not for webhooks at large; its GitHub
mapping is a non-normative adapter and its "Design Goals" live in the
non-normative primer. Standard Webhooks calls itself "a set of conventions",
recommends `{type, timestamp, data}` but "does not dictate the structure",
and defines "webhook metadata" only by listing the two headers. GitHub's
pages are descriptive. No spec defines "envelope" for a receiver's verified
unit; the closest normative use is the JSON format's §3 "Envelope", the JSON
container for a whole structured-mode event.

GitHub's wire format is neither CloudEvents mode cleanly. Like **binary
mode**, it carries the event name and delivery id in headers
(`X-GitHub-Event`, `X-GitHub-Delivery`, the analogues of `ce-type`, `ce-id`)
with an `application/json` body of application data. Like **structured
mode**, it carries routing/context attributes inside the body: `action`,
`installation`, `repository`, `sender`, which GitHub lists as "Common payload
parameters". CloudEvents' rule for that situation is that the producer copies
such fields into context attributes so consumers need not "decode and
examine the event data"; GitHub does this only after the fact, in its REST
delivery object (`event`, `action`, `installation_id`, `repository_id`), not
on the wire. Standard Webhooks explicitly permits GitHub's placement:
"Additional metadata can be added both as top-level properties or as part of
data". Consequently every GitHub receiver that routes on `action` (octokit,
Probot, gidgethub, octocrab, this crate) reads the body before it can route,
which is the step CloudEvents designed context attributes to make
unnecessary, and which no source has named.
