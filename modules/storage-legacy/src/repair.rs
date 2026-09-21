//! LD-12 pre-export membership audit and repair.
//!
//! The ADR 0018 export fails closed on membership links it cannot map. Legacy creature
//! deletion never removed memberships (LD-12), so installations can hold pairs whose
//! member or store no longer exists. This module finds every such link with the export's
//! own resolution rules. It proposes removal only where the legacy node intended removal.
//! Everything else is left for an operator decision. A repair applies only when the
//! operator passes back the digest of the audit they approved, recomputed from the
//! current keys, so nothing that changed after approval is ever removed.

use super::*;

/// Why a legacy membership link cannot be exported as it is.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum LegacyMembershipDefect {
    /// The store object is gone; the node's store delete intends the links gone too.
    MissingStore,
    /// A local-origin member with no creature or program; LD-12 left it behind.
    DanglingLocalMember,
    /// Only one of `onaccess`/`hasaccess` exists (LD-11). Deleting it or completing it
    /// changes who can read or signal, so an operator decides.
    OneSidedPair,
    /// The store has no creator link, or it names a local-origin identity with no
    /// creature or program, so the store has no owner to migrate under. An operator
    /// reassigns or deletes the store. `principal` is the creator, or empty.
    OrphanedCreator,
    /// LD-16: an `ownerof` link to a creature that no longer exists.
    DanglingOwnerLink,
    /// LD-16: an `ownerof` link naming an owner other than the creature's `ownerId`.
    StaleOwnerLink,
    /// A non-human creature without the `ownerof` link its `ownerId` derives. With an
    /// empty `ownerId` nothing can be derived, and an operator decides.
    MissingOwnerLink,
}

impl LegacyMembershipDefect {
    fn code(self) -> &'static str {
        match self {
            Self::MissingStore => "missing_store",
            Self::DanglingLocalMember => "dangling_local_member",
            Self::OneSidedPair => "one_sided_pair",
            Self::OrphanedCreator => "orphaned_creator",
            Self::DanglingOwnerLink => "dangling_owner_link",
            Self::StaleOwnerLink => "stale_owner_link",
            Self::MissingOwnerLink => "missing_owner_link",
        }
    }
}

/// One membership link the export would refuse.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct LegacyMembershipFinding {
    pub defect: LegacyMembershipDefect,
    pub store: String,
    /// The member, or the creator for [`LegacyMembershipDefect::OrphanedCreator`].
    pub principal: String,
    /// Physical keys the repair deletes.
    pub removals: Vec<String>,
    /// Derived link keys the repair writes as `true`, rebuilt from the record.
    /// A finding with neither removals nor additions needs an operator decision.
    pub additions: Vec<String>,
}

/// The audit an operator reviews and approves by its digest.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LegacyMembershipAudit {
    pub findings: Vec<LegacyMembershipFinding>,
    /// SHA-256 over the canonical findings; the approval token for a repair.
    pub digest: [u8; 32],
}

impl LegacyMembershipAudit {
    fn new(mut findings: Vec<LegacyMembershipFinding>) -> Self {
        findings.sort();
        let mut hasher = Sha256::new();
        hasher.update(b"aseman.legacy-membership-audit.v2\n");
        for finding in &findings {
            for part in [finding.defect.code(), &finding.store, &finding.principal] {
                hasher.update(part.as_bytes());
                hasher.update([0]);
            }
            for key in &finding.removals {
                hasher.update(b"-");
                hasher.update(key.as_bytes());
                hasher.update([0]);
            }
            for key in &finding.additions {
                hasher.update(b"+");
                hasher.update(key.as_bytes());
                hasher.update([0]);
            }
            hasher.update(b"\n");
        }
        Self {
            findings,
            digest: hasher.finalize().into(),
        }
    }

    /// `true` when the export would accept every membership link.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.findings.is_empty()
    }

    /// Findings that only an operator can resolve.
    pub fn needs_decision(&self) -> impl Iterator<Item = &LegacyMembershipFinding> {
        self.findings
            .iter()
            .filter(|finding| finding.removals.is_empty() && finding.additions.is_empty())
    }
}

