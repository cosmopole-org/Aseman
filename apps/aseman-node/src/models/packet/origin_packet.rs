use serde::{Deserialize, Serialize};

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
    #[serde(with = "crate::util::bytes_base64", default)]
    pub binary: Vec<u8>,
    pub signature: String,
    #[serde(default)]
    pub exceptions: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(parsed.key, original.key);
        assert_eq!(parsed.user_id, original.user_id);
        assert_eq!(parsed.res_code, original.res_code);
        assert_eq!(parsed.binary, original.binary);
        assert_eq!(parsed.exceptions, original.exceptions);
    }

    #[test]
    fn origin_packet_type_field_is_capitalised_in_json() {
        let p = OriginPacket {
            typ: "X".to_string(),
            ..OriginPacket::default()
        };
        let s = serde_json::to_string(&p).unwrap();
        // The struct explicitly renames `typ` -> "Type".
        assert!(s.contains(r#""Type":"X""#), "missing Type field: {}", s);
        // PascalCase derives convert snake_case names.
        assert!(s.contains(r#""UserId""#));
        assert!(s.contains(r#""ResCode""#));
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
        // Both binary and exceptions are #[serde(default)].
        let p: OriginPacket = serde_json::from_str(
            r#"{"Type":"X","Key":"","UserId":"","StoreId":"","RequestId":"","ResCode":0,"Signature":""}"#,
        )
        .expect("parse");
        assert!(p.binary.is_empty());
        assert!(p.exceptions.is_empty());
    }
}
