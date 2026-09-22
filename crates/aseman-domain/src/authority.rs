//! Authorization (ADR 0008, A402/A404): the typed action registry, policy requests with
//! caller-established facts, the decision contract, and the reference evaluator.
//!
//! Evaluation is pure and bounded: the caller resolves the facts (ownership, membership,
//! roles) before asking, and the evaluator never performs I/O. Anything unknown denies.

use crate::capability::{Grant, chain_authorizes};
use crate::identity::{Subject, SubjectKind};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// A402 conditions. An action's rule is a list of them; the first one that holds allows.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Condition {
    Public,
    Authenticated,
    #[serde(rename = "self")]
    SelfResource,
    Owner,
    SameCreature,
    StoreRead,
    StoreSignal,
    StoreManage,
    SecretGrantee,
    Counterparty,
    FinanceOperator,
    NodeAdmin,
    Granted,
    Never,
}

impl Condition {
    pub const ALL: [Self; 14] = [
        Self::Public,
        Self::Authenticated,
        Self::SelfResource,
        Self::Owner,
        Self::SameCreature,
        Self::StoreRead,
        Self::StoreSignal,
        Self::StoreManage,
        Self::SecretGrantee,
        Self::Counterparty,
        Self::FinanceOperator,
        Self::NodeAdmin,
        Self::Granted,
        Self::Never,
    ];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Public => "public",
            Self::Authenticated => "authenticated",
            Self::SelfResource => "self",
            Self::Owner => "owner",
            Self::SameCreature => "same_creature",
            Self::StoreRead => "store_read",
            Self::StoreSignal => "store_signal",
            Self::StoreManage => "store_manage",
            Self::SecretGrantee => "secret_grantee",
            Self::Counterparty => "counterparty",
            Self::FinanceOperator => "finance_operator",
            Self::NodeAdmin => "node_admin",
            Self::Granted => "granted",
            Self::Never => "never",
        }
    }

    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|condition| condition.as_str() == text)
    }

    /// A relation the caller establishes as a fact; the others are decided by the
    /// evaluator itself.
    #[must_use]
    pub const fn is_fact(self) -> bool {
        !matches!(
            self,
            Self::Public | Self::Authenticated | Self::Granted | Self::Never
        )
    }
}

/// How sensitive an action is.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionClass {
    Read,
    Write,
    Security,
    Financial,
    Administrative,
}

/// One A402 registry entry.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RegisteredAction {
    pub id: String,
    /// The resource type the action applies to.
    pub resource: String,
    pub class: ActionClass,
    pub subjects: BTreeSet<SubjectKind>,
    pub rule: Vec<Condition>,
    /// Whether a grant for this action may be delegated (A403).
    pub delegable: bool,
}

/// The versioned A402 registry.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ActionRegistry {
    pub version: String,
    pub actions: BTreeMap<String, RegisteredAction>,
}

/// The resource an action targets.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct ResourceRef {
    /// An A402 resource type.
    pub kind: String,
    pub id: String,
}

/// A decision request. `subject` is `None` for an anonymous caller. `facts` are the
/// relations the caller established between the subject and the resource. `grants` are
/// the subject's candidate capability chains (each a grant then its ancestors, A403),
/// loaded by the caller.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PolicyRequest {
    pub subject: Option<Subject>,
    pub action: String,
    pub resource: ResourceRef,
    pub facts: BTreeSet<Condition>,
    pub grants: Vec<Vec<Grant>>,
    pub at_millis: i64,
}

/// Stable decision reason codes (A404).
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionReason {
    Allowed,
    UnknownAction,
    ResourceMismatch,
    Forbidden,
    AuthenticationRequired,
    SubjectNotAllowed,
    ConditionNotMet,
    /// The policy provider failed; decisions fail closed.
    ProviderError,
}

impl DecisionReason {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Allowed => "allowed",
            Self::UnknownAction => "unknown_action",
            Self::ResourceMismatch => "resource_mismatch",
            Self::Forbidden => "forbidden",
            Self::AuthenticationRequired => "authentication_required",
            Self::SubjectNotAllowed => "subject_not_allowed",
            Self::ConditionNotMet => "condition_not_met",
            Self::ProviderError => "provider_error",
        }
    }
}

