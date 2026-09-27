//! The envelope's tests, beside the production code so they share the
//! crate's fixtures. Grouped by concern: the receive errors, the data
//! constructor, the probe, the meta as a value, the wire format, and
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

/// The data constructor, `Envelope::new`: the header meta as given, the
/// probe of the payload, and nothing verified.
mod new {
    use bytes::Bytes;

    use crate::test_support::{BODY, headers, verifier};
    use crate::{
        AccountMeta, Action, Envelope, EventKind, HeaderMeta, RepositoryMeta, TargetType,
        authenticate,
    };

    #[test]
    fn carries_the_header_meta_it_was_given_and_the_probe_of_the_payload() {
        let mut meta = HeaderMeta::new("delivery", EventKind::PullRequest);
        meta.target_type = Some(TargetType::Integration);
        meta.target_id = Some(12345);

        let envelope = Envelope::new(meta, BODY);

        // The header half, target included, as given.
        assert_eq!(envelope.meta.delivery_id, "delivery");
        assert_eq!(envelope.meta.kind, EventKind::PullRequest);
        assert_eq!(envelope.meta.target_type, Some(TargetType::Integration));
        assert_eq!(envelope.meta.target_id, Some(12345));

        // The payload half, read from the bytes, not hand-assigned.
        assert_eq!(envelope.meta.action, Some(Action::Opened));
        assert_eq!(envelope.meta.installation_id, Some(42));
        assert_eq!(
            envelope.meta.repository,
            Some(RepositoryMeta::new(1, "repo", "octo/repo", "octo"))
        );
        assert_eq!(
            envelope.meta.organization,
            Some(AccountMeta::new(9919, "github"))
        );
        assert_eq!(envelope.meta.sender, Some(AccountMeta::new(2, "monalisa")));
        assert_eq!(envelope.raw_payload, Bytes::from_static(BODY));
    }

    #[test]
    fn builds_the_envelope_authenticate_would_from_the_same_headers_and_bytes() {
        // A transport that authenticated the request by its own means reads
        // the header meta and builds the same envelope, target included.
        let signature = verifier().sign(BODY).to_string();
        let headers = headers(&signature);
        let authenticated = authenticate(&verifier(), &headers, Bytes::from_static(BODY)).unwrap();

        let built = Envelope::new(HeaderMeta::from_headers(&headers).unwrap(), BODY);

        assert_eq!(built, authenticated);
        assert_eq!(built.meta.target_type, Some(TargetType::Repository));
        assert_eq!(built.meta.target_id, Some(7));
    }

    #[test]
    fn verifies_nothing() {
        // Nothing is signed and nothing is checked: bytes no verifier would
        // accept still build an envelope, which is why one proves nothing.
        let envelope = Envelope::new(HeaderMeta::new("delivery", EventKind::Push), b"not json");

        assert_eq!(envelope.meta.kind, EventKind::Push);
        assert_eq!(envelope.raw_payload, Bytes::from_static(b"not json"));
    }
}

/// The probe: best-effort and never fatal, the same through `authenticate`
/// and `Envelope::new`, and reading what the corpus fixtures carry.
mod probe {
    use std::fmt::Write as _;

    use bytes::Bytes;

    use crate::test_support::{BODY, headers, verifier};
    use crate::{
        AccountMeta, Action, EventKind, EventMeta, RepositoryMeta, TargetType, authenticate,
        test_support,
    };

    #[test]
    fn invalid_utf8_in_skipped_values_clears_all_payload_meta_and_preserves_the_envelope() {
        for extra in [
            &b",\"extra\":\"\xff\"}"[..],
            &b",\"extra\":{\"nested\":[\"\x80\"]}}"[..],
            &b",\"extra\":\"\xc0\xaf\"}"[..],
            &b",\"extra\":\"\xed\xa0\x80\"}"[..],
            &b",\"extra\":\"\xf0\x9f\"}"[..],
        ] {
            let payload = [&BODY[..BODY.len() - 1], extra].concat();
            let mut expected = EventMeta::new("delivery", EventKind::PullRequest);
            let synthetic = test_support::envelope(EventKind::PullRequest, &payload);
            assert_eq!(synthetic.meta, expected);
            assert_eq!(synthetic.raw_payload.as_ref(), payload);

            let signature = verifier().sign(&payload).to_string();
            let signed = authenticate(
                &verifier(),
                &headers(&signature),
                Bytes::copy_from_slice(&payload),
            )
            .unwrap();
            expected.target_type = Some(TargetType::Repository);
            expected.target_id = Some(7);
            assert_eq!(signed.meta, expected);
            assert_eq!(signed.raw_payload.as_ref(), payload);
        }
    }

