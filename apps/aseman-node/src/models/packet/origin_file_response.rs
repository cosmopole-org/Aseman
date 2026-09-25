use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct OriginFileRes {
    pub user_id: String,
    pub store_id: String,
    pub request_id: String,
    pub file_id: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origin_file_res_uses_pascal_case_keys() {
        let r = OriginFileRes {
            user_id: "u".to_string(),
            store_id: "s".to_string(),
            request_id: "r".to_string(),
            file_id: "f".to_string(),
        };
        let s = serde_json::to_string(&r).unwrap();
        assert!(s.contains(r#""UserId":"u""#), "{}", s);
        assert!(s.contains(r#""StoreId":"s""#), "{}", s);
        assert!(s.contains(r#""RequestId":"r""#), "{}", s);
        assert!(s.contains(r#""FileId":"f""#), "{}", s);

        let parsed: OriginFileRes = serde_json::from_str(&s).unwrap();
        assert_eq!(parsed.user_id, r.user_id);
        assert_eq!(parsed.file_id, r.file_id);
    }
}
