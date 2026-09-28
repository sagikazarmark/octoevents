//! The authenticating path's tests, grouped by concern: what it produces,
//! the header rules it applies once the signature holds, and how it reads
//! the `http::HeaderMap`.
//!
//! Every request here is signed by [`verifier`], which the envelope is also
//! checked against; [`headers`] is the well-formed header set of a
//! `pull_request` delivery, for a test to start from.

/// What `authenticate` produces: it verifies, then reads the headers,
/// keeping the bytes exactly as they arrived and reading nothing of them.
mod receive {
    use bytes::Bytes;

    use crate::test_support::{BODY, headers, headers_from, verifier};
    use crate::{
        Action, EventKind, EventMeta, ReceiveError, SignatureError, TargetType, WebhookMeta,
        authenticate,
    };

    #[test]
    fn verifies_then_carries_the_webhook_meta_and_the_bytes_untouched() {
        let verifier = verifier();
        let signature = verifier.sign(BODY).to_string();

        let envelope =
            authenticate(&verifier, &headers(&signature), Bytes::from_static(BODY)).unwrap();

        let mut expected = WebhookMeta::new("delivery", EventKind::PullRequest);
        expected.target_type = Some(TargetType::Repository);
        expected.target_id = Some(7);
        assert_eq!(envelope.meta, expected);
        assert_eq!(envelope.raw_payload, Bytes::from_static(BODY));
    }

    #[test]
    fn builds_an_envelope_over_any_authenticated_bytes() {
        // Nothing of the payload is read, so bytes that are not JSON, or not
        // an object, build an envelope as a payload GitHub sends does; what
        // they say is the dispatcher's to decode.
        for body in [&b"not json"[..], b"[]", b"", b"\xff"] {
            let body = Bytes::from_static(body);
            let signature = verifier().sign(&body).to_string();

            let envelope = authenticate(&verifier(), &headers(&signature), body.clone()).unwrap();

            let mut expected = WebhookMeta::new("delivery", EventKind::PullRequest);
            expected.target_type = Some(TargetType::Repository);
            expected.target_id = Some(7);
            assert_eq!(envelope.meta, expected);
            assert_eq!(envelope.raw_payload, body);
        }
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

        let envelope = authenticate(&verifier(), &headers, body).unwrap();

        assert_eq!(envelope.meta.kind, EventKind::from("brand_new"));
        let meta = EventMeta::decode(&envelope).unwrap();
        assert_eq!(meta.action, Some(Action::from("brand_new")));
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
            authenticate(&verifier(), &headers, Bytes::new()),
            Err(ReceiveError::Signature(SignatureError::Mismatch))
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
            authenticate(&verifier(), &headers, Bytes::from_static(UNICODE_BODY)).unwrap();

        assert_eq!(envelope.raw_payload.as_ref(), UNICODE_BODY);

        let decoded: serde_json::Value = envelope.decode().unwrap();
        assert_eq!(decoded["zen"], "⚡ é café 🐙");
    }
}

/// The header rules `authenticate` applies once the signature holds: which
/// headers are required, that an empty one is missing, and which content
/// types are accepted.
mod header_rules {
    use bytes::Bytes;
    use http::HeaderValue;

    use crate::test_support::{headers, verifier};
    use crate::{EventKind, ReceiveError, SignatureError, authenticate, header};

    /// What the receiving path makes of a signed, otherwise well-formed empty
    /// delivery under `content_type`, reduced to the kind it read.
    fn received_as(content_type: &str) -> Result<EventKind, ReceiveError> {
        let signature = verifier().sign(b"").to_string();
        let mut headers = headers(&signature);
        headers.insert(header::CONTENT_TYPE, content_type.parse().unwrap());

        authenticate(&verifier(), &headers, Bytes::new()).map(|envelope| envelope.meta.kind)
    }

    #[test]
    fn requires_signature_content_type_and_routing_headers() {
        let signature = verifier().sign(b"").to_string();

        let mut no_signature = headers(&signature);
        no_signature.remove(header::SIGNATURE);
        assert_eq!(
            authenticate(&verifier(), &no_signature, Bytes::new()),
            Err(ReceiveError::Signature(SignatureError::Missing))
        );

        let mut form = headers(&signature);
        form.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/x-www-form-urlencoded"),
        );
        assert_eq!(
            authenticate(&verifier(), &form, Bytes::new()),
            Err(ReceiveError::UnsupportedContentType)
        );

