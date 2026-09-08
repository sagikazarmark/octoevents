//! The envelope's tests, beside the production code so they share the
//! crate's fixtures. Grouped by concern: the receiving path through
//! `from_signed`, the header rules it applies, the probe, the meta as a
//! value, the wire format, `decode`, and the `HeaderView` constructors.
//!
//! Every signed envelope here is checked against [`verifier`], which also
//! signs the bodies it accepts; [`headers`] is the well-formed header set
//! of a `pull_request` delivery, for a test to start from.

use crate::{HeaderView, Secret, Verifier};

const BODY: &[u8] = br#"{
    "action":"opened",
    "installation":{"id":42},
    "repository":{"id":1,"name":"repo","full_name":"octo/repo","owner":{"login":"octo"}},
    "organization":{"login":"github"},
    "sender":{"login":"monalisa"}
}"#;

/// The verifier every signed envelope here is checked against; it also
/// signs the bodies it accepts.
fn verifier() -> Verifier {
    Verifier::new(Secret::new("secret"))
}

/// The headers of a well-formed `pull_request` delivery carrying
/// `signature`, target included; a test that wants one header wrong
/// overrides it.
fn headers(signature: &str) -> HeaderView<'_> {
    HeaderView::new()
        .signature(signature)
        .delivery_id("delivery")
        .event_name("pull_request")
        .content_type("application/json; charset=utf-8")
        .target_type("repository")
        .target_id("7")
}

/// The receiving path, `from_signed`: it verifies, then reads the headers
/// and probes the payload, keeping the bytes exactly as they arrived.
mod receive {
    use bytes::Bytes;

    use super::{BODY, headers, verifier};
    use crate::{
        Action, BodyError, Envelope, EventKind, EventMeta, HeaderView, ReceiveError, RepositoryRef,
        TargetType, VerifyError,
    };

    #[test]
    fn verifies_then_extracts_the_metadata() {
        let verifier = verifier();
        let signature = verifier.sign(BODY);

        let envelope =
            Envelope::from_signed(&verifier, &headers(&signature), Bytes::from_static(BODY))
                .unwrap();

        let meta = &envelope.meta;
        assert_eq!(meta.delivery_id, "delivery");
        assert_eq!(meta.kind, EventKind::PullRequest);
        assert_eq!(meta.action, Some(Action::Opened));
        assert_eq!(meta.installation_id, Some(42));
        assert_eq!(
            meta.repository,
            Some(RepositoryRef::new(1, "repo", "octo/repo", "octo"))
        );
        assert_eq!(meta.organization.as_deref(), Some("github"));
        assert_eq!(meta.sender.as_deref(), Some("monalisa"));
        assert_eq!(meta.target_type, Some(TargetType::Repository));
        assert_eq!(meta.target_id, Some(7));
        assert_eq!(envelope.raw_payload, Bytes::from_static(BODY));
    }

    #[test]
    fn from_signed_reads_the_target_from_the_headers_when_the_payload_yields_nothing() {
        // The target is the one thing the receiving path knows and the test
        // path does not: it comes from headers, not from the payload.
        let body = Bytes::from_static(b"not json");
        let signature = verifier().sign(&body);

        let envelope =
            Envelope::from_signed(&verifier(), &headers(&signature), body.clone()).unwrap();

        let mut expected = EventMeta::new("delivery", EventKind::PullRequest);
        expected.target_type = Some(TargetType::Repository);
        expected.target_id = Some(7);
        assert_eq!(envelope.meta, expected);
        assert_eq!(envelope.raw_payload, body);
    }

