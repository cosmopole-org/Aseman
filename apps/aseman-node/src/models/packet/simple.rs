use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ResponseSimpleMessage {
    #[serde(rename = "message")]
    pub message: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn response_simple_message_round_trips() {
        let r = ResponseSimpleMessage {
            message: "ok".to_string(),
        };
        let s = serde_json::to_string(&r).unwrap();
        assert_eq!(s, r#"{"message":"ok"}"#);
        let parsed: ResponseSimpleMessage = serde_json::from_str(&s).unwrap();
        assert_eq!(parsed.message, "ok");
    }
}
