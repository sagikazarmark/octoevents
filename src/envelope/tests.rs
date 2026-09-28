//! The envelope's tests, beside the production code so they share the
//! crate's fixtures. Grouped by concern: the receive errors, the data
//! constructor, the meta decode, the meta as a value, the wire format, and
//! `decode`. The authenticating path's own tests are beside it, in
//! `authenticate`.
//!
//! A signed envelope here comes from [`authenticate`](crate::authenticate)
//! over a request [`verifier`](crate::test_support::verifier) signed;
//! [`headers`](crate::test_support::headers) is the well-formed header set
//! of a `pull_request` delivery, for a test to start from.

/// The receive errors: the `http::StatusCode` each is answered with, the
/// response contract, and what each carries.
mod status {
    use http::StatusCode;

    use crate::{BodyError, ReceiveError, SignatureError, header};

    #[test]
    fn a_body_read_failure_carries_the_transports_error_as_its_source() {
        // The message says what the receiver could not do; the transport's
        // own text is one `source()` hop down, where whoever holds the value
        // (a transport built on `authenticate`, an error chain walker) finds
        // it without the variant naming the transport's type.
        let error = ReceiveError::BodyRead(BodyError::new("connection reset by peer"));

        assert_eq!(error.to_string(), "could not read the webhook body");
        assert_eq!(
            std::error::Error::source(&error).map(ToString::to_string),
            Some("connection reset by peer".to_string())
        );
    }

    #[test]
    fn maps_every_receive_error_to_the_status_the_contract_names() {
        // The whole table: an absent or mismatched signature is the client's
        // authentication failing (401); a signature that is not `sha256=` and
        // 64 hex characters, a missing required header, a form-encoded body
        // and a body frame the transport could not produce are malformed
        // requests (400); the body limit is its own code (413). One row per
        // `ReceiveError` shape the match has an arm for. Which failure a
        // request earns is `authenticate`'s test; that the receiver
        // answers with the mapped status is its own.
        let table = [
            (
                ReceiveError::Signature(SignatureError::Missing),
                StatusCode::UNAUTHORIZED,
            ),
            (
                ReceiveError::Signature(SignatureError::Mismatch),
                StatusCode::UNAUTHORIZED,
            ),
            (
                ReceiveError::Signature(SignatureError::Malformed),
                StatusCode::BAD_REQUEST,
            ),
            (
                ReceiveError::MissingHeader {
                    name: header::DELIVERY_ID,
                },
                StatusCode::BAD_REQUEST,
            ),
            (
                ReceiveError::UnsupportedContentType,
                StatusCode::BAD_REQUEST,
            ),
            (
                ReceiveError::BodyRead(BodyError::new("connection reset by peer")),
                StatusCode::BAD_REQUEST,
            ),
            (
                ReceiveError::BodyTooLarge { limit: 1 },
                StatusCode::PAYLOAD_TOO_LARGE,
            ),
        ];

        for (error, status) in table {
            assert_eq!(error.status(), status, "{error:?}");
        }
    }
}

/// The data constructor, `Envelope::new`: the header meta and the bytes as
/// given, nothing read and nothing verified.
mod new {
    use bytes::Bytes;

    use crate::test_support::{BODY, headers, verifier};
    use crate::{Envelope, EventKind, TargetType, WebhookMeta, authenticate};

    #[test]
    fn carries_the_webhook_meta_and_the_bytes_it_was_given_untouched() {
        let mut meta = WebhookMeta::new("delivery", EventKind::PullRequest);
        meta.target_type = Some(TargetType::Integration);
        meta.target_id = Some(12345);

        // Any bytes: a payload GitHub sends, and bytes no decode would read.
        for payload in [BODY, b"not json", b"[]", b"", b"\xff\xfe"] {
            let envelope = Envelope::new(meta.clone(), payload);

            assert_eq!(envelope.meta, meta);
            assert_eq!(envelope.raw_payload, Bytes::copy_from_slice(payload));
        }
    }

