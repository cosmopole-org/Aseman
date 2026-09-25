use serde::{Deserialize, Serialize};

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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn consume_token_input_round_trips() {
        let v = ConsumeTokenInput {
            orig: "x".to_string(),
            token_owner_id: "owner".to_string(),
            token_id: "tok".to_string(),
            amount: -1,
        };
        let s = serde_json::to_string(&v).unwrap();
        let parsed: ConsumeTokenInput = serde_json::from_str(&s).unwrap();
        assert_eq!(parsed.orig, v.orig);
        assert_eq!(parsed.amount, v.amount);
        assert_eq!(parsed.token_id, v.token_id);
    }
}