    #[test]
    fn non_object_payloads_have_no_probed_metadata() {
        for payload in [
            r#"["opened",{"id":42},null,null,{"id":2,"login":"monalisa"}]"#,
            "[]",
            "null",
            "true",
            "42",
            r#""opened""#,
        ] {
            assert_probed_meta(payload, &EventMeta::new("delivery", EventKind::PullRequest));
        }
    }

    fn assert_probed_meta(payload: &str, expected: &EventMeta) {
        let signature = verifier().sign(payload.as_bytes()).to_string();
        let mut signed_headers = headers(&signature);
        signed_headers.remove("x-github-hook-installation-target-type");
        signed_headers.remove("x-github-hook-installation-target-id");
        let signed = authenticate(
            &verifier(),
            &signed_headers,
            Bytes::copy_from_slice(payload.as_bytes()),
        )
        .unwrap();
        let synthetic = test_support::envelope(EventKind::PullRequest, payload.as_bytes());

        for envelope in [synthetic, signed] {
            assert_eq!(&envelope.meta, expected, "{payload}");
            assert_eq!(
                envelope.raw_payload.as_ref(),
                payload.as_bytes(),
                "{payload}"
            );
        }
    }

    fn complete_meta() -> EventMeta {
        let mut meta = EventMeta::new("delivery", EventKind::PullRequest);
        meta.action = Some(Action::Opened);
        meta.installation_id = Some(42);
        meta.repository = Some(RepositoryMeta::new(1, "repo", "octo/repo", "octo"));
        meta.organization = Some(AccountMeta::new(9919, "github"));
        meta.sender = Some(AccountMeta::new(2, "monalisa"));
        meta
    }

    #[test]
    fn malformed_escapes_in_skipped_values_clear_all_payload_meta() {
        let fields = std::str::from_utf8(BODY).unwrap().trim_end_matches('}');
        let empty = EventMeta::new("delivery", EventKind::PullRequest);
        for value in [r#""\q""#, r#""\u12""#, r#""\uZZZZ""#, "\"line\nbreak\""] {
            for extra in [value.to_owned(), format!("{{\"nested\":[{value}]}}")] {
                assert_probed_meta(&format!("{fields},\"extra\":{extra}}}"), &empty);
            }
        }
    }

    #[test]
    fn escaped_surrogates_in_skipped_values_leave_metadata_intact() {
        let fields = std::str::from_utf8(BODY).unwrap().trim_end_matches('}');
        for value in [r#""\uD83D\uDE00""#, r#""\uD800""#, r#""\uDC00""#] {
            // Escape syntax is checked, but skipped strings need not decode
            // into Unicode scalar values, unlike strings the meta keeps.
            assert_probed_meta(
                &format!("{fields},\"extra\":{{\"nested\":[{value}]}}}}"),
                &complete_meta(),
            );
        }
    }

    #[test]
    fn unpaired_escaped_surrogates_clear_only_the_metadata_field_that_reads_them() {
        let fields = std::str::from_utf8(BODY).unwrap();
        for escape in [r"\uD800", r"\uDC00"] {
            let mut expected = complete_meta();
            expected.sender = None;
            assert_probed_meta(&fields.replace("monalisa", escape), &expected);

            let mut expected = complete_meta();
            expected.action = None;
            assert_probed_meta(&fields.replace("opened", escape), &expected);
        }

        let mut expected = complete_meta();
        expected.sender = Some(AccountMeta::new(2, "😀"));
        assert_probed_meta(&fields.replace("monalisa", r"\uD83D\uDE00"), &expected);
    }