/// A decision and its explanation (A404).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PolicyDecision {
    pub allowed: bool,
    pub reason: DecisionReason,
    /// The condition that allowed the action.
    pub matched: Option<Condition>,
    /// The conditions evaluated, in rule order, when the action was known.
    pub considered: Vec<Condition>,
    /// When `granted` allowed: the authorizing chain, grant first, root last.
    pub grant_chain: Vec<uuid::Uuid>,
    pub registry_version: String,
    pub policy_version: String,
}

impl PolicyDecision {
    /// A fail-closed decision for a provider that could not decide.
    #[must_use]
    pub fn provider_error(registry_version: &str, policy_version: &str) -> Self {
        Self {
            allowed: false,
            reason: DecisionReason::ProviderError,
            matched: None,
            considered: Vec::new(),
            grant_chain: Vec::new(),
            registry_version: registry_version.to_owned(),
            policy_version: policy_version.to_owned(),
        }
    }
}

/// One recorded policy decision (plan "Keys and audit"): who asked, for what, on
/// what, the outcome, and why. `details` is JSON text (matched condition, grant
/// chain, versions, shadow mode).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AuditRecord {
    /// The subject's canonical text, or `anonymous`.
    pub actor: String,
    pub action: String,
    /// `{resource kind}:{resource id}`.
    pub target: String,
    /// `allowed`, or the denial reason code.
    pub decision: String,
    pub occurred_at_millis: i64,
    pub details: String,
}

/// A recorded decision with its place in the actor's append-only stream.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AuditedDecision {
    pub record: AuditRecord,
    /// 1-based position in the actor's stream.
    pub sequence: u64,
}

