/// Identity / authorization context that accompanies a state modification.
pub trait IInfo: Send + Sync {
    fn is_god(&self) -> bool;
    fn user_id(&self) -> String;
    fn store_id(&self) -> String;
    fn identity(&self) -> (String, String);
}