        let mut no_delivery = headers(&signature);
        no_delivery.remove(header::DELIVERY_ID);
        assert_eq!(
            authenticate(&verifier(), &no_delivery, Bytes::new()),
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
            authenticate(&verifier(), &no_content_type, Bytes::new()),
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
            authenticate(&verifier(), &empty_delivery, Bytes::new()),
            Err(ReceiveError::MissingHeader {
                name: header::DELIVERY_ID
            })
        );

        let mut empty_event = headers(&signature);
        empty_event.insert(header::EVENT_NAME, HeaderValue::from_static(""));
        assert_eq!(
            authenticate(&verifier(), &empty_event, Bytes::new()),
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
            authenticate(&verifier(), &no_event_name, Bytes::new()),
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

/// How `authenticate` reads the `http::HeaderMap`: by name, whatever case
/// the names arrived in; the first value of a repeated header; a value that
/// is not visible ASCII as malformed for the signature and as missing for
/// the rest.
mod header_map {
    use bytes::Bytes;
    use http::{HeaderMap, HeaderValue};

    use crate::test_support::{BODY, headers, headers_from, verifier};
    use crate::{
        EventKind, ReceiveError, SignatureError, TargetType, WebhookMeta, authenticate, header,
    };

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

        let envelope = authenticate(&verifier(), &headers, Bytes::from_static(BODY)).unwrap();

        assert_eq!(envelope.meta.delivery_id, "delivery");
        assert_eq!(envelope.meta.kind, EventKind::PullRequest);
        assert_eq!(envelope.meta.target_type, Some(TargetType::Integration));
        assert_eq!(envelope.meta.target_id, Some(12345));
    }

    #[test]
    fn the_header_meta_reads_the_headers_the_envelope_carries() {
        // What a source chooses the verifier by is what the envelope ends up
        // with: one read of the headers, the target included.
        let signature = verifier().sign(BODY).to_string();
        let headers = headers(&signature);

        let meta = WebhookMeta::from_headers(&headers).unwrap();
        let envelope = authenticate(&verifier(), &headers, Bytes::from_static(BODY)).unwrap();

        assert_eq!(meta.delivery_id, envelope.meta.delivery_id);
        assert_eq!(meta.kind, envelope.meta.kind);
        assert_eq!(meta.target_type, Some(TargetType::Repository));
        assert_eq!(meta.target_type, envelope.meta.target_type);
        assert_eq!(meta.target_id, Some(7));
        assert_eq!(meta.target_id, envelope.meta.target_id);
    }

    #[test]
    fn the_header_meta_refuses_a_missing_delivery_id_before_a_missing_event_name() {
        // The order `authenticate` reports them in, once authenticated; the
        // receiver reads the header meta before the body and reports the same.
        let mut neither = HeaderMap::new();
        neither.insert(header::TARGET_TYPE, HeaderValue::from_static("integration"));
        let mut no_event = neither.clone();
        no_event.insert(header::DELIVERY_ID, HeaderValue::from_static("delivery"));

        assert_eq!(
            WebhookMeta::from_headers(&neither),
            Err(ReceiveError::MissingHeader {
                name: header::DELIVERY_ID
            })
        );
        assert_eq!(
            WebhookMeta::from_headers(&no_event),
            Err(ReceiveError::MissingHeader {
                name: header::EVENT_NAME
            })
        );
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
            authenticate(&verifier(), &headers, Bytes::new()),
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
            authenticate(&verifier(), &headers, Bytes::new()),
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
            authenticate(&verifier(), &headers, Bytes::new()),
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

        let envelope = authenticate(&verifier(), &headers, Bytes::from_static(BODY)).unwrap();

        assert_eq!(envelope.meta.delivery_id, "delivery");
    }

    #[test]
    fn an_empty_map_is_refused_as_unsigned() {
        assert_eq!(
            authenticate(&verifier(), &HeaderMap::new(), Bytes::new()),
            Err(ReceiveError::Signature(SignatureError::Missing))
        );
    }
}