    #[test]
    fn unknown_event_and_action_remain_routable() {
        let body = Bytes::from_static(br#"{"action":"brand_new"}"#);
        let signature = verifier().sign(&body);
        let headers = HeaderView::new()
            .signature(&signature)
            .delivery_id("delivery")
            .event_name("brand_new")
            .content_type("application/json");

        let envelope = Envelope::from_signed(&verifier(), &headers, body).unwrap();

        assert_eq!(envelope.meta.kind, EventKind::Unknown("brand_new".into()));
        assert_eq!(
            envelope.meta.action,
            Some(Action::Unknown("brand_new".into()))
        );
    }

    #[test]
    fn authenticates_before_rejecting_content_type() {
        let headers = HeaderView::new()
            .signature("sha256=0000000000000000000000000000000000000000000000000000000000000000")
            .delivery_id("delivery")
            .event_name("push")
            .content_type("application/x-www-form-urlencoded");

        assert_eq!(
            Envelope::from_signed(&verifier(), &headers, Bytes::new()),
            Err(ReceiveError::Verify(VerifyError::Mismatch))
        );
    }

    #[test]
    fn a_body_read_failure_carries_the_transports_error_as_its_source() {
        // The message says what the receiver could not do; the transport's
        // own text is one `source()` hop down, where whoever holds the value
        // (a transport built on `from_signed`, an error chain walker) finds
        // it without the variant naming the transport's type.
        let error = ReceiveError::BodyRead(BodyError::new("connection reset by peer"));

        assert_eq!(error.to_string(), "could not read the webhook body");
        assert_eq!(
            std::error::Error::source(&error).map(ToString::to_string),
            Some("connection reset by peer".to_string())
        );
    }

    #[test]
    fn preserves_multibyte_and_escaped_payload_bytes_exactly() {
        // Signing happens over bytes as sent: a re-encode would change both the
        // escape form and the MAC, so assert the exact body survives.
        const UNICODE_BODY: &[u8] =
            "{\"action\":\"opened\",\"zen\":\"⚡ \\u00e9 caf\u{e9} 🐙\"}".as_bytes();

        let signature = verifier().sign(UNICODE_BODY);
        let headers = HeaderView::new()
            .signature(&signature)
            .delivery_id("delivery")
            .event_name("pull_request")
            .content_type("application/json");

        let envelope =
            Envelope::from_signed(&verifier(), &headers, Bytes::from_static(UNICODE_BODY)).unwrap();

        assert_eq!(envelope.raw_payload.as_ref(), UNICODE_BODY);
        assert_eq!(envelope.meta.action, Some(Action::Opened));

        let decoded: serde_json::Value = envelope.decode().unwrap();
        assert_eq!(decoded["zen"], "⚡ é café 🐙");
    }
}

/// The header rules `from_signed` applies once the signature holds: which
/// headers are required, that an empty one is missing, and which content
/// types are accepted.
mod header_rules {
    use bytes::Bytes;

    use super::{headers, verifier};
    use crate::{Envelope, EventKind, HeaderView, ReceiveError, VerifyError, header};

    /// What the receiving path makes of a signed, otherwise well-formed empty
    /// delivery under `content_type`, reduced to the kind it read.
    fn received_as(content_type: &str) -> Result<EventKind, ReceiveError> {
        let signature = verifier().sign(b"");
        let headers = headers(&signature).content_type(content_type);

        Envelope::from_signed(&verifier(), &headers, Bytes::new())
            .map(|envelope| envelope.meta.kind)
    }

    #[test]
    fn requires_signature_content_type_and_routing_headers() {
        let no_signature = HeaderView::new()
            .delivery_id("delivery")
            .event_name("push")
            .content_type("application/json");
        assert_eq!(
            Envelope::from_signed(&verifier(), &no_signature, Bytes::new()),
            Err(ReceiveError::Verify(VerifyError::MissingSignature))
        );

        let signature = verifier().sign(b"");
        let form = HeaderView::new()
            .signature(&signature)
            .delivery_id("delivery")
            .event_name("push")
            .content_type("application/x-www-form-urlencoded");
        assert_eq!(
            Envelope::from_signed(&verifier(), &form, Bytes::new()),
            Err(ReceiveError::UnsupportedContentType)
        );

        let no_delivery = HeaderView::new()
            .signature(&signature)
            .event_name("push")
            .content_type("application/json");
        assert_eq!(
            Envelope::from_signed(&verifier(), &no_delivery, Bytes::new()),
            Err(ReceiveError::MissingHeader {
                name: header::DELIVERY_ID
            })
        );
        assert_eq!(
            ReceiveError::MissingHeader {
                name: header::DELIVERY_ID
            }
            .to_string(),
            "missing x-github-delivery header"
        );
    }