    #[test]
    fn wrong_shaped_metadata_objects_clear_only_their_top_level_field() {
        for (field, array) in [
            ("installation", "[42]"),
            ("repository", r#"[1,"repo","octo/repo",{"login":"octo"}]"#),
            ("owner", r#"["octo"]"#),
            ("organization", r#"[9919,"github"]"#),
            ("sender", r#"[2,"monalisa"]"#),
        ] {
            for shape in [array, "null", "true", "42", r#""object""#] {
                let mut payload: serde_json::Value = serde_json::from_slice(BODY).unwrap();
                let mut expected = complete_meta();
                let value = serde_json::from_str(shape).unwrap();
                match field {
                    "installation" => expected.installation_id = None,
                    "repository" | "owner" => expected.repository = None,
                    "organization" => expected.organization = None,
                    "sender" => expected.sender = None,
                    _ => unreachable!(),
                }
                if field == "owner" {
                    payload["repository"]["owner"] = value;
                } else {
                    payload[field] = value;
                }
                assert_probed_meta(&payload.to_string(), &expected);
            }
        }
    }

    #[test]
    fn duplicate_metadata_keys_clear_only_that_field_even_after_null_or_a_third_value() {
        let fields: serde_json::Value = serde_json::from_slice(BODY).unwrap();
        for field in [
            "action",
            "installation",
            "repository",
            "organization",
            "sender",
        ] {
            let mut expected = complete_meta();
            match field {
                "action" => expected.action = None,
                "installation" => expected.installation_id = None,
                "repository" => expected.repository = None,
                "organization" => expected.organization = None,
                "sender" => expected.sender = None,
                _ => unreachable!(),
            }
            let valid = fields[field].to_string();
            let mut siblings = fields.clone();
            siblings.as_object_mut().unwrap().remove(field);
            let siblings = siblings.to_string();
            let siblings = siblings.trim_start_matches('{');
            for values in [
                vec![valid.as_str(), valid.as_str()],
                vec!["null", valid.as_str()],
                vec![valid.as_str(), "null"],
                vec![valid.as_str(), "null", valid.as_str()],
            ] {
                // Raw JSON keeps duplicate keys that a Value would collapse.
                let mut entries = String::new();
                for value in values {
                    write!(entries, "{field:?}:{value},").unwrap();
                }
                assert_probed_meta(&format!("{{{entries}{siblings}"), &expected);
            }
        }
    }

    #[test]
    fn duplicate_required_keys_in_metadata_objects_clear_only_their_top_level_field() {
        for (field, object) in [
            ("installation", r#"{"id":42,"id":43}"#),
            (
                "repository",
                r#"{"id":1,"id":2,"name":"repo","full_name":"octo/repo","owner":{"login":"octo"}}"#,
            ),
            (
                "repository",
                r#"{"id":1,"name":"repo","name":"other","full_name":"octo/repo","owner":{"login":"octo"}}"#,
            ),
            (
                "repository",
                r#"{"id":1,"name":"repo","full_name":"octo/repo","full_name":"octo/other","owner":{"login":"octo"}}"#,
            ),
            (
                "repository",
                r#"{"id":1,"name":"repo","full_name":"octo/repo","owner":{"login":"octo"},"owner":{"login":"other"}}"#,
            ),
            (
                "repository",
                r#"{"id":1,"name":"repo","full_name":"octo/repo","owner":{"login":"octo","login":"other"}}"#,
            ),
            ("organization", r#"{"id":9919,"id":9920,"login":"github"}"#),
            (
                "organization",
                r#"{"id":9919,"login":"github","login":"other"}"#,
            ),
            ("sender", r#"{"id":2,"id":3,"login":"monalisa"}"#),
            ("sender", r#"{"id":2,"login":"monalisa","login":"other"}"#),
        ] {
            let mut siblings: serde_json::Value = serde_json::from_slice(BODY).unwrap();
            siblings.as_object_mut().unwrap().remove(field);
            let siblings = siblings.to_string();
            let mut expected = complete_meta();
            match field {
                "installation" => expected.installation_id = None,
                "repository" => expected.repository = None,
                "organization" => expected.organization = None,
                "sender" => expected.sender = None,
                _ => unreachable!(),
            }
            let payload = format!("{{{field:?}:{object},{}", siblings.trim_start_matches('{'));
            assert_probed_meta(&payload, &expected);
        }
    }

    #[test]
    fn unknown_duplicate_keys_are_ignored_but_invalid_json_clears_every_field() {
        let mut expected = EventMeta::new("delivery", EventKind::PullRequest);
        expected.action = Some(Action::Opened);
        expected.sender = Some(AccountMeta::new(2, "monalisa"));
        assert_probed_meta(
            r#"{"action":"opened","extra":[],"extra":{},"sender":{"id":2,"login":"monalisa","extra":0,"extra":1}}"#,
            &expected,
        );

        let empty = EventMeta::new("delivery", EventKind::PullRequest);
        for payload in [
            r#"{"action":"opened","extra":[}"#,
            r#"{"action":"opened","sender":{},"sender":[}"#,
            r#"{"action":"opened"} trailing"#,
        ] {
            assert_probed_meta(payload, &empty);
        }
    }

    #[test]
    fn invalid_json_is_preserved_without_failing_the_envelope() {
        let envelope = test_support::envelope(EventKind::PullRequest, b"not json");

        assert_eq!(
            envelope.meta,
            EventMeta::new("delivery", EventKind::PullRequest)
        );
        assert_eq!(envelope.raw_payload, Bytes::from_static(b"not json"));
    }

    #[test]
    fn the_probe_reads_the_action_installation_and_sender_of_every_corpus_fixture() {
        // Real payloads, not the synthetic `BODY`: what GitHub sends is what
        // the probe must read, the sender out of GitHub's full account object
        // with the fields the meta does not keep ignored. The ping carries
        // none of the three.
        let corpus = [
            (
                test_support::pull_request_opened(),
                Some(Action::Opened),
                Some(7_777_777),
                Some(AccountMeta::new(10_496_163, "gagbo")),
            ),
            (
                test_support::check_run_completed(),
                Some(Action::Completed),
                None,
                Some(AccountMeta::new(21_031_067, "Codertocat")),
            ),
            (
                test_support::installation_created(),
                Some(Action::Created),
                Some(39_593_433),
                Some(AccountMeta::new(10_496_163, "gagbo")),
            ),
            (
                test_support::installation_repositories_removed(),
                Some(Action::Removed),
                Some(7_777_777),
                Some(AccountMeta::new(10_496_163, "gagbo")),
            ),
            (test_support::ping(), None, None, None),
        ];

        for (envelope, action, installation_id, sender) in corpus {
            let meta = &envelope.meta;
            assert_eq!(meta.action, action, "{}", meta.kind);
            assert_eq!(meta.installation_id, installation_id, "{}", meta.kind);
            assert_eq!(meta.sender, sender, "{}", meta.kind);
        }
    }

    #[test]
    fn malformed_probe_fields_do_not_discard_valid_siblings() {
        // `repository` lacks `full_name` and `organization` lacks `id`, so
        // each alone reads as absent.
        let envelope = test_support::envelope(
            EventKind::PullRequest,
            br#"{
                "action":"opened",
                "installation":{"id":42},
                "repository":{"id":1,"name":"repo","owner":{"login":"octo"}},
                "organization":{"login":"github"},
                "sender":{"id":2,"login":"monalisa"}
            }"#,
        );

        assert_eq!(envelope.meta.action, Some(Action::Opened));
        assert_eq!(envelope.meta.installation_id, Some(42));
        assert_eq!(envelope.meta.sender, Some(AccountMeta::new(2, "monalisa")));
        assert_eq!(envelope.meta.repository, None);
        assert_eq!(envelope.meta.organization, None);
    }
}

/// The meta as a value: the `RepositoryMeta` and `AccountMeta` constructors,
/// and `EventMeta` as a set member by value.
mod meta {
    use std::collections::HashSet;

    use crate::test_support::BODY;
    use crate::{
        AccountMeta, Envelope, EventKind, EventMeta, HeaderMeta, RepositoryMeta, test_support,
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
        // `Hash` agrees with `Eq`: two metas read from the same payload are
        // one key, so a policy that remembers what it saw needs no key of
        // its own.
        let first = test_support::envelope(EventKind::PullRequest, BODY).meta;
        let again = test_support::envelope(EventKind::PullRequest, BODY).meta;
        let other = Envelope::new(HeaderMeta::new("other", EventKind::PullRequest), BODY).meta;

        let seen: HashSet<EventMeta> = [first, again, other].into_iter().collect();

        assert_eq!(seen.len(), 2);
        assert!(seen.contains(&test_support::envelope(EventKind::PullRequest, BODY).meta));
    }
}

/// The wire format a forwarded envelope takes: one flat object, the raw
/// payload as base64, absent fields omitted on the way out and tolerated
/// either way on the way back in.
mod wire_format {
    use bytes::Bytes;

    use crate::test_support::{BODY, headers, verifier};
    use crate::{Envelope, EventKind, EventMeta, TargetType, authenticate, test_support};

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
        let signature = verifier().sign(BODY).to_string();
        let envelope =
            authenticate(&verifier(), &headers(&signature), Bytes::from_static(BODY)).unwrap();

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

        assert_eq!(envelope.meta, EventMeta::new("delivery", EventKind::Push));
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
    fn preserves_forwarded_meta_without_probing_the_payload() {
        let document = r#"{
            "delivery_id":"delivery", "kind":"issues", "action":"closed",
            "raw_payload":"eyJhY3Rpb24iOiJvcGVuZWQifQ=="
        }"#;
        let received: Envelope = serde_json::from_str(document).unwrap();
        assert_eq!(received.meta.action, Some(crate::Action::Closed));
        assert_eq!(received.raw_payload.as_ref(), br#"{"action":"opened"}"#);
    }

    #[test]
    fn refuses_malformed_and_duplicate_known_wire_fields() {
        for field in [
            r#""action": 42"#,
            r#""installation_id": -1"#,
            r#""repository": {}"#,
            r#""repository": [1, "repo", "octo/repo", "octo"]"#,
            r#""sender": false"#,
            r#""sender": [2, "monalisa"]"#,
            r#""organization": []"#,
            r#""organization": [9919, "github"]"#,
            r#""target_type": 1"#,
            r#""target_id": "1"#,
            r#""action": null, "action": "opened""#,
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