/// The reference evaluator (ADR 0008). Checks, in order: the action is registered, the
/// resource type matches, the action is not forbidden, an anonymous caller only reaches
/// public actions, the subject class may hold the action, and then the first condition
/// of the rule that holds allows. `granted` holds when one of the request's grant chains
/// authorizes the subject for the action and resource (A403); the decision names it.
#[must_use]
pub fn evaluate(
    registry: &ActionRegistry,
    request: &PolicyRequest,
    policy_version: &str,
) -> PolicyDecision {
    let decide = |reason: DecisionReason,
                  matched: Option<Condition>,
                  considered: Vec<Condition>,
                  grant_chain: Vec<uuid::Uuid>| PolicyDecision {
        allowed: reason == DecisionReason::Allowed,
        reason,
        matched,
        considered,
        grant_chain,
        registry_version: registry.version.clone(),
        policy_version: policy_version.to_owned(),
    };
    let Some(action) = registry.actions.get(&request.action) else {
        return decide(DecisionReason::UnknownAction, None, Vec::new(), Vec::new());
    };
    let considered = action.rule.clone();
    if request.resource.kind != action.resource {
        return decide(
            DecisionReason::ResourceMismatch,
            None,
            considered,
            Vec::new(),
        );
    }
    if action.rule.contains(&Condition::Never) {
        return decide(DecisionReason::Forbidden, None, considered, Vec::new());
    }
    let Some(subject) = request.subject else {
        return if action.rule.contains(&Condition::Public) {
            decide(
                DecisionReason::Allowed,
                Some(Condition::Public),
                considered,
                Vec::new(),
            )
        } else {
            decide(
                DecisionReason::AuthenticationRequired,
                None,
                considered,
                Vec::new(),
            )
        };
    };
    if !action.subjects.contains(&subject.kind) {
        return decide(
            DecisionReason::SubjectNotAllowed,
            None,
            considered,
            Vec::new(),
        );
    }
    for condition in action.rule.iter().copied() {
        match condition {
            Condition::Public | Condition::Authenticated => {
                return decide(
                    DecisionReason::Allowed,
                    Some(condition),
                    considered,
                    Vec::new(),
                );
            }
            Condition::Never => {}
            Condition::Granted => {
                if let Some(chain) = request.grants.iter().find(|chain| {
                    chain_authorizes(
                        chain,
                        &subject,
                        &request.action,
                        &request.resource.kind,
                        &request.resource.id,
                        request.at_millis,
                    )
                }) {
                    return decide(
                        DecisionReason::Allowed,
                        Some(condition),
                        considered,
                        chain.iter().map(|grant| grant.id).collect(),
                    );
                }
            }
            fact => {
                if request.facts.contains(&fact) {
                    return decide(DecisionReason::Allowed, Some(fact), considered, Vec::new());
                }
            }
        }
    }
    decide(
        DecisionReason::ConditionNotMet,
        None,
        considered,
        Vec::new(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry() -> ActionRegistry {
        let action = |id: &str, subjects: &[SubjectKind], rule: &[Condition]| RegisteredAction {
            id: id.to_owned(),
            resource: "store".to_owned(),
            class: ActionClass::Write,
            subjects: subjects.iter().copied().collect(),
            rule: rule.to_vec(),
            delegable: true,
        };
        ActionRegistry {
            version: "test".to_owned(),
            actions: [
                action(
                    "store.signal",
                    &[SubjectKind::User, SubjectKind::Workload],
                    &[Condition::StoreSignal, Condition::Granted],
                ),
                action("store.peek", &[SubjectKind::User], &[Condition::Public]),
                action("store.raw", &[SubjectKind::Workload], &[Condition::Never]),
            ]
            .into_iter()
            .map(|action| (action.id.clone(), action))
            .collect(),
        }
    }

    fn request(action: &str, subject: Option<SubjectKind>, facts: &[Condition]) -> PolicyRequest {
        PolicyRequest {
            subject: subject.map(|kind| Subject {
                kind,
                id: "0190f1a2-7b3c-7d4e-8f00-112233445566".parse().unwrap(),
            }),
            action: action.to_owned(),
            resource: ResourceRef {
                kind: "store".to_owned(),
                id: "s1".to_owned(),
            },
            facts: facts.iter().copied().collect(),
            grants: Vec::new(),
            at_millis: 0,
        }
    }

    fn reason(request: &PolicyRequest) -> (DecisionReason, Option<Condition>) {
        let decision = evaluate(&registry(), request, "p1");
        assert_eq!(decision.allowed, decision.reason == DecisionReason::Allowed);
        assert_eq!(
            (
                decision.registry_version.as_str(),
                decision.policy_version.as_str()
            ),
            ("test", "p1")
        );
        (decision.reason, decision.matched)
    }

    #[test]
    fn decisions_deny_by_default_and_explain_themselves() {
        let user = Some(SubjectKind::User);
        assert_eq!(
            reason(&request("store.signal", user, &[Condition::StoreSignal])),
            (DecisionReason::Allowed, Some(Condition::StoreSignal))
        );
        assert_eq!(
            reason(&request("store.signal", user, &[Condition::StoreRead])),
            (DecisionReason::ConditionNotMet, None)
        );
        assert_eq!(
            reason(&request("store.unknown", user, &[])),
            (DecisionReason::UnknownAction, None)
        );
        assert_eq!(
            reason(&request(
                "store.signal",
                Some(SubjectKind::Node),
                &[Condition::StoreSignal]
            )),
            (DecisionReason::SubjectNotAllowed, None)
        );
        assert_eq!(
            reason(&request("store.signal", None, &[Condition::StoreSignal])),
            (DecisionReason::AuthenticationRequired, None)
        );
        assert_eq!(
            reason(&request("store.peek", None, &[])),
            (DecisionReason::Allowed, Some(Condition::Public))
        );
        // Forbidden actions stay forbidden whatever the facts.
        assert_eq!(
            reason(&request(
                "store.raw",
                Some(SubjectKind::Workload),
                &Condition::ALL
            )),
            (DecisionReason::Forbidden, None)
        );
        // `granted` cannot be asserted as a fact.
        assert_eq!(
            reason(&request("store.signal", user, &[Condition::Granted])),
            (DecisionReason::ConditionNotMet, None)
        );
        let mut mismatched = request("store.signal", user, &[Condition::StoreSignal]);
        mismatched.resource.kind = "program".to_owned();
        assert_eq!(
            reason(&mismatched),
            (DecisionReason::ResourceMismatch, None)
        );
    }

    #[test]
    fn conditions_round_trip_their_registry_names() {
        for condition in Condition::ALL {
            assert_eq!(Condition::parse(condition.as_str()), Some(condition));
        }
        assert_eq!(Condition::parse("root"), None);
        assert!(!Condition::Granted.is_fact());
        assert!(Condition::Owner.is_fact());
    }
}