    #[test]
    fn builds_the_envelope_authenticate_would_from_the_same_headers_and_bytes() {
        // A transport that authenticated the request by its own means reads
        // the header meta and builds the same envelope, target included.
        let signature = verifier().sign(BODY).to_string();
        let headers = headers(&signature);
        let authenticated = authenticate(&verifier(), &headers, Bytes::from_static(BODY)).unwrap();

        let built = Envelope::new(WebhookMeta::from_headers(&headers).unwrap(), BODY);

        assert_eq!(built, authenticated);
        assert_eq!(built.meta.target_type, Some(TargetType::Repository));
        assert_eq!(built.meta.target_id, Some(7));
    }

    #[test]
    fn verifies_nothing() {
        // Nothing is signed and nothing is checked: bytes no verifier would
        // accept still build an envelope, which is why one proves nothing.
        let envelope = Envelope::new(WebhookMeta::new("delivery", EventKind::Push), BODY);

        assert_eq!(envelope.meta.kind, EventKind::Push);
        assert_eq!(envelope.raw_payload, Bytes::from_static(BODY));
    }
}

/// `EventMeta::decode`: the header meta beside what the payload says, decoded
/// strictly to GitHub's shape, reading what the corpus fixtures carry.
mod meta_decode {
    use crate::test_support::{self, BODY};
    use crate::{
        AccountMeta, Action, DecodeError, Envelope, EventKind, EventMeta, RepositoryMeta,
        TargetType, WebhookMeta,
    };

    fn decode(payload: &[u8]) -> Result<EventMeta, DecodeError> {
        EventMeta::decode(&test_support::envelope(EventKind::PullRequest, payload))
    }

    #[test]
    fn carries_the_webhook_meta_beside_the_payloads_fields() {
        let mut webhook = WebhookMeta::new("delivery", EventKind::PullRequest);
        webhook.target_type = Some(TargetType::Integration);
        webhook.target_id = Some(12345);

        let meta = EventMeta::decode(&Envelope::new(webhook, BODY)).unwrap();

        let mut expected = EventMeta::new("delivery", EventKind::PullRequest);
        expected.action = Some(Action::Opened);
        expected.installation_id = Some(42);
        expected.repository = Some(RepositoryMeta::new(1, "repo", "octo/repo", "octo"));
        expected.organization = Some(AccountMeta::new(9919, "github"));
        expected.sender = Some(AccountMeta::new(2, "monalisa"));
        expected.target_type = Some(TargetType::Integration);
        expected.target_id = Some(12345);
        assert_eq!(meta, expected);
    }

    #[test]
    fn every_corpus_fixture_decodes_to_the_meta_it_carries() {
        // Real payloads, not the synthetic `BODY`: what GitHub sends is what
        // the decode must read, the accounts out of GitHub's full account
        // objects with the fields the meta does not keep ignored.
        let gagbo = || Some(AccountMeta::new(10_496_163, "gagbo"));
        let corpus = [
            (
                test_support::pull_request_opened(),
                Some(Action::Opened),
                Some(7_777_777),
                Some(RepositoryMeta::new(
                    537_482_687,
                    "ouro-closures",
                    "gagbo/ouro-closures",
                    "gagbo",
                )),
                gagbo(),
            ),
            (
                test_support::check_run_completed(),
                Some(Action::Completed),
                None,
                Some(RepositoryMeta::new(
                    186_853_002,
                    "Hello-World",
                    "Codertocat/Hello-World",
                    "Codertocat",
                )),
                Some(AccountMeta::new(21_031_067, "Codertocat")),
            ),
            (
                test_support::installation_created(),
                Some(Action::Created),
                Some(39_593_433),
                None,
                gagbo(),
            ),
            (
                test_support::installation_repositories_removed(),
                Some(Action::Removed),
                Some(7_777_777),
                None,
                gagbo(),
            ),
            (test_support::ping(), None, None, None, None),
            (test_support::unknown(), None, None, None, None),
            (test_support::unrepresentable(), None, None, None, None),
        ];

        for (envelope, action, installation_id, repository, sender) in corpus {
            let meta = EventMeta::decode(&envelope).unwrap();

            let mut expected = EventMeta::new("delivery", envelope.meta.kind.clone());
            expected.action = action;
            expected.installation_id = installation_id;
            expected.repository = repository;
            expected.sender = sender;
            assert_eq!(meta, expected, "{}", envelope.meta.kind);
        }
    }