/// What an applied repair removed and what it left for an operator.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LegacyMembershipRepairReport {
    pub approved_digest: [u8; 32],
    pub removed_keys: Vec<String>,
    pub added_keys: Vec<String>,
    pub needs_decision: Vec<LegacyMembershipFinding>,
}

impl LegacySnapshotGraph {
    /// Audits every membership link and store creator against ADR 0018.
    pub fn membership_audit(
        &self,
        local_origins: &BTreeSet<String>,
    ) -> LegacyMigrationResult<LegacyMembershipAudit> {
        let (permissions, flags) = self.legacy_membership_links()?;
        let pairs = permissions
            .keys()
            .chain(flags.iter())
            .cloned()
            .collect::<BTreeSet<_>>();
        let mut findings = Vec::new();
        for (store, member) in pairs {
            let onaccess = format!("onaccess::{store}::{member}");
            let hasaccess = format!("hasaccess::{member}::{store}");
            let present = [&onaccess, &hasaccess]
                .into_iter()
                .filter(|link| self.links.contains_key(*link))
                .map(|link| format!("link::{link}"))
                .collect::<Vec<_>>();
            let store_exists = self
                .objects
                .contains_key(&("Store".to_owned(), store.clone()));
            let defect = if !store_exists {
                Some(LegacyMembershipDefect::MissingStore)
            } else if self.classify_member(&member, local_origins)?
                == LegacyMemberClass::DanglingLocal
            {
                Some(LegacyMembershipDefect::DanglingLocalMember)
            } else if present.len() == 1 {
                Some(LegacyMembershipDefect::OneSidedPair)
            } else {
                None
            };
            if let Some(defect) = defect {
                let removals = if defect == LegacyMembershipDefect::OneSidedPair {
                    Vec::new()
                } else {
                    present
                };
                findings.push(LegacyMembershipFinding {
                    defect,
                    store,
                    principal: member,
                    removals,
                    additions: Vec::new(),
                });
            }
        }
        for (family, store) in self.objects.keys() {
            if family != "Store" {
                continue;
            }
            let suffix = format!("::{store}");
            let has_creator_link = self.links.keys().any(|link| {
                link.strip_prefix("creatorof::")
                    .and_then(|rest| rest.strip_suffix(&suffix))
                    .is_some_and(|creator| !creator.is_empty())
            });
            // The fixed LD-12 deletion also drops `creatorof`, leaving no creator.
            let creator = if has_creator_link {
                self.resolve_store_creator(store)?
            } else {
                String::new()
            };
            if creator.is_empty()
                || self.classify_member(&creator, local_origins)?
                    == LegacyMemberClass::DanglingLocal
            {
                findings.push(LegacyMembershipFinding {
                    defect: LegacyMembershipDefect::OrphanedCreator,
                    store: store.clone(),
                    principal: creator,
                    removals: Vec::new(),
                    additions: Vec::new(),
                });
            }
        }
        findings.extend(self.owner_link_findings()?);
        Ok(LegacyMembershipAudit::new(findings))
    }

