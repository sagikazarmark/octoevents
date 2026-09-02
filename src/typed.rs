use octocrab::models::webhook_events::WebhookEvent;

use crate::Envelope;

impl Envelope {
    /// Parses the payload using octocrab's deep webhook models.
    ///
    /// Best-effort: octocrab's webhook models are hand-maintained and
    /// self-described as beta. An event kind octocrab does not know still
    /// parses -- it arrives as [`WebhookEventPayload::Unknown`] carrying the
    /// generic JSON -- so an error here means the body was not a JSON object
    /// or a known kind's payload drifted. [`Envelope::raw`] is unaffected
    /// either way, and [`Envelope::parse`] deserializes a caller-defined view
    /// that only breaks on fields you name.
    ///
    /// Parses [`Envelope::raw`] on every call. Bind the result rather than
    /// calling it repeatedly: a delivery can carry megabytes of JSON.
    ///
    /// Enabling the `octocrab` feature makes octocrab's pre-1.0 version part
    /// of this crate's public API: [`WebhookEvent`] is octocrab's type, so an
    /// octocrab major bump here is a breaking change for this method.
    ///
    /// [`WebhookEventPayload::Unknown`]: octocrab::models::webhook_events::WebhookEventPayload::Unknown
    ///
    /// # Errors
    ///
    /// Returns a serde error for non-object bodies and payloads octocrab
    /// cannot represent.
    #[cfg_attr(docsrs, doc(cfg(feature = "octocrab")))]
    pub fn parse_typed(&self) -> Result<WebhookEvent, serde_json::Error> {
        WebhookEvent::try_from_header_and_body(self.kind.as_str(), &self.raw)
    }
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use octocrab::models::webhook_events::{WebhookEventPayload, WebhookEventType};

    use crate::{Common, Envelope, EventKind};

    fn envelope(kind: EventKind, raw: &'static [u8]) -> Envelope {
        Envelope {
            delivery_id: "delivery".into(),
            kind,
            action: None,
            common: Common::default(),
            target_type: None,
            target_id: None,
            raw: Bytes::from_static(raw),
        }
    }

    #[test]
    fn returns_octocrab_models_when_the_payload_is_supported() {
        let event = envelope(EventKind::Ping, br#"{"zen":"Keep it logically awesome."}"#)
            .parse_typed()
            .unwrap();

        assert_eq!(event.kind, WebhookEventType::Ping);
        assert!(matches!(event.specific, WebhookEventPayload::Ping(_)));
    }

    #[test]
    fn represents_unknown_event_kinds_as_generic_json() {
        let event = envelope(
            EventKind::Unknown("future_event".into()),
            br#"{"future":true}"#,
        )
        .parse_typed()
        .unwrap();

        assert_eq!(
            event.specific,
            WebhookEventPayload::Unknown(Box::new(serde_json::json!({"future": true})))
        );
    }

    #[test]
    fn fails_for_payloads_octocrab_cannot_represent() {
        let envelope = envelope(EventKind::PullRequest, br#"{"future":true}"#);

        assert!(envelope.parse_typed().is_err());
        assert_eq!(envelope.raw, Bytes::from_static(br#"{"future":true}"#));
    }

    #[test]
    fn fails_for_invalid_json_without_touching_the_raw_body() {
        let envelope = envelope(EventKind::Ping, b"not json");

        assert!(envelope.parse_typed().is_err());
        assert_eq!(envelope.raw, Bytes::from_static(b"not json"));
    }
}