    #[test]
    fn null_fields_decode_as_absent() {
        let meta = decode(
            br#"{"action":null,"installation":null,"repository":null,"organization":null,"sender":null}"#,
        )
        .unwrap();

        assert_eq!(meta, EventMeta::new("delivery", EventKind::PullRequest));
    }

    #[test]
    fn a_payload_missing_a_required_field_is_an_error() {
        let complete: serde_json::Value = serde_json::from_slice(BODY).unwrap();
        for pointer in [
            "/installation/id",
            "/repository/id",
            "/repository/name",
            "/repository/full_name",
            "/repository/owner",
            "/repository/owner/login",
            "/organization/id",
            "/organization/login",
            "/sender/id",
            "/sender/login",
        ] {
            let mut payload = complete.clone();
            let (parent, field) = pointer.rsplit_once('/').unwrap();
            payload
                .pointer_mut(parent)
                .and_then(serde_json::Value::as_object_mut)
                .unwrap()
                .remove(field)
                .unwrap();

            let error = decode(payload.to_string().as_bytes()).unwrap_err();

            assert!(
                matches!(error, DecodeError::Json(_)),
                "{pointer}: {error:?}"
            );
        }
    }

    #[test]
    fn a_field_outside_githubs_shape_is_an_error() {
        for (field, value) in [
            ("action", "42"),
            ("installation", "[42]"),
            ("repository", r#"[1,"repo","octo/repo",{"login":"octo"}]"#),
            (
                "repository",
                r#"{"id":1,"name":"repo","full_name":"octo/repo","owner":["octo"]}"#,
            ),
            ("organization", r#"[9919,"github"]"#),
            ("sender", r#"[2,"monalisa"]"#),
            ("installation", r#"{"id":"42"}"#),
            ("repository", r#""octo/repo""#),
            ("sender", r#"{"id":2,"login":null}"#),
        ] {
            let mut payload: serde_json::Value = serde_json::from_slice(BODY).unwrap();
            payload[field] = serde_json::from_str(value).unwrap();

            let error = decode(payload.to_string().as_bytes()).unwrap_err();

            assert!(matches!(error, DecodeError::Json(_)), "{field}: {error:?}");
        }
    }

    #[test]
    fn a_repeated_meta_key_is_an_error() {
        for payload in [
            r#"{"action":"opened","action":"closed"}"#,
            r#"{"sender":{"id":2,"login":"monalisa","login":"octo"}}"#,
        ] {
            assert!(
                matches!(decode(payload.as_bytes()), Err(DecodeError::Json(_))),
                "{payload}"
            );
        }
    }

    #[test]
    fn invalid_utf8_in_a_value_the_meta_skips_is_not_validated() {
        // The decode is one serde pass that skips what it does not keep, and
        // serde_json does not validate the UTF-8 of a skipped string: the
        // meta decodes, and only a view that reads the value fails on it.
        let fields = &BODY[..BODY.len() - 1];
        for extra in [
            &b",\"extra\":\"\xff\"}"[..],
            b",\"extra\":{\"nested\":[\"\x80\"]}}",
        ] {
            let payload = [fields, extra].concat();

            let meta = decode(&payload).unwrap();

            assert_eq!(meta.action, Some(Action::Opened));
            assert_eq!(meta.sender, Some(AccountMeta::new(2, "monalisa")));
        }
    }

    #[test]
    fn invalid_json_is_an_error() {
        for payload in [&b"not json"[..], b"{", b"", b"{\"action\":\"\xff\"}"] {
            assert!(
                matches!(decode(payload), Err(DecodeError::Json(_))),
                "{}",
                String::from_utf8_lossy(payload)
            );
        }
    }

    #[test]
    fn a_non_object_top_level_is_an_error() {
        // Serde's derived structs also read a positional array; the meta is
        // an object or nothing.
        for payload in [
            "[]",
            "[null,null,null,null,null]",
            r#"["opened",{"id":42},null,null,null]"#,
            "null",
            "true",
            "42",
            r#""opened""#,
        ] {
            assert!(
                matches!(decode(payload.as_bytes()), Err(DecodeError::Json(_))),
                "{payload}"
            );
        }
    }
}

/// The meta as a value: the `RepositoryMeta` and `AccountMeta` constructors,
/// and `EventMeta` as a set member by value.
mod meta {
    use std::collections::HashSet;

    use crate::test_support::BODY;
    use crate::{
        AccountMeta, Envelope, EventKind, EventMeta, RepositoryMeta, WebhookMeta, test_support,
    };

    #[test]
    fn a_repository_meta_is_built_from_its_constructor() {
        let repository = RepositoryMeta::new(1, "repo", "octo/repo", "octo");

        assert_eq!(repository.id, 1);
        assert_eq!(repository.name, "repo");
        assert_eq!(repository.full_name, "octo/repo");
        assert_eq!(repository.owner, "octo");
    }

    #[test]
    fn an_account_meta_is_built_from_its_constructor_and_displays_as_its_login() {
        let account = AccountMeta::new(583_231, "octocat");

        assert_eq!(account.id, 583_231);
        assert_eq!(account.login, "octocat");
        assert_eq!(account.to_string(), "octocat");
    }

    #[test]
    fn a_meta_is_a_set_member_by_value() {
        // `Hash` agrees with `Eq`: two metas decoded from the same delivery
        // are one key, so a policy that remembers what it saw needs no key
        // of its own.
        let decode = |envelope: &Envelope| EventMeta::decode(envelope).unwrap();
        let first = decode(&test_support::envelope(EventKind::PullRequest, BODY));
        let again = decode(&test_support::envelope(EventKind::PullRequest, BODY));
        let other = decode(&Envelope::new(
            WebhookMeta::new("other", EventKind::PullRequest),
            BODY,
        ));

        let seen: HashSet<EventMeta> = [first, again, other].into_iter().collect();

        assert_eq!(seen.len(), 2);
        assert!(seen.contains(&decode(&test_support::envelope(
            EventKind::PullRequest,
            BODY
        ))));
    }
}

/// The wire format a forwarded envelope takes: one flat object, the raw
/// payload as base64, absent fields omitted on the way out and tolerated
/// either way on the way back in.
mod wire_format {
    use bytes::Bytes;

    use crate::test_support::{BODY, headers, verifier};
    use crate::{Envelope, EventKind, TargetType, WebhookMeta, authenticate, test_support};

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
    fn serializes_the_header_meta_flat_beside_the_raw_payload() {
        let signature = verifier().sign(BODY).to_string();
        let envelope =
            authenticate(&verifier(), &headers(&signature), Bytes::from_static(BODY)).unwrap();

        let value = serde_json::to_value(envelope).unwrap();

        // The meta/raw_payload split is a Rust-side composition only: on the
        // wire the header meta sits at the top level with no `meta` nesting,
        // and the payload meta is not written at all, though `BODY` carries
        // every field of it.
        assert_eq!(
            sorted_keys(&value),
            [
                "delivery_id",
                "kind",
                "raw_payload",
                "target_id",
                "target_type",
            ]
        );
        assert_eq!(value["delivery_id"], "delivery");
        assert_eq!(value["kind"], "pull_request");
        assert_eq!(value["target_id"], 7);
    }

    #[test]
    fn serializes_the_raw_payload_as_base64() {
        let verifier = verifier();
        let signature = verifier.sign(BODY).to_string();
        let envelope =
            authenticate(&verifier, &headers(&signature), Bytes::from_static(BODY)).unwrap();

        let value = serde_json::to_value(envelope).unwrap();

        assert_eq!(
            value["raw_payload"],
            base64::Engine::encode(&base64::engine::general_purpose::STANDARD, BODY)
        );
        assert_eq!(value["target_type"], "repository");
    }

    #[test]
    fn survives_a_json_round_trip_to_a_forwarding_target() {
        let signature = verifier().sign(BODY).to_string();
        let envelope =
            authenticate(&verifier(), &headers(&signature), Bytes::from_static(BODY)).unwrap();

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
        let envelope = test_support::envelope(EventKind::Push, b"{}");

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
            "target_type": null,
            "target_id": null,
            "raw_payload": "e30="
        }"#;
        let without = r#"{"delivery_id": "delivery", "kind": "push", "raw_payload": "e30="}"#;

        let from_nulls: Envelope = serde_json::from_str(with_nulls).unwrap();
        let from_absent: Envelope = serde_json::from_str(without).unwrap();

        assert_eq!(from_nulls, from_absent);
        assert_eq!(from_absent, test_support::envelope(EventKind::Push, b"{}"));
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

        assert_eq!(envelope.meta, WebhookMeta::new("delivery", EventKind::Push));
        assert_eq!(envelope.raw_payload, Bytes::from_static(b"{}"));
    }

    #[test]
    fn skips_unknown_annotations_without_decoding_their_values() {
        for annotation in [
            "1e400".to_owned(),
            format!("{}0{}", "[".repeat(150), "]".repeat(150)),
            r#""\uD800""#.to_owned(),
        ] {
            let document = format!(
                r#"{{"annotation":{annotation},"delivery_id":"delivery","kind":"push","raw_payload":"e30="}}"#
            );
            let received: Envelope = serde_json::from_str(&document).unwrap();
            assert_eq!(received, test_support::envelope(EventKind::Push, b"{}"));
        }
    }

    #[test]
    fn reads_an_older_document_ignoring_its_payload_meta() {
        // A document from a producer on 0.3 carries the payload meta beside
        // the header fields, here disagreeing with its bytes. The fields are
        // ignored, malformed or not, and the bytes are kept for the
        // dispatcher to decode.
        let document = r#"{
            "delivery_id": "delivery",
            "kind": "issues",
            "action": "closed",
            "installation_id": -1,
            "repository": {"id": 1, "name": "repo", "full_name": "octo/repo", "owner": "octo"},
            "organization": [9919, "github"],
            "sender": {"id": 2, "login": "monalisa"},
            "target_type": "integration",
            "target_id": 12345,
            "raw_payload": "eyJhY3Rpb24iOiJvcGVuZWQifQ=="
        }"#;

        let received: Envelope = serde_json::from_str(document).unwrap();

        let mut expected = WebhookMeta::new("delivery", EventKind::Issues);
        expected.target_type = Some(TargetType::Integration);
        expected.target_id = Some(12345);
        assert_eq!(received.meta, expected);
        assert_eq!(received.raw_payload.as_ref(), br#"{"action":"opened"}"#);
    }

    #[test]
    fn refuses_malformed_and_duplicate_known_wire_fields() {
        for field in [
            r#""target_type": 1"#,
            r#""target_id": "1"#,
            r#""target_id": null, "target_id": 1"#,
            r#""delivery_id": "second""#,
            r#""raw_payload": "e30=""#,
            r#""annotation": [1,]"#,
        ] {
            let document = format!(
                r#"{{"delivery_id":"delivery","kind":"push","raw_payload":"e30=",{field}}}"#
            );
            assert!(
                serde_json::from_str::<Envelope>(&document).is_err(),
                "{field}"
            );
        }
        for document in [
            "null",
            "[]",
            r#"["delivery","push",null,null,null,null,null,null,null,"e30="]"#,
            r#"{"delivery_id":"delivery","kind":"push","raw_payload":"!"}"#,
        ] {
            assert!(
                serde_json::from_str::<Envelope>(document).is_err(),
                "{document}"
            );
        }
    }
}

/// The kind-free `decode` and what a `DecodeError` says.
mod decode {
    use crate::{DecodeError, EventKind, test_support};

    #[test]
    fn a_view_borrows_strings_and_raw_json_from_the_payload() {
        #[derive(serde::Deserialize)]
        struct View<'a> {
            action: &'a str,
            #[serde(borrow)]
            issue: &'a serde_json::value::RawValue,
        }

        let envelope = test_support::envelope(
            EventKind::Issues,
            br#"{"action":"opened","issue":{ "number": 7 }}"#,
        );
        let view: View<'_> = envelope.decode().unwrap();

        assert_eq!(view.action, "opened");
        assert_eq!(view.issue.get(), r#"{ "number": 7 }"#);
        let bytes = envelope.raw_payload.as_ptr_range();
        assert!(bytes.contains(&view.action.as_ptr()));
        assert!(bytes.contains(&view.issue.get().as_ptr()));
    }

    /// A view over an `issues` payload. Not a `Payload`: `decode` ties
    /// nothing to the kind, so the view declares none; the kind-checked
    /// decode is the `Payload`'s `FromEnvelope` impl, tested where it lives,
    /// in `payload`.
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
