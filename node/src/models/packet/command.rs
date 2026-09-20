use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct Command {
    pub value: String,
    pub data: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_serializes_with_pascal_case_keys() {
        let c = Command {
            value: "v".to_string(),
            data: "d".to_string(),
        };
        let s = serde_json::to_string(&c).unwrap();
        // PascalCase rename turns `value` -> `Value`, `data` -> `Data`.
        assert_eq!(s, r#"{"Value":"v","Data":"d"}"#);
    }

    #[test]
    fn command_round_trips() {
        let c = Command {
            value: "ping".to_string(),
            data: "x".to_string(),
        };
        let s = serde_json::to_string(&c).unwrap();
        let parsed: Command = serde_json::from_str(&s).unwrap();
        assert_eq!(parsed.value, c.value);
        assert_eq!(parsed.data, c.data);
    }
}