    #[test]
    fn accepts_a_json_content_type_by_its_media_type_alone() {
        // The media type is compared case-insensitively, and neither its
        // parameters nor the whitespace around it take part.
        for content_type in [
            "application/json",
            "APPLICATION/JSON",
            "Application/Json",
            "application/json; charset=utf-8",
            "application/json;charset=utf-8",
            "application/json;",
            " application/json",
            "application/json ",
            "application/json ; charset=utf-8",
        ] {
            assert_eq!(
                received_as(content_type),
                Ok(EventKind::PullRequest),
                "content type {content_type:?}"
            );
        }
    }

    #[test]
    fn refuses_every_content_type_whose_media_type_is_not_application_json() {
        // Near-misses included: another JSON media type, GitHub's own API
        // media type, GitHub's other webhook content type, `application/json`
        // as a parameter rather than the media type, and an empty value.
        for content_type in [
            "",
            "text/json",
            "application/vnd.github+json",
            "application/json-patch+json",
            "application/x-www-form-urlencoded",
            "text/plain; type=application/json",
            "application/json charset=utf-8",
        ] {
            assert_eq!(
                received_as(content_type),
                Err(ReceiveError::UnsupportedContentType),
                "content type {content_type:?}"
            );
        }

        // No `Content-Type` header at all is refused the same way.
        let signature = verifier().sign(b"");
        let no_content_type = HeaderView::new()
            .signature(&signature)
            .delivery_id("delivery")
            .event_name("push");
        assert_eq!(
            Envelope::from_signed(&verifier(), &no_content_type, Bytes::new()),
            Err(ReceiveError::UnsupportedContentType)
        );
    }

    #[test]
    fn an_empty_required_header_is_missing() {
        // A header sent with no value is as good as not sent: the error names
        // the header, so no envelope is built with an empty delivery ID or an
        // event name that parses as `Unknown("")`.
        let signature = verifier().sign(b"");

        assert_eq!(
            Envelope::from_signed(
                &verifier(),
                &headers(&signature).delivery_id(""),
                Bytes::new()
            ),
            Err(ReceiveError::MissingHeader {
                name: header::DELIVERY_ID
            })
        );
        assert_eq!(
            Envelope::from_signed(
                &verifier(),
                &headers(&signature).event_name(""),
                Bytes::new()
            ),
            Err(ReceiveError::MissingHeader {
                name: header::EVENT_NAME
            })
        );
    }

    #[test]
    fn requires_the_event_name() {
        let signature = verifier().sign(b"");
        let no_event_name = HeaderView::new()
            .signature(&signature)
            .delivery_id("delivery")
            .content_type("application/json");

        assert_eq!(
            Envelope::from_signed(&verifier(), &no_event_name, Bytes::new()),
            Err(ReceiveError::MissingHeader {
                name: header::EVENT_NAME
            })
        );
        assert_eq!(
            ReceiveError::MissingHeader {
                name: header::EVENT_NAME
            }
            .to_string(),
            "missing x-github-event header"
        );
    }
}

/// The probe: best-effort and never fatal, the same on the receiving path
/// and in `Envelope::new`, and reading what the corpus fixtures carry.
mod probe {
    use bytes::Bytes;

    use super::{BODY, headers, verifier};
    use crate::{Action, Envelope, EventKind, EventMeta, RepositoryRef, test_support};

    #[test]
    fn invalid_json_is_preserved_without_failing_the_envelope() {
        let envelope = Envelope::new("delivery", EventKind::PullRequest, b"not json");

        assert_eq!(
            envelope.meta,
            EventMeta::new("delivery", EventKind::PullRequest)
        );
        assert_eq!(envelope.raw_payload, Bytes::from_static(b"not json"));
    }

