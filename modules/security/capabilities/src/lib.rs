//! The reference policy provider (ADR 0008): the pure evaluator of
//! [`aseman_domain::authority`] over the compiled-in A402 registry.
#![forbid(unsafe_code)]

use aseman_domain::authority::{ActionRegistry, PolicyDecision, PolicyRequest, evaluate};
use aseman_ports::{PolicyDecisionPort, PortError, PortResult};

/// Decides with one registry and policy version.
#[derive(Clone, Debug)]
pub struct RegistryPolicy {
    registry: ActionRegistry,
    policy_version: String,
}

impl RegistryPolicy {
    #[must_use]
    pub fn new(registry: ActionRegistry, policy_version: impl Into<String>) -> Self {
        Self {
            registry,
            policy_version: policy_version.into(),
        }
    }

    /// The provider over the compiled-in A402 registry.
    ///
    /// # Errors
    ///
    /// `Failed` when the registry does not load; nothing is then authorized.
    pub fn compiled(policy_version: impl Into<String>) -> PortResult<Self> {
        aseman_contracts::security::action_registry()
            .map(|registry| Self::new(registry, policy_version))
            .map_err(PortError::Failed)
    }
}

impl PolicyDecisionPort for RegistryPolicy {
    fn decide(&self, request: &PolicyRequest) -> PortResult<PolicyDecision> {
        Ok(evaluate(&self.registry, request, &self.policy_version))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_reference_provider_reproduces_the_a404_fixtures() {
        let provider = RegistryPolicy::compiled("policy-1").unwrap();
        let cases = aseman_policy_conformance::check_provider(&provider).unwrap();
        assert!(cases >= 20);
    }
}
