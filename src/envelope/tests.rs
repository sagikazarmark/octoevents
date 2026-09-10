//! The envelope's tests, beside the production code so they share the
//! crate's fixtures. Grouped by concern: the receiving path through
//! `from_signed`, the status each receive failure is answered with, the
//! header rules it applies, how it reads the `http::HeaderMap`, the probe,
//! the meta as a value, the wire format, and `decode`.
//!
//! Every signed envelope here is checked against [`verifier`], which also
//! signs the bodies it accepts; [`headers`] is the well-formed header set
//! of a `pull_request` delivery, for a test to start from.

use http::{HeaderMap, HeaderValue};

use crate::{Verifier, WebhookSecret, header};

const BODY: &[u8] = br#"{
    "action":"opened",
    "installation":{"id":42},
    "repository":{"id":1,"name":"repo","full_name":"octo/repo","owner":{"login":"octo"}},
    "organization":{"id":9919,"login":"github"},
    "sender":{"id":2,"login":"monalisa"}
}"#;

/// The verifier every signed envelope here is checked against; it also
/// signs the bodies it accepts.
fn verifier() -> Verifier {
    Verifier::new(WebhookSecret::new("secret"))
}

/// The headers of a well-formed `pull_request` delivery carrying
/// `signature`, target included; a test that wants one header wrong
/// overrides or removes it.
fn headers(signature: &str) -> HeaderMap {
    HeaderMap::from_iter([
        (header::SIGNATURE, signature.parse().unwrap()),
        (header::DELIVERY_ID, HeaderValue::from_static("delivery")),
        (header::EVENT_NAME, HeaderValue::from_static("pull_request")),
        (
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json; charset=utf-8"),
        ),
        (header::TARGET_TYPE, HeaderValue::from_static("repository")),
        (header::TARGET_ID, HeaderValue::from_static("7")),
    ])
}

/// A header map from `(name, value)` string pairs, each name parsed into a
/// `HeaderName`.
fn headers_from<const N: usize>(entries: [(&str, &str); N]) -> HeaderMap {
    entries
        .into_iter()
        .map(|(name, value)| (name.parse().unwrap(), value.parse().unwrap()))
        .collect()
}

/// The receiving path, `from_signed`: it verifies, then reads the headers
/// and probes the payload, keeping the bytes exactly as they arrived.
mod receive {
    use bytes::Bytes;

    use super::{BODY, headers, headers_from, verifier};
    use crate::{
        AccountMeta, Action, BodyError, Envelope, EventKind, EventMeta, ReceiveError,
        RepositoryMeta, SignatureError, TargetType,
    };

    #[test]
    fn verifies_then_reads_the_metadata() {
        let verifier = verifier();
        let signature = verifier.sign(BODY).to_string();

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
            Some(RepositoryMeta::new(1, "repo", "octo/repo", "octo"))
        );
        assert_eq!(meta.organization, Some(AccountMeta::new(9919, "github")));
        assert_eq!(meta.sender, Some(AccountMeta::new(2, "monalisa")));
        assert_eq!(meta.target_type, Some(TargetType::Repository));
        assert_eq!(meta.target_id, Some(7));
        assert_eq!(envelope.raw_payload, Bytes::from_static(BODY));
    }

    #[test]
    fn from_signed_reads_the_target_from_the_headers_when_the_payload_yields_nothing() {
        // The target is the one thing the receiving path knows and the test
        // path does not: it comes from headers, not from the payload.
        let body = Bytes::from_static(b"not json");
        let signature = verifier().sign(&body).to_string();

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
        let signature = verifier().sign(&body).to_string();
        let headers = headers_from([
            ("x-hub-signature-256", signature.as_str()),
            ("x-github-delivery", "delivery"),
            ("x-github-event", "brand_new"),
            ("content-type", "application/json"),
        ]);

        let envelope = Envelope::from_signed(&verifier(), &headers, body).unwrap();

        assert_eq!(envelope.meta.kind, EventKind::from("brand_new"));
        assert_eq!(envelope.meta.action, Some(Action::from("brand_new")));
    }

    #[test]
    fn authenticates_before_rejecting_content_type() {
        let headers = headers_from([
            (
                "x-hub-signature-256",
                "sha256=0000000000000000000000000000000000000000000000000000000000000000",
            ),
            ("x-github-delivery", "delivery"),
            ("x-github-event", "push"),
            ("content-type", "application/x-www-form-urlencoded"),
        ]);

        assert_eq!(
            Envelope::from_signed(&verifier(), &headers, Bytes::new()),
            Err(ReceiveError::Signature(SignatureError::Mismatch))
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

        let signature = verifier().sign(UNICODE_BODY).to_string();
        let headers = headers_from([
            ("x-hub-signature-256", signature.as_str()),
            ("x-github-delivery", "delivery"),
            ("x-github-event", "pull_request"),
            ("content-type", "application/json"),
        ]);

        let envelope =
            Envelope::from_signed(&verifier(), &headers, Bytes::from_static(UNICODE_BODY)).unwrap();

        assert_eq!(envelope.raw_payload.as_ref(), UNICODE_BODY);
        assert_eq!(envelope.meta.action, Some(Action::Opened));

        let decoded: serde_json::Value = envelope.decode().unwrap();
        assert_eq!(decoded["zen"], "⚡ é café 🐙");
    }
}