    #[test]
    fn a_synthetic_envelope_carries_what_the_receiver_would_have_read() {
        // The test path and the receiving path probe the same payload the
        // same way; only the header-derived target differs, since `new` has
        // no headers to read it from.
        let signature = verifier().sign(BODY);
        let signed =
            Envelope::from_signed(&verifier(), &headers(&signature), Bytes::from_static(BODY))
                .unwrap();

        let synthetic = Envelope::new("delivery", EventKind::PullRequest, BODY);

        let mut expected = signed.meta.clone();
        expected.target_type = None;
        expected.target_id = None;
        assert_eq!(synthetic.meta, expected);
        assert_eq!(synthetic.raw_payload, signed.raw_payload);

        // What the payload carried is now in the meta, not hand-assigned.
        assert_eq!(synthetic.meta.action, Some(Action::Opened));
        assert_eq!(synthetic.meta.installation_id, Some(42));
        assert_eq!(
            synthetic.meta.repository,
            Some(RepositoryRef::new(1, "repo", "octo/repo", "octo"))
        );
        assert_eq!(synthetic.meta.organization.as_deref(), Some("github"));
        assert_eq!(synthetic.meta.sender.as_deref(), Some("monalisa"));
    }

    #[test]
    fn the_probe_reads_the_action_and_installation_of_every_corpus_fixture() {
        // Real payloads, not the synthetic `BODY`: what GitHub sends is what
        // the probe must read. The ping carries neither field.
        let corpus = [
            (
                test_support::pull_request_opened(),
                Some(Action::Opened),
                Some(7_777_777),
            ),
            (
                test_support::check_run_completed(),
                Some(Action::Completed),
                None,
            ),
            (
                test_support::installation_created(),
                Some(Action::Created),
                Some(39_593_433),
            ),
            (
                test_support::installation_repositories_removed(),
                Some(Action::Removed),
                Some(7_777_777),
            ),
            (test_support::ping(), None, None),
        ];

        for (envelope, action, installation_id) in corpus {
            let meta = &envelope.meta;
            assert_eq!(meta.action, action, "{}", meta.kind);
            assert_eq!(meta.installation_id, installation_id, "{}", meta.kind);
        }
    }

    #[test]
    fn malformed_probe_fields_do_not_discard_valid_siblings() {
        // `repository` lacks `full_name`, so it alone reads as absent.
        let envelope = Envelope::new(
            "delivery",
            EventKind::PullRequest,
            br#"{
                "action":"opened",
                "installation":{"id":42},
                "repository":{"id":1,"name":"repo","owner":{"login":"octo"}},
                "sender":{"login":"monalisa"}
            }"#,
        );

        assert_eq!(envelope.meta.action, Some(Action::Opened));
        assert_eq!(envelope.meta.installation_id, Some(42));
        assert_eq!(envelope.meta.sender.as_deref(), Some("monalisa"));
        assert_eq!(envelope.meta.repository, None);
    }
}

/// The meta as a value: `RepositoryRef`'s constructor, and `EventMeta` as
/// a set member by value.
mod meta {
    use std::collections::HashSet;

    use super::BODY;
    use crate::{Envelope, EventKind, EventMeta, RepositoryRef};

    #[test]
    fn a_repository_ref_is_built_from_its_constructor() {
        let repository = RepositoryRef::new(1, "repo", "octo/repo", "octo");

        assert_eq!(repository.id, 1);
        assert_eq!(repository.name, "repo");
        assert_eq!(repository.full_name, "octo/repo");
        assert_eq!(repository.owner, "octo");
    }

    #[test]
    fn a_meta_is_a_set_member_by_value() {
        // `Hash` agrees with `Eq`: two metas read from the same payload are
        // one key, so a policy that remembers what it saw needs no key of
        // its own.
        let first = Envelope::new("delivery", EventKind::PullRequest, BODY).meta;
        let again = Envelope::new("delivery", EventKind::PullRequest, BODY).meta;
        let other = Envelope::new("other", EventKind::PullRequest, BODY).meta;

        let seen: HashSet<EventMeta> = [first, again, other].into_iter().collect();

        assert_eq!(seen.len(), 2);
        assert!(seen.contains(&Envelope::new("delivery", EventKind::PullRequest, BODY).meta));
    }
}

/// The wire format a forwarded envelope takes: one flat object, the raw
/// payload as base64, absent fields omitted on the way out and tolerated
/// either way on the way back in.
mod wire_format {
    use bytes::Bytes;

    use super::{BODY, headers, verifier};
    use crate::{Envelope, EventKind, EventMeta, TargetType};

    /// The top-level keys of a serialized envelope, sorted for comparison.
    fn sorted_keys(value: &serde_json::Value) -> Vec<&str> {
        let mut keys: Vec<&str> = value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        keys
    }

