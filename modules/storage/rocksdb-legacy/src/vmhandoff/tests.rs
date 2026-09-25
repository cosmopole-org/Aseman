use std::sync::Mutex;

use super::*;

#[derive(Default)]
struct Memory(Mutex<BTreeMap<Vec<u8>, Vec<u8>>>);

impl LegacyKvStore for Memory {
    fn get(&self, key: &[u8]) -> LegacyMigrationResult<Option<Vec<u8>>> {
        Ok(self.0.lock().unwrap().get(key).cloned())
    }
    fn scan_prefix(&self, prefix: &[u8]) -> LegacyMigrationResult<Vec<(Vec<u8>, Vec<u8>)>> {
        Ok(self
            .0
            .lock()
            .unwrap()
            .iter()
            .filter(|(key, _)| key.starts_with(prefix))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect())
    }
    fn scan_all(&self) -> LegacyMigrationResult<Vec<(Vec<u8>, Vec<u8>)>> {
        self.scan_prefix(b"")
    }
    fn write_batch(&self, writes: &[LegacyKvWrite]) -> LegacyMigrationResult<()> {
        let mut map = self.0.lock().unwrap();
        for write in writes {
            match write {
                LegacyKvWrite::Put { key, value } => {
                    map.insert(key.clone(), value.clone());
                }
                LegacyKvWrite::Delete { key } => {
                    map.remove(key);
                }
            }
        }
        Ok(())
    }
}

fn store() -> Memory {
    let memory = Memory::default();
    let put = |key: &str, value: &str| LegacyKvWrite::Put {
        key: key.as_bytes().to_vec(),
        value: value.as_bytes().to_vec(),
    };
    memory
        .write_batch(&[
            put("link::VmInstance::5@o::main::vm-a", "true"),
            put("link::VmStatus::vm-a", "running"),
            put("link::VmStartedAt::vm-a", "1700"),
            put("link::vmEntityType::5@o::main", "javascript"),
            put("link::VmInstance::6@o::web::vm-b", "true"),
            put("link::VmStatus::vm-b", "running"),
            put("link::VmContainerName::6@o::web::vm-b", "aseman-6-web"),
            put("link::vmEntityType::6@o::web", "docker"),
            put("link::ModalVolume::7@o", "vol-123"),
            put("link::Unrelated::x", "kept"),
        ])
        .unwrap();
    memory
}

#[test]
fn the_plan_lists_instances_and_external_handles() {
    let store = store();
    let plan = plan_legacy_vm_handoff(&store).unwrap();
    assert_eq!(plan.instances.len(), 2);
    assert_eq!(plan.instances[0].key(), "5@o::main::vm-a");
    assert_eq!(plan.instances[0].runtime, "javascript");
    assert_eq!(plan.instances[0].started_at_millis, Some(1700));
    assert_eq!(plan.instances[1].container.as_deref(), Some("aseman-6-web"));
    assert_eq!(
        plan.external
            .iter()
            .map(|handle| format!("{}::{}={}", handle.family, handle.key, handle.value))
            .collect::<Vec<_>>(),
        vec![
            "ModalVolume::7@o=vol-123".to_owned(),
            "VmContainerName::6@o::web::vm-b=aseman-6-web".to_owned(),
        ]
    );
    // Deterministic.
    assert_eq!(plan_legacy_vm_handoff(&store).unwrap().digest, plan.digest);
}

#[test]
fn every_instance_and_handle_needs_one_explicit_decision() {
    let store = store();
    let plan = plan_legacy_vm_handoff(&store).unwrap();
    let mut decisions = LegacyVmHandoffDecisions::default();
    decisions
        .instances
        .insert("5@o::main::vm-a".to_owned(), LegacyVmDecision::Adopt);
    assert!(
        check_legacy_vm_decisions(&plan, &decisions).is_err(),
        "vm-b undecided"
    );
    decisions
        .instances
        .insert("6@o::web::vm-b".to_owned(), LegacyVmDecision::Stop);
    assert!(
        check_legacy_vm_decisions(&plan, &decisions).is_err(),
        "the volume undecided"
    );
    decisions.kept.insert("ModalVolume::7@o".to_owned());
    assert!(
        check_legacy_vm_decisions(&plan, &decisions).is_err(),
        "the container undecided"
    );
    // Keeping the container of a stopped instance would leave it unowned.
    decisions
        .kept
        .insert("VmContainerName::6@o::web::vm-b".to_owned());
    let refused = check_legacy_vm_decisions(&plan, &decisions).unwrap_err();
    assert!(refused.to_string().contains("aseman-6-web"));
    decisions.kept.remove("VmContainerName::6@o::web::vm-b");
    decisions
        .released
        .insert("VmContainerName::6@o::web::vm-b".to_owned());
    check_legacy_vm_decisions(&plan, &decisions).unwrap();
    decisions.released.insert("ModalApp::nope".to_owned());
    assert!(
        check_legacy_vm_decisions(&plan, &decisions).is_err(),
        "unknown handle"
    );
}

#[test]
fn completion_removes_decided_records_only_for_the_approved_plan() {
    let store = store();
    let plan = plan_legacy_vm_handoff(&store).unwrap();
    let decisions = LegacyVmHandoffDecisions {
        instances: BTreeMap::from([
            ("5@o::main::vm-a".to_owned(), LegacyVmDecision::Adopt),
            ("6@o::web::vm-b".to_owned(), LegacyVmDecision::Adopt),
        ]),
        released: BTreeSet::from(["VmContainerName::6@o::web::vm-b".to_owned()]),
        kept: BTreeSet::from(["ModalVolume::7@o".to_owned()]),
    };
    assert!(complete_legacy_vm_handoff(&store, [0; 32], &decisions).is_err());
    complete_legacy_vm_handoff(&store, plan.digest, &decisions).unwrap();
    let remaining = plan_legacy_vm_handoff(&store).unwrap();
    assert!(remaining.instances.is_empty());
    assert_eq!(remaining.external.len(), 1, "a kept handle stays");
    assert!(store.get(b"link::VmStatus::vm-a").unwrap().is_none());
    assert!(
        store
            .get(b"link::VmContainerName::6@o::web::vm-b")
            .unwrap()
            .is_none()
    );
    assert_eq!(
        store.get(b"link::Unrelated::x").unwrap(),
        Some(b"kept".to_vec())
    );
    assert_eq!(
        store.get(b"link::vmEntityType::5@o::main").unwrap(),
        Some(b"javascript".to_vec()),
        "durable intent is not observed runtime"
    );
}
