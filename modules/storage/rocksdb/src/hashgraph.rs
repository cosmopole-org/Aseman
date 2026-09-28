//! ADR 0025: the legacy Hashgraph store is consensus-provider state. It is classified
//! strictly and checkpointed read-only; its marshal bytes are never decoded here.

use super::*;

/// A read-only checkpoint of one node's legacy Hashgraph store.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LegacyHashgraphCheckpoint {
    pub family_counts: BTreeMap<String, usize>,
    pub highest_block: Option<u64>,
    /// Domain-separated SHA-256 over the sorted `block_*` records.
    pub block_digest: [u8; 32],
}

fn indexed(key: &str, prefix: &str) -> Option<u64> {
    let digits = key.strip_prefix(prefix)?.strip_prefix('_')?;
    (digits.len() >= 9 && digits.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| digits.parse().ok())
        .flatten()
}

/// Classify one key into its reviewed family (ADR 0025), or `None` if unreviewed.
#[must_use]
pub fn legacy_hashgraph_family(key: &str) -> Option<&'static str> {
    if let Some((participant, index)) = key.split_once("__event_") {
        return (!participant.is_empty()
            && index.len() >= 9
            && index.bytes().all(|byte| byte.is_ascii_digit()))
        .then_some("participant-event");
    }
    for (prefix, family) in [
        ("peerset", "peer-set"),
        ("topo", "topological-event"),
        ("round", "round"),
        ("block", "block"),
        ("frame", "frame"),
    ] {
        if indexed(key, prefix).is_some() {
            return Some(family);
        }
    }
    if key.strip_prefix("rep_").is_some_and(|key| !key.is_empty()) {
        return Some("repertoire");
    }
    if key
        .strip_suffix("_root")
        .is_some_and(|participant| !participant.is_empty())
    {
        return Some("participant-root");
    }
    None
}

impl LegacyHashgraphCheckpoint {
    pub fn from_records<'a>(
        records: impl IntoIterator<Item = (&'a [u8], &'a [u8])>,
    ) -> LegacyMigrationResult<Self> {
        let mut family_counts = BTreeMap::new();
        let mut blocks = BTreeMap::new();
        for (key, value) in records {
            let key = std::str::from_utf8(key).map_err(|_| {
                LegacyMigrationError::Invalid("legacy Hashgraph key is not UTF-8".to_owned())
            })?;
            let family =
                legacy_hashgraph_family(key).ok_or_else(|| LegacyMigrationError::Unmapped {
                    family: "hashgraph".to_owned(),
                    key: key.to_owned(),
                })?;
            *family_counts.entry(family.to_owned()).or_insert(0) += 1;
            if family == "block" {
                let index = indexed(key, "block").unwrap_or_default();
                if blocks.insert(index, value.to_vec()).is_some() {
                    return Err(LegacyMigrationError::Invalid(format!(
                        "legacy Hashgraph block {index} appears twice"
                    )));
                }
            }
        }
        let mut hasher = Sha256::new();
        hasher.update(b"ASEMAN-LEGACY-HASHGRAPH-BLOCKS-V1\0");
        for (index, value) in &blocks {
            hasher.update(index.to_be_bytes());
            hasher.update((value.len() as u64).to_be_bytes());
            hasher.update(value);
        }
        Ok(Self {
            family_counts,
            highest_block: blocks.keys().next_back().copied(),
            block_digest: hasher.finalize().into(),
        })
    }

    /// Open the separate Hashgraph RocksDB read-only and checkpoint it.
    pub fn read_only(path: &Path) -> LegacyMigrationResult<Self> {
        let mut options = Options::default();
        options.create_if_missing(false);
        let database = DB::open_for_read_only(&options, path, false)
            .map_err(|error| LegacyMigrationError::Storage(error.to_string()))?;
        let records = database
            .iterator(IteratorMode::Start)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| LegacyMigrationError::Storage(error.to_string()))?;
        Self::from_records(records.iter().map(|(key, value)| (&key[..], &value[..])))
    }
}