    #[test]
    fn serializes_the_metadata_flat_beside_the_raw_payload() {
        let signature = verifier().sign(BODY);
        let envelope =
            Envelope::from_signed(&verifier(), &headers(&signature), Bytes::from_static(BODY))
                .unwrap();

        let value = serde_json::to_value(envelope).unwrap();

        // The meta/raw_payload split is a Rust-side composition only: on the
        // wire the metadata sits at the top level with no `meta` nesting.
        assert_eq!(
            sorted_keys(&value),
            [
                "action",
                "delivery_id",
                "installation_id",
                "kind",
                "organization",
                "raw_payload",
                "repository",
                "sender",
                "target_id",
                "target_type",
            ]
        );
        assert_eq!(value["delivery_id"], "delivery");
        assert_eq!(value["kind"], "pull_request");
        assert_eq!(value["installation_id"], 42);
        assert_eq!(value["repository"]["full_name"], "octo/repo");
    }

    #[test]
    fn serializes_the_raw_payload_as_base64() {
        let verifier = verifier();
        let signature = verifier.sign(BODY);
        let envelope =
            Envelope::from_signed(&verifier, &headers(&signature), Bytes::from_static(BODY))
                .unwrap();

        let value = serde_json::to_value(envelope).unwrap();

        assert_eq!(
            value["raw_payload"],
            base64::Engine::encode(&base64::engine::general_purpose::STANDARD, BODY)
        );
        assert_eq!(value["target_type"], "repository");
    }

    #[test]
    fn survives_a_json_round_trip_to_a_forwarding_target() {
        let signature = verifier().sign(BODY);
        let envelope =
            Envelope::from_signed(&verifier(), &headers(&signature), Bytes::from_static(BODY))
                .unwrap();

        let forwarded = serde_json::to_string(&envelope).unwrap();
        let received: Envelope = serde_json::from_str(&forwarded).unwrap();

        assert_eq!(received, envelope);
        assert_eq!(received.raw_payload, Bytes::from_static(BODY));
        assert_eq!(received.meta.target_type, Some(TargetType::Repository));
    }

    #[test]
    fn omits_absent_optional_fields_from_the_serialized_envelope() {
        // A forwarded envelope says what it knows and nothing else, so a
        // consumer in another language reads a missing key, not a null.
        let envelope = Envelope::new("delivery", EventKind::Push, b"{}");

        let value = serde_json::to_value(envelope).unwrap();

        assert_eq!(sorted_keys(&value), ["delivery_id", "kind", "raw_payload"]);
    }

    #[test]
    fn deserializes_null_and_absent_optional_fields_alike() {
        // Both spellings of "unknown" that a producer might use read back as
        // the same envelope, so a hand-written forwarder need not pick one.
        let with_nulls = r#"{
            "delivery_id": "delivery",
            "kind": "push",
            "action": null,
            "installation_id": null,
            "repository": null,
            "organization": null,
            "sender": null,
            "target_type": null,
            "target_id": null,
            "raw_payload": "e30="
        }"#;
        let without = r#"{"delivery_id": "delivery", "kind": "push", "raw_payload": "e30="}"#;

        let from_nulls: Envelope = serde_json::from_str(with_nulls).unwrap();
        let from_absent: Envelope = serde_json::from_str(without).unwrap();

        assert_eq!(from_nulls, from_absent);
        assert_eq!(
            from_absent,
            Envelope::new("delivery", EventKind::Push, b"{}")
        );
    }

    #[test]
    fn requires_delivery_id_kind_and_raw_payload_on_deserialize() {
        // The three fields the docs name as required are the three whose
        // absence is an error; every other field defaults.
        for missing in ["delivery_id", "kind", "raw_payload"] {
            let mut document = serde_json::json!({"delivery_id": "delivery", "kind": "push", "raw_payload": "e30="});
            document.as_object_mut().unwrap().remove(missing);

            let error = serde_json::from_value::<Envelope>(document).unwrap_err();

            assert!(
                error.to_string().contains(missing),
                "removing {missing} should name it: {error}"
            );
        }
    }

    #[test]
    fn ignores_unknown_fields_on_deserialize() {
        // A producer on a newer version, or one that annotates the document
        // for its own transport, does not break an older consumer.
        let document = r#"{
            "delivery_id": "delivery",
            "kind": "push",
            "raw_payload": "e30=",
            "enterprise": {"id": 1},
            "received_at": "2026-09-06T00:00:00Z"
        }"#;

        let envelope: Envelope = serde_json::from_str(document).unwrap();

        assert_eq!(envelope.meta, EventMeta::new("delivery", EventKind::Push));
        assert_eq!(envelope.raw_payload, Bytes::from_static(b"{}"));
    }
}

