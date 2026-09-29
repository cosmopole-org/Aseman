//! The A402 action registry (`contracts/security/actions.json`), loaded into the typed
//! registry the evaluator uses. The JSON is the maintained source; its consistency and
//! surface coverage are checked by `scripts/generate_security_registry.py`.

use aseman_domain::authority::{ActionClass, ActionRegistry, Condition, RegisteredAction};
use aseman_domain::identity::SubjectKind;
use serde::Deserialize;

/// The registry JSON, compiled in.
pub const ACTIONS_JSON: &str = include_str!("../../../contracts/security/actions.json");

#[derive(Deserialize)]
struct RegistryFile {
    registry_version: String,
    actions: Vec<ActionEntry>,
}

#[derive(Deserialize)]
struct ActionEntry {
    id: String,
    resource: String,
    class: ActionClass,
    subjects: Vec<String>,
    rule: Vec<String>,
    delegable: bool,
    #[serde(default)]
    packet_guard: Option<String>,
    #[serde(default)]
    surfaces: Vec<String>,
}

/// How a signed shell action authenticates its caller on the signed-packet transports
/// (TCP, WebSocket, chain, and federation), as the registry records it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PacketGuard {
    /// Anonymous callers are admitted; a supplied signature must verify.
    Public,
    /// A verified creature; a machine may use the in-process applet marker.
    User,
    /// A verified creature that is a member of the addressed store.
    Store,
    /// A verified creature with a real signature: value never moves on the
    /// applet marker.
    Finance,
}

impl PacketGuard {
    fn parse(name: &str) -> Option<Self> {
        match name {
            "public" => Some(Self::Public),
            "user" => Some(Self::User),
            "store" => Some(Self::Store),
            "finance" => Some(Self::Finance),
            _ => None,
        }
    }
}

/// One signed shell operation: the action it is authorized as and its guard.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShellOperation {
    pub action: String,
    pub guard: PacketGuard,
}

/// Parse a registry document.
///
/// # Errors
///
/// A message naming the first unknown subject class or condition, a duplicate action,
/// or malformed JSON. An unparseable registry authorizes nothing.
pub fn parse_action_registry(json: &str) -> Result<ActionRegistry, String> {
    let file: RegistryFile = serde_json::from_str(json).map_err(|error| error.to_string())?;
    let mut actions = std::collections::BTreeMap::new();
    for entry in file.actions {
        let subjects = entry
            .subjects
            .iter()
            .map(|name| {
                SubjectKind::ALL
                    .into_iter()
                    .find(|kind| kind.as_str() == name)
                    .ok_or_else(|| format!("{}: unknown subject class {name}", entry.id))
            })
            .collect::<Result<_, _>>()?;
        let rule = entry
            .rule
            .iter()
            .map(|name| {
                Condition::parse(name)
                    .ok_or_else(|| format!("{}: unknown condition {name}", entry.id))
            })
            .collect::<Result<_, _>>()?;
        let action = RegisteredAction {
            id: entry.id.clone(),
            resource: entry.resource,
            class: entry.class,
            subjects,
            rule,
            delegable: entry.delegable,
        };
        if actions.insert(entry.id.clone(), action).is_some() {
            return Err(format!("{}: duplicate action", entry.id));
        }
    }
    Ok(ActionRegistry {
        version: file.registry_version,
        actions,
    })
}

/// Every inventoried surface of the compiled-in registry and the action it is
/// authorized as (A402 coverage: each surface maps to exactly one action).
///
/// # Errors
///
/// As [`parse_action_registry`].
pub fn surface_actions() -> Result<std::collections::BTreeMap<String, String>, String> {
    let file: RegistryFile =
        serde_json::from_str(ACTIONS_JSON).map_err(|error| error.to_string())?;
    let mut map = std::collections::BTreeMap::new();
    for entry in file.actions {
        for surface in entry.surfaces {
            if map.insert(surface.clone(), entry.id.clone()).is_some() {
                return Err(format!("{surface}: claimed twice"));
            }
        }
    }
    Ok(map)
}

/// Every signed shell path of the compiled-in registry, with the action it is
/// authorized as and its guard.
///
/// # Errors
///
/// As [`parse_action_registry`], or a shell surface without a legacy guard.
pub fn shell_operations() -> Result<std::collections::BTreeMap<String, ShellOperation>, String> {
    let file: RegistryFile =
        serde_json::from_str(ACTIONS_JSON).map_err(|error| error.to_string())?;
    let mut map = std::collections::BTreeMap::new();
    for entry in file.actions {
        for surface in &entry.surfaces {
            let Some(path) = surface.strip_prefix("signed-shell-action ") else {
                continue;
            };
            let guard = entry
                .packet_guard
                .as_deref()
                .and_then(PacketGuard::parse)
                .ok_or_else(|| format!("{}: a shell surface needs a shell guard", entry.id))?;
            map.insert(
                path.to_owned(),
                ShellOperation {
                    action: entry.id.clone(),
                    guard,
                },
            );
        }
    }
    Ok(map)
}

/// The compiled-in A402 registry.
///
/// # Errors
///
/// As [`parse_action_registry`].
pub fn action_registry() -> Result<ActionRegistry, String> {
    parse_action_registry(ACTIONS_JSON)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_committed_registry_loads_and_keeps_its_invariants() {
        let registry = action_registry().unwrap();
        assert!(registry.actions.len() >= 100);
        for action in registry.actions.values() {
            assert!(!action.rule.is_empty(), "{}", action.id);
            if action.rule.contains(&Condition::Never) {
                assert_eq!(action.rule, [Condition::Never], "{}", action.id);
            }
        }
        // The legacy raw key access is registered, and forbidden.
        assert_eq!(registry.actions["raw_state.write"].rule, [Condition::Never]);
        let surfaces = surface_actions().unwrap();
        assert_eq!(
            surfaces["unified-host-call createCreature"],
            "creature.create"
        );
        assert_eq!(
            surfaces["signed-shell-action /stores/signal"],
            "store.signal"
        );
        let shell = shell_operations().unwrap();
        assert_eq!(shell.len(), 76);
        assert_eq!(shell["/creatures/transfer"].guard, PacketGuard::Finance);
        assert_eq!(shell["/stores/signal"].guard, PacketGuard::Store);
        // The actions the application already asks about exist.
        for id in [
            "workload.start",
            "workload.stop",
            "workload.pause",
            "workload.delete",
        ] {
            assert_eq!(registry.actions[id].resource, "workload", "{id}");
        }
    }

    #[test]
    fn unknown_names_fail_the_whole_registry() {
        let valid = r#"{"registry_version":"v","actions":[{"id":"a.b","resource":"r","class":"read","subjects":["user"],"rule":["public"],"delegable":true}]}"#;
        assert!(parse_action_registry(valid).is_ok());
        assert!(parse_action_registry(&valid.replace("\"public\"", "\"root\"")).is_err());
        assert!(parse_action_registry(&valid.replace("\"user\"", "\"admin\"")).is_err());
        assert!(parse_action_registry(&valid.replace("\"read\"", "\"sudo\"")).is_err());
    }
}
