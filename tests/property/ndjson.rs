//! Properties: NDJSON op + event roundtrip.
//!
//! - `prop_ndjson_op_event_roundtrip_subscribe` — encoding a
//!   `subscribe` op + decoding back yields equal value.
//! - `prop_ndjson_event_ack_encodable` — every Ack event is encodable
//!   without losing the correlation id.

use proptest::prelude::*;
use ssh_mcp::domain::ids::SessionId;
use ssh_mcp::embed::formatter::{Event, encode_line};

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    /// `prop_ndjson_op_event_roundtrip_subscribe`
    ///
    /// Encode a `subscribe` payload as JSON and decode back via the
    /// public Op type. Roundtrip MUST preserve every field.
    #[test]
    fn prop_ndjson_op_event_roundtrip_subscribe(
        uri in "[a-z]{4,8}://[a-z0-9-]{2,16}/output",
        cursor in 0_u64..u64::from(u32::MAX),
        id in proptest::option::of("[A-Za-z0-9-]{1,16}"),
    ) {
        let payload = serde_json::json!({
            "op": "read",
            "uri": uri,
            "cursor": cursor,
            "id": id,
        });
        let json = serde_json::to_string(&payload).unwrap();
        let parsed: ssh_mcp::embed::parser::Op = serde_json::from_str(&json).unwrap();
        match parsed {
            ssh_mcp::embed::parser::Op::Read { uri: u, cursor: c, id: i } => {
                prop_assert_eq!(u, uri);
                prop_assert_eq!(c, Some(cursor));
                prop_assert_eq!(i, id);
            }
            other => prop_assert!(false, "wrong variant decoded: {:?}", other),
        }
    }

    /// `prop_ndjson_event_ack_encodable`
    ///
    /// Encoding an Ack event preserves the correlation id and the
    /// session id when present.
    #[test]
    fn prop_ndjson_event_ack_encodable(
        id in "[A-Za-z0-9-]{1,16}",
        sid in "[a-z0-9-]{4,32}",
    ) {
        let event = Event::Ack {
            id: Some(id.clone()),
            sid: Some(SessionId::new(sid.clone())),
            cid: None,
            shid: None,
            sub_id: None,
            tid: None,
            uri: None,
        };
        let line = encode_line(&event).unwrap();
        prop_assert!(line.contains(&id), "ack missing id: {line}");
        prop_assert!(line.contains(&sid), "ack missing sid: {line}");
    }

    /// `prop_ndjson_event_warn_carries_code`
    ///
    /// Warn events are encoded with the wire code intact.
    #[test]
    fn prop_ndjson_event_warn_carries_code(
        code in "[A-Z_]{4,32}",
        msg in "[a-zA-Z0-9 _]{1,64}",
    ) {
        let event = Event::Warn {
            code: code.clone(),
            resource: None,
            msg: msg.clone(),
        };
        let line = encode_line(&event).unwrap();
        prop_assert!(line.contains(&code));
        prop_assert!(line.contains(&msg));
    }
}