/// The kind-free `decode` and what a `DecodeError` says.
mod decode {
    use crate::{DecodeError, EventKind, test_support};

    /// A view over an `issues` payload. Not a `Payload`: `decode` ties
    /// nothing to the kind, so the view declares none; the kind-checked
    /// decode is `decode_payload`'s, tested where it lives, in `payload`.
    #[derive(Debug, serde::Deserialize)]
    struct IssueNumber {
        issue: Numbered,
    }

    #[derive(Debug, serde::Deserialize)]
    struct Numbered {
        number: u64,
    }

    #[test]
    fn kind_mismatch_displays_the_kinds_as_wire_strings_with_the_article_each_takes() {
        // The kind's wire string is quoted, and the article agrees with it:
        // no "a issues event", and no "an push event" either.
        let issues = DecodeError::KindMismatch {
            expected: EventKind::Issues,
            actual: EventKind::PullRequest,
        };
        let push = DecodeError::KindMismatch {
            expected: EventKind::Push,
            actual: EventKind::Issues,
        };

        assert_eq!(
            issues.to_string(),
            "expected an `issues` event, received `pull_request`"
        );
        assert_eq!(
            push.to_string(),
            "expected a `push` event, received `issues`"
        );
    }

    #[test]
    fn decode_reports_a_payload_that_does_not_fit_with_the_shared_decode_error() {
        let envelope = test_support::envelope(EventKind::Issues, br#"{"issue":{}}"#);

        let error = envelope.decode::<IssueNumber>().unwrap_err();

        assert!(matches!(error, DecodeError::Json(_)));
    }

    #[test]
    fn decode_does_not_check_the_kind() {
        // A view of an `issues` payload decodes from a `pull_request`
        // envelope: `decode` ties nothing to the kind.
        let envelope = test_support::envelope(EventKind::PullRequest, br#"{"issue":{"number":7}}"#);

        let payload = envelope.decode::<IssueNumber>().unwrap();

        assert_eq!(payload.issue.number, 7);
    }

    #[test]
    fn an_input_error_displays_the_consumers_message_and_carries_the_source_it_was_given() {
        use std::error::Error as _;

        // A plain message: the consumer's words are the whole reason.
        let error = DecodeError::input("payload has no installation");
        assert_eq!(error.to_string(), "payload has no installation");
        assert!(error.source().is_none());

        // With an underlying error: the message is still what `Display`
        // shows, and the cause is one `source()` hop down, as it is for
        // `Json`.
        let cause = "forty-two".parse::<u64>().unwrap_err();
        let error =
            DecodeError::input_with_source("installation id is not a number", cause.clone());
        assert_eq!(error.to_string(), "installation id is not a number");
        assert_eq!(
            error
                .source()
                .and_then(|source| source.downcast_ref::<std::num::ParseIntError>()),
            Some(&cause)
        );
    }
}

/// The `HeaderView` constructors a transport reaches for, `from_lookup` and
/// the `http::HeaderMap` conversion, and its redacting `Debug`.
mod header_view {
    use std::collections::HashMap;

    use bytes::Bytes;

    use super::{BODY, verifier};
    #[cfg(feature = "http")]
    use crate::header;
    use crate::{Action, Envelope, EventKind, HeaderView, ReceiveError, TargetType, VerifyError};

    #[test]
    fn from_lookup_authenticates_a_string_map_the_caller_lowercased() {
        // HTTP/1.1 hands a serverless runtime the names as GitHub wrote them.
        // The constants are lowercase and the lookup compares nothing itself,
        // so the transport lowercases its keys before asking.
        let signature = verifier().sign(BODY);
        let received: HashMap<String, String> = [
            ("X-Hub-Signature-256", signature.as_str()),
            ("X-GitHub-Delivery", "delivery"),
            ("X-GitHub-Event", "pull_request"),
            ("Content-Type", "application/json"),
            ("X-GitHub-Hook-Installation-Target-Type", "integration"),
            ("X-GitHub-Hook-Installation-Target-ID", "12345"),
        ]
        .into_iter()
        .map(|(name, value)| (name.to_ascii_lowercase(), value.to_owned()))
        .collect();

        let headers = HeaderView::from_lookup(|name| received.get(name).map(String::as_str));
        let envelope =
            Envelope::from_signed(&verifier(), &headers, Bytes::from_static(BODY)).unwrap();

        assert_eq!(envelope.meta.delivery_id, "delivery");
        assert_eq!(envelope.meta.kind, EventKind::PullRequest);
        assert_eq!(envelope.meta.action, Some(Action::Opened));
        assert_eq!(envelope.meta.target_type, Some(TargetType::Integration));
        assert_eq!(envelope.meta.target_id, Some(12345));
        assert_eq!(envelope.meta.installation_id, Some(42));
    }

    #[test]
    fn from_lookup_reports_an_absent_signature_as_missing() {
        // A string map cannot hold a header whose bytes are not a string, so
        // the only header failure this path can earn for the signature is its
        // absence. The lookup hands over owned values here: either flavour of
        // string is accepted, as the setters accept both.
        let received: HashMap<String, String> = [
            ("x-github-delivery", "delivery"),
            ("x-github-event", "push"),
            ("content-type", "application/json"),
        ]
        .into_iter()
        .map(|(name, value)| (name.to_owned(), value.to_owned()))
        .collect();

        let headers = HeaderView::from_lookup(|name| received.get(name).cloned());

        assert_eq!(
            Envelope::from_signed(&verifier(), &headers, Bytes::new()),
            Err(ReceiveError::Verify(VerifyError::MissingSignature))
        );
    }

    #[cfg(feature = "http")]
    #[test]
    fn constructs_from_an_http_header_map() {
        let signature = verifier().sign(BODY);
        let mut map = http::HeaderMap::new();
        map.insert(header::SIGNATURE, signature.parse().unwrap());
        map.insert(header::DELIVERY_ID, "delivery".parse().unwrap());
        map.insert(header::EVENT_NAME, "pull_request".parse().unwrap());
        map.insert(header::CONTENT_TYPE, "application/json".parse().unwrap());
        map.insert(header::TARGET_TYPE, "integration".parse().unwrap());
        map.insert(header::TARGET_ID, "12345".parse().unwrap());

        let envelope = Envelope::from_signed(
            &verifier(),
            &HeaderView::from(&map),
            Bytes::from_static(BODY),
        )
        .unwrap();

        assert_eq!(envelope.meta.delivery_id, "delivery");
        assert_eq!(envelope.meta.kind, EventKind::PullRequest);
        assert_eq!(envelope.meta.action, Some(Action::Opened));
        assert_eq!(envelope.meta.target_type, Some(TargetType::Integration));
        assert_eq!(envelope.meta.target_id, Some(12345));
        assert_eq!(envelope.meta.installation_id, Some(42));
    }

    #[cfg(feature = "http")]
    #[test]
    fn rejects_a_non_ascii_signature_header_as_malformed() {
        let mut map = http::HeaderMap::new();
        map.insert(
            "x-hub-signature-256",
            http::HeaderValue::from_bytes(b"sha256=\xff\xfe").unwrap(),
        );
        map.insert("x-github-delivery", "delivery".parse().unwrap());
        map.insert("x-github-event", "push".parse().unwrap());
        map.insert("content-type", "application/json".parse().unwrap());

        assert_eq!(
            Envelope::from_signed(&verifier(), &HeaderView::from(&map), Bytes::new()),
            Err(ReceiveError::Verify(VerifyError::MalformedSignature))
        );
    }

    #[test]
    fn header_debug_output_redacts_the_signature() {
        let headers = HeaderView::new()
            .signature("sha256=secret-value")
            .delivery_id("delivery")
            .event_name("push")
            .content_type("application/json");

        let debug = format!("{headers:?}");
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains("secret-value"));
    }
}