/// The response contract: the `http::StatusCode` each receive failure is
/// answered with.
mod status {
    use http::StatusCode;

    use crate::{BodyError, ReceiveError, SignatureError, header};

    #[test]
    fn maps_every_receive_error_to_the_status_the_contract_names() {
        // The whole table: an absent or mismatched signature is the client's
        // authentication failing (401); a signature that is not `sha256=` and
        // 64 hex characters, a missing required header, a form-encoded body
        // and a body frame the transport could not produce are malformed
        // requests (400); the body limit is its own code (413). One row per
        // `ReceiveError` shape the match has an arm for. Which failure a
        // request earns is `Envelope::from_signed`'s test; that the receiver
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

/// The header rules `from_signed` applies once the signature holds: which
/// headers are required, that an empty one is missing, and which content
/// types are accepted.
mod header_rules {
    use bytes::Bytes;
    use http::HeaderValue;

    use super::{headers, verifier};
    use crate::{Envelope, EventKind, ReceiveError, SignatureError, header};

    /// What the receiving path makes of a signed, otherwise well-formed empty
    /// delivery under `content_type`, reduced to the kind it read.
    fn received_as(content_type: &str) -> Result<EventKind, ReceiveError> {
        let signature = verifier().sign(b"").to_string();
        let mut headers = headers(&signature);
        headers.insert(header::CONTENT_TYPE, content_type.parse().unwrap());

        Envelope::from_signed(&verifier(), &headers, Bytes::new())
            .map(|envelope| envelope.meta.kind)
    }

    #[test]
    fn requires_signature_content_type_and_routing_headers() {
        let signature = verifier().sign(b"").to_string();

        let mut no_signature = headers(&signature);
        no_signature.remove(header::SIGNATURE);
        assert_eq!(
            Envelope::from_signed(&verifier(), &no_signature, Bytes::new()),
            Err(ReceiveError::Signature(SignatureError::Missing))
        );

        let mut form = headers(&signature);
        form.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/x-www-form-urlencoded"),
        );
        assert_eq!(
            Envelope::from_signed(&verifier(), &form, Bytes::new()),
            Err(ReceiveError::UnsupportedContentType)
        );

        let mut no_delivery = headers(&signature);
        no_delivery.remove(header::DELIVERY_ID);
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
        let signature = verifier().sign(b"").to_string();
        let mut no_content_type = headers(&signature);
        no_content_type.remove(header::CONTENT_TYPE);
        assert_eq!(
            Envelope::from_signed(&verifier(), &no_content_type, Bytes::new()),
            Err(ReceiveError::UnsupportedContentType)
        );
    }

    #[test]
    fn an_empty_required_header_is_missing() {
        // A header sent with no value is as good as not sent: the error names
        // the header, so no envelope is built with an empty delivery ID or an
        // event name that parses as `Unknown { value: "" }`.
        let signature = verifier().sign(b"").to_string();

        let mut empty_delivery = headers(&signature);
        empty_delivery.insert(header::DELIVERY_ID, HeaderValue::from_static(""));
        assert_eq!(
            Envelope::from_signed(&verifier(), &empty_delivery, Bytes::new()),
            Err(ReceiveError::MissingHeader {
                name: header::DELIVERY_ID
            })
        );

        let mut empty_event = headers(&signature);
        empty_event.insert(header::EVENT_NAME, HeaderValue::from_static(""));
        assert_eq!(
            Envelope::from_signed(&verifier(), &empty_event, Bytes::new()),
            Err(ReceiveError::MissingHeader {
                name: header::EVENT_NAME
            })
        );
    }

    #[test]
    fn requires_the_event_name() {
        let signature = verifier().sign(b"").to_string();
        let mut no_event_name = headers(&signature);
        no_event_name.remove(header::EVENT_NAME);

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

/// How `from_signed` reads the `http::HeaderMap`: by name, whatever case
/// the names arrived in; the first value of a repeated header; a value that
/// is not visible ASCII as malformed for the signature and as missing for
/// the rest.
mod header_map {
    use bytes::Bytes;
    use http::{HeaderMap, HeaderValue};

    use super::{BODY, headers, headers_from, verifier};
    use crate::{Action, Envelope, EventKind, ReceiveError, SignatureError, TargetType, header};

    #[test]
    fn names_in_githubs_casing_verify_and_route() {
        // What an HTTP/1.1 hop hands a hand-rolled event parser: the names as
        // GitHub wrote them. Parsing each into a `HeaderName` lowercases it,
        // and the map matches case-insensitively, so no casing policy on the
        // way here can turn into a 401.
        let signature = verifier().sign(BODY).to_string();
        let headers = headers_from([
            ("X-Hub-Signature-256", signature.as_str()),
            ("X-GitHub-Delivery", "delivery"),
            ("X-GitHub-Event", "pull_request"),
            ("Content-Type", "application/json"),
            ("X-GitHub-Hook-Installation-Target-Type", "integration"),
            ("X-GitHub-Hook-Installation-Target-ID", "12345"),
        ]);

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
    fn a_signature_value_that_is_not_visible_ascii_is_malformed() {
        // An `http::HeaderValue` need not be a string. The signature is parsed
        // from its bytes, so such a value is present and not a signature
        // (400), never mistaken for an absent header (401): a corrupting hop
        // and a missing header stay distinguishable.
        let mut headers = headers("sha256=00");
        headers.insert(
            header::SIGNATURE,
            HeaderValue::from_bytes(b"sha256=\xff\xfe").unwrap(),
        );

        assert_eq!(
            Envelope::from_signed(&verifier(), &headers, Bytes::new()),
            Err(ReceiveError::Signature(SignatureError::Malformed))
        );
    }

    #[test]
    fn a_signature_value_that_is_a_string_but_not_a_signature_is_malformed() {
        // A value that is present but not `sha256=` and 64 hex digits, here
        // GitHub's legacy SHA-1 header value. Refused from the headers,
        // before any secret is used.
        let headers = headers_from([
            (
                "x-hub-signature-256",
                "sha1=757107ea0eb2509fc211221cce984b8a37570b6d",
            ),
            ("x-github-delivery", "delivery"),
            ("x-github-event", "push"),
            ("content-type", "application/json"),
        ]);

        assert_eq!(
            Envelope::from_signed(&verifier(), &headers, Bytes::new()),
            Err(ReceiveError::Signature(SignatureError::Malformed))
        );
    }

    #[test]
    fn a_delivery_id_value_that_is_not_visible_ascii_is_missing() {
        // GitHub never sends one, so the case adds no error variant: a value
        // the crate cannot read as a string is a value it does not have.
        let signature = verifier().sign(b"").to_string();
        let mut headers = headers(&signature);
        headers.insert(
            header::DELIVERY_ID,
            HeaderValue::from_bytes(b"\xffdelivery").unwrap(),
        );

        assert_eq!(
            Envelope::from_signed(&verifier(), &headers, Bytes::new()),
            Err(ReceiveError::MissingHeader {
                name: header::DELIVERY_ID
            })
        );
    }

    #[test]
    fn a_repeated_header_reads_as_its_first_value() {
        // `HeaderMap::get` answers the first value of a repeated header, so
        // the receiving path does too: the first signature is the one
        // verified, the first delivery ID the one read.
        let signature = verifier().sign(BODY).to_string();
        let mut headers = headers(&signature);
        headers.append(header::SIGNATURE, HeaderValue::from_static("sha256=not-it"));
        headers.append(header::DELIVERY_ID, HeaderValue::from_static("second"));

        let envelope =
            Envelope::from_signed(&verifier(), &headers, Bytes::from_static(BODY)).unwrap();

        assert_eq!(envelope.meta.delivery_id, "delivery");
    }

    #[test]
    fn an_empty_map_is_refused_as_unsigned() {
        assert_eq!(
            Envelope::from_signed(&verifier(), &HeaderMap::new(), Bytes::new()),
            Err(ReceiveError::Signature(SignatureError::Missing))
        );
    }
}

/// The probe: best-effort and never fatal, the same on the receiving path
/// and in `Envelope::new`, and reading what the corpus fixtures carry.
mod probe {
    use std::fmt::Write as _;

    use bytes::Bytes;

    use super::{BODY, headers, verifier};
    use crate::{
        AccountMeta, Action, Envelope, EventKind, EventMeta, RepositoryMeta, TargetType,
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
            let synthetic = Envelope::new("delivery", EventKind::PullRequest, &payload);
            assert_eq!(synthetic.meta, expected);
            assert_eq!(synthetic.raw_payload.as_ref(), payload);

            let signature = verifier().sign(&payload).to_string();
            let signed = Envelope::from_signed(
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
        let signed = Envelope::from_signed(
            &verifier(),
            &signed_headers,
            Bytes::copy_from_slice(payload.as_bytes()),
        )
        .unwrap();
        let synthetic = Envelope::new("delivery", EventKind::PullRequest, payload.as_bytes());

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
        let signature = verifier().sign(BODY).to_string();
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
            Some(RepositoryMeta::new(1, "repo", "octo/repo", "octo"))
        );
        assert_eq!(
            synthetic.meta.organization,
            Some(AccountMeta::new(9919, "github"))
        );
        assert_eq!(synthetic.meta.sender, Some(AccountMeta::new(2, "monalisa")));
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
        let envelope = Envelope::new(
            "delivery",
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

    use super::BODY;
    use crate::{AccountMeta, Envelope, EventKind, EventMeta, RepositoryMeta};

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
        let signature = verifier().sign(BODY).to_string();
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
        let signature = verifier.sign(BODY).to_string();
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
        let signature = verifier().sign(BODY).to_string();
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
            assert_eq!(received, Envelope::new("delivery", EventKind::Push, b"{}"));
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
