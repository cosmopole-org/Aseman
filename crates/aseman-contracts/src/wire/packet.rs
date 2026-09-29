//! Legacy wire packet types (RL-002). Migrated from the node's `models/packet`
//! and `compat/multipart`; the node keeps a compatibility re-export shim until
//! the legacy transports retire (RL-009).

use std::collections::HashMap;
use std::io::Cursor;

use serde::{Deserialize, Serialize};

/// Minimal in-memory equivalent of Go's `mime/multipart.FileHeader`,
/// covering the parts the Caspar node relies on.
#[derive(Debug, Clone, Default)]
pub struct FileHeader {
    pub filename: String,
    pub header: HashMap<String, Vec<String>>,
    pub size: i64,
    /// Full file contents held in memory (Go keeps either a temp file or an
    /// in-memory buffer; the node always reads the whole file, so we buffer it).
    pub content: Vec<u8>,
}

impl FileHeader {
    /// Equivalent of Go's `(*FileHeader).Open()`.
    pub fn open(&self) -> std::io::Result<Cursor<Vec<u8>>> {
        Ok(Cursor::new(self.content.clone()))
    }
}

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
    /// Sender-supplied labels stored with the packet. These are what
    /// `stores/history` filters on (`aseman_domain::signal_tags`).
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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct Command {
    pub value: String,
    pub data: String,
}

/// Input for consuming an amount of a token. Required-field validation is
/// applied by the shell API layer.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ConsumeTokenInput {
    #[serde(rename = "orig")]
    pub orig: String,
    #[serde(rename = "tokenOwnerId")]
    pub token_owner_id: String,
    #[serde(rename = "tokenId")]
    pub token_id: String,
    #[serde(rename = "amount")]
    pub amount: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ResponseSimpleMessage {
    #[serde(rename = "message")]
    pub message: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Error {
    #[serde(rename = "message")]
    pub message: String,
}

pub fn build_error_json(message: &str) -> Error {
    Error {
        message: message.to_string(),
    }
}

#[derive(Debug, Clone, Default)]
pub struct OriginFile {
    pub file_info: String,
    pub user_id: String,
    pub space_id: String,
    pub topic_id: String,
    pub request_id: String,
    pub data: Option<FileHeader>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct OriginFileRes {
    pub user_id: String,
    pub store_id: String,
    pub request_id: String,
    pub file_id: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct OriginPacket {
    #[serde(rename = "Type")]
    pub typ: String,
    pub key: String,
    pub user_id: String,
    pub store_id: String,
    pub request_id: String,
    pub res_code: i64,
    #[serde(with = "crate::legacy_wire::bytes_base64", default)]
    pub binary: Vec<u8>,
    pub signature: String,
    #[serde(default)]
    pub exceptions: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn file_header_open_returns_a_cursor_that_yields_the_full_content() {
        let fh = FileHeader {
            filename: "f".to_string(),
            content: b"hello world".to_vec(),
            size: 11,
            header: HashMap::new(),
        };
        let mut cur = fh.open().expect("open");
        let mut buf = Vec::new();
        cur.read_to_end(&mut buf).unwrap();
        assert_eq!(buf, b"hello world");
    }

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
    fn command_serializes_with_pascal_case_keys() {
        let c = Command {
            value: "v".to_string(),
            data: "d".to_string(),
        };
        let s = serde_json::to_string(&c).unwrap();
        assert_eq!(s, r#"{"Value":"v","Data":"d"}"#);
    }

    #[test]
    fn consume_token_input_uses_explicit_field_names() {
        let v = ConsumeTokenInput {
            orig: "fed-a".to_string(),
            token_owner_id: "owner".to_string(),
            token_id: "tok".to_string(),
            amount: 42,
        };
        let s = serde_json::to_string(&v).unwrap();
        assert_eq!(
            s,
            r#"{"orig":"fed-a","tokenOwnerId":"owner","tokenId":"tok","amount":42}"#
        );
    }

    #[test]
    fn response_simple_message_round_trips() {
        let r = ResponseSimpleMessage {
            message: "ok".to_string(),
        };
        let s = serde_json::to_string(&r).unwrap();
        assert_eq!(s, r#"{"message":"ok"}"#);
    }

    #[test]
    fn origin_packet_round_trip_preserves_all_fields() {
        let original = OriginPacket {
            typ: "MSG".to_string(),
            key: "k".to_string(),
            user_id: "u".to_string(),
            store_id: "s".to_string(),
            request_id: "r".to_string(),
            res_code: 200,
            binary: vec![1u8, 2, 3, 255],
            signature: "sig".to_string(),
            exceptions: vec!["e1".to_string(), "e2".to_string()],
        };
        let bytes = serde_json::to_vec(&original).unwrap();
        let parsed: OriginPacket = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(parsed.typ, original.typ);
        assert_eq!(parsed.binary, original.binary);
        assert_eq!(parsed.exceptions, original.exceptions);
    }

    #[test]
    fn origin_packet_binary_uses_base64_encoding() {
        let p = OriginPacket {
            binary: vec![0xde, 0xad, 0xbe, 0xef],
            ..OriginPacket::default()
        };
        let s = serde_json::to_string(&p).unwrap();
        assert!(s.contains(r#""Binary":"3q2+7w==""#), "encoding: {}", s);
    }

    #[test]
    fn origin_packet_accepts_missing_binary_and_exceptions() {
        let p: OriginPacket = serde_json::from_str(
            r#"{"Type":"X","Key":"","UserId":"","StoreId":"","RequestId":"","ResCode":0,"Signature":""}"#,
        )
        .expect("parse");
        assert!(p.binary.is_empty());
        assert!(p.exceptions.is_empty());
    }

    #[test]
    fn build_error_json_round_trip() {
        let e = build_error_json("oops");
        let s = serde_json::to_string(&e).unwrap();
        assert_eq!(s, r#"{"message":"oops"}"#);
        let parsed: Error = serde_json::from_str(&s).unwrap();
        assert_eq!(parsed.message, "oops");
    }
}
