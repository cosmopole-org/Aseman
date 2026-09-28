/// Identity / authorization context that accompanies a state modification.
pub trait IInfo: Send + Sync {
    #[expect(
        dead_code,
        reason = "RL-002: legacy model surface kept until its deletion gate"
    )]
    fn is_god(&self) -> bool;
    fn user_id(&self) -> String;
    fn store_id(&self) -> String;
    #[expect(
        dead_code,
        reason = "RL-002: legacy model surface kept until its deletion gate"
    )]
    fn identity(&self) -> (String, String);
}