    /// LD-16: `ownerof` links derive from each non-human creature's `ownerId`.
    fn owner_link_findings(&self) -> LegacyMigrationResult<Vec<LegacyMembershipFinding>> {
        let owner_of = |family: &str, id: &str| -> LegacyMigrationResult<Option<String>> {
            self.objects
                .get(&(family.to_owned(), id.to_owned()))
                .map(|columns| {
                    columns
                        .get("ownerId")
                        .map(|value| {
                            String::from_utf8(value.clone()).map_err(|_| {
                                LegacyMigrationError::Invalid(format!(
                                    "legacy Creature {id} ownerId is not UTF-8"
                                ))
                            })
                        })
                        .transpose()
                        .map(Option::unwrap_or_default)
                })
                .transpose()
        };
        let mut findings = Vec::new();
        let mut linked = BTreeSet::new();
        for link in self.links.keys() {
            let Some(rest) = link.strip_prefix("ownerof::") else {
                continue;
            };
            let Some((owner, creature)) = rest.split_once("::") else {
                continue;
            };
            let defect = match owner_of("Creature", creature)? {
                None => Some(LegacyMembershipDefect::DanglingOwnerLink),
                Some(owner_id) if owner_id != owner => Some(LegacyMembershipDefect::StaleOwnerLink),
                Some(_) => {
                    linked.insert(creature.to_owned());
                    None
                }
            };
            if let Some(defect) = defect {
                findings.push(LegacyMembershipFinding {
                    defect,
                    store: String::new(),
                    principal: creature.to_owned(),
                    removals: vec![format!("link::{link}")],
                    additions: Vec::new(),
                });
            }
        }
        for ((family, id), columns) in &self.objects {
            let human = columns.get("type").map(Vec::as_slice) == Some(b"human".as_slice());
            if family != "Creature" || human || linked.contains(id) {
                continue;
            }
            let owner_id = owner_of("Creature", id)?.unwrap_or_default();
            findings.push(LegacyMembershipFinding {
                defect: LegacyMembershipDefect::MissingOwnerLink,
                store: String::new(),
                principal: id.clone(),
                removals: Vec::new(),
                additions: if owner_id.is_empty() {
                    Vec::new()
                } else {
                    vec![format!("link::ownerof::{owner_id}::{id}")]
                },
            });
        }
        Ok(findings)
    }
}

/// The key families the membership audit reads.
const MEMBERSHIP_AUDIT_PREFIXES: [&[u8]; 4] = [
    b"link::",
    b"obj::Store::",
    b"obj::Creature::",
    b"obj::Program::",
];

fn membership_audit_graph(store: &dyn LegacyKvStore) -> LegacyMigrationResult<LegacySnapshotGraph> {
    let mut records = Vec::new();
    for prefix in MEMBERSHIP_AUDIT_PREFIXES {
        for (key, value) in store.scan_prefix(prefix)? {
            records.push(LegacyPhysicalRecord {
                family: "membership-audit".to_owned(),
                key,
                value,
            });
        }
    }
    LegacySnapshotGraph::assemble(records)
}

/// Audits the membership links in a stopped legacy node's key/value store.
pub fn audit_legacy_memberships(
    store: &dyn LegacyKvStore,
    local_origins: &BTreeSet<String>,
) -> LegacyMigrationResult<LegacyMembershipAudit> {
    membership_audit_graph(store)?.membership_audit(local_origins)
}

/// Applies an approved audit in one atomic batch: removals are deleted and derived
/// owner links are rebuilt.
///
/// The audit is recomputed from the current keys. If its digest differs from
/// `approved_digest`, nothing is written. Run it only while the node is stopped.
pub fn repair_legacy_memberships(
    store: &dyn LegacyKvStore,
    local_origins: &BTreeSet<String>,
    approved_digest: [u8; 32],
) -> LegacyMigrationResult<LegacyMembershipRepairReport> {
    let audit = audit_legacy_memberships(store, local_origins)?;
    if audit.digest != approved_digest {
        return Err(LegacyMigrationError::Invalid(
            "legacy membership audit changed since it was approved; re-run the audit".to_owned(),
        ));
    }
    let removed_keys = audit
        .findings
        .iter()
        .flat_map(|finding| finding.removals.iter().cloned())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let added_keys = audit
        .findings
        .iter()
        .flat_map(|finding| finding.additions.iter().cloned())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let writes = removed_keys
        .iter()
        .map(|key| LegacyKvWrite::Delete {
            key: key.as_bytes().to_vec(),
        })
        .chain(added_keys.iter().map(|key| LegacyKvWrite::Put {
            key: key.as_bytes().to_vec(),
            value: b"true".to_vec(),
        }))
        .collect::<Vec<_>>();
    if !writes.is_empty() {
        store.write_batch(&writes)?;
    }
    Ok(LegacyMembershipRepairReport {
        approved_digest,
        removed_keys,
        added_keys,
        needs_decision: audit.needs_decision().cloned().collect(),
    })
}
