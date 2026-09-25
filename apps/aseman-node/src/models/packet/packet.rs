use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Packet {
    #[serde(rename = "origin")]
    pub origin: String,
    #[serde(rename = "data")]
    pub data: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LogPacket {
    #[serde(rename = "id")]
    pub id: String,
    #[serde(rename = "storeId")]
    pub store_id: String,
    #[serde(rename = "userId")]
    pub user_id: String,
    #[serde(rename = "data")]
    pub data: String,
    /// Sender-supplied labels stored with the packet. See
    /// [`crate::models::packet::signal_tags`] — these are what `stores/history`
    /// filters on.
    #[serde(rename = "tags", default)]
    pub tags: Vec<String>,
    #[serde(rename = "time")]
    pub time: i64,
    #[serde(rename = "edited")]
    pub edited: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BuildPacket {
    #[serde(rename = "id")]
    pub id: String,
    #[serde(rename = "buildId")]
    pub build_id: String,
    #[serde(rename = "creatureId")]
    pub creature_id: String,
    #[serde(rename = "vmId", skip_serializing_if = "String::is_empty", default)]
    pub vm_id: String,
    #[serde(rename = "logType", skip_serializing_if = "String::is_empty", default)]
    pub log_type: String,
    #[serde(rename = "time")]
    pub time: i64,
    #[serde(rename = "data")]
    pub data: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::packet::command::Command;
    use crate::models::packet::error::build_error_json;

    #[test]
    fn test_build_error_json() {
        let err_obj = build_error_json("boom");
        assert_eq!(err_obj.message, "boom", "message mismatch");
    }

    #[test]
    fn test_packet_json_shapes() {
        let p = Packet {
            origin: "fed-a".to_string(),
            data: "payload".to_string(),
        };
        let raw = serde_json::to_string(&p).expect("marshal packet");
        assert_eq!(
            raw, r#"{"origin":"fed-a","data":"payload"}"#,
            "unexpected packet json"
        );

        let cmd = Command {
            value: "ping".to_string(),
            data: "x".to_string(),
        };
        assert!(
            cmd.value == "ping" && cmd.data == "x",
            "command fields mismatch"
        );
    }

    #[test]
    fn packet_round_trips_through_json() {
        let p = Packet {
            origin: "fed".to_string(),
            data: "d".to_string(),
        };
        let raw = serde_json::to_vec(&p).unwrap();
        let parsed: Packet = serde_json::from_slice(&raw).unwrap();
        assert_eq!(parsed.origin, p.origin);
        assert_eq!(parsed.data, p.data);
    }

    #[test]
    fn log_packet_serializes_with_camel_case_keys() {
        let p = LogPacket {
            id: "1".to_string(),
            store_id: "store-7".to_string(),
            user_id: "u".to_string(),
            data: "msg".to_string(),
            tags: vec!["kind=message".to_string()],
            time: 17,
            edited: true,
        };
        let s = serde_json::to_string(&p).unwrap();
        // Field names follow the explicit serde rename.
        assert!(s.contains(r#""storeId":"store-7""#), "{}", s);
        assert!(s.contains(r#""userId":"u""#), "{}", s);
        assert!(s.contains(r#""edited":true"#), "{}", s);
        // Round trip is lossless.
        let parsed: LogPacket = serde_json::from_str(&s).unwrap();
        assert_eq!(parsed.store_id, p.store_id);
        assert_eq!(parsed.time, p.time);
        assert_eq!(parsed.edited, p.edited);
        assert_eq!(parsed.tags, p.tags);
    }

    #[test]
    fn build_packet_omits_empty_optional_fields() {
        let p = BuildPacket {
            id: "id".to_string(),
            build_id: "b".to_string(),
            creature_id: "c".to_string(),
            // vm_id and log_type are left empty; they should be skipped on serialize.
            vm_id: String::new(),
            log_type: String::new(),
            time: 0,
            data: "x".to_string(),
        };
        let s = serde_json::to_string(&p).unwrap();
        assert!(
            !s.contains("vmId"),
            "vmId should be skipped when empty: {}",
            s
        );
        assert!(
            !s.contains("logType"),
            "logType should be skipped when empty: {}",
            s
        );
        assert!(s.contains(r#""buildId":"b""#));
        assert!(s.contains(r#""creatureId":"c""#));
    }

    #[test]
    fn build_packet_includes_optional_fields_when_set() {
        let p = BuildPacket {
            id: "id".to_string(),
            build_id: "b".to_string(),
            creature_id: "c".to_string(),
            vm_id: "vm".to_string(),
            log_type: "stdout".to_string(),
            time: 1,
            data: String::new(),
        };
        let s = serde_json::to_string(&p).unwrap();
        assert!(s.contains(r#""vmId":"vm""#), "{}", s);
        assert!(s.contains(r#""logType":"stdout""#), "{}", s);
    }

    #[test]
    fn build_error_json_round_trip() {
        let e = build_error_json("oops");
        let s = serde_json::to_string(&e).unwrap();
        assert_eq!(s, r#"{"message":"oops"}"#);
        let parsed: crate::models::packet::error::Error = serde_json::from_str(&s).unwrap();
        assert_eq!(parsed.message, "oops");
    }
}
