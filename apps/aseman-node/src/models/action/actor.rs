use std::sync::Arc;

use crate::models::action::{IAction, ISecureAction};
use crate::util::AnyVal;

/// Registry of actions and services.
///
/// Secure actions are tracked on their own channel because
/// `Arc<dyn IAction>` cannot be downcast to `Arc<dyn ISecureAction>` at
/// runtime; callers register them with [`IActor::inject_secure_action`]
/// and look them up with [`IActor::fetch_secure_action`].
pub trait IActor: Send + Sync {
    fn inject_action(&self, action: Arc<dyn IAction>);
    fn inject_service(&self, service: AnyVal);
    fn fetch_action(&self, key: &str) -> Option<Arc<dyn IAction>>;
    fn inject_secure_action(&self, action: Arc<dyn ISecureAction>);
    fn fetch_secure_action(&self, key: &str) -> Option<Arc<dyn ISecureAction>>;
}
