//! Chains and their shards (ADR 0036): the `core.chain` and `core.chain_shard`
//! models, keyed by chain id, in their legacy wire shapes.

use anyhow::Result;
use aseman_storage::client::core::{chain, chain_shard};
use aseman_storage::{FindMany, Models};
use serde::{Deserialize, Serialize};

use crate::core::trx::{Trx, failed};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Chain {
    #[serde(default)]
    pub id: String,
    #[serde(rename = "storeId", default)]
    pub store_id: String,
}

impl Chain {
    /// Record the chain (idempotent).
    pub fn save(&self, trx: &Trx) -> Result<()> {
        trx.chain()
            .upsert(
                chain::by_key(self.id.clone()),
                chain::Create {
                    key: self.id.clone(),
                    store_id: Some(self.store_id.clone()),
                    status: Some("active".to_owned()),
                },
                chain::update().store_id(Some(self.store_id.clone())),
            )
            .map(drop)
            .map_err(failed)
    }

    /// Every recorded chain, by id.
    pub fn all(trx: &Trx) -> Result<Vec<Chain>> {
        Ok(trx
            .chain()
            .find_many(FindMany::default().order_by(chain::key().asc()))
            .map_err(failed)?
            .into_iter()
            .map(|row| Chain {
                id: row.key,
                store_id: row.store_id.unwrap_or_default(),
            })
            .collect())
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChainShard {
    #[serde(default)]
    pub id: String,
    #[serde(rename = "workChainId", default)]
    pub work_chain_id: String,
}

impl ChainShard {
    /// Record the shard (idempotent).
    pub fn save(&self, trx: &Trx) -> Result<()> {
        trx.chain_shard()
            .upsert(
                chain_shard::by_key(self.id.clone()),
                chain_shard::Create {
                    key: self.id.clone(),
                    work_chain_id: Some(self.work_chain_id.clone()),
                    shard_name: Some(self.id.clone()),
                },
                chain_shard::update().work_chain_id(Some(self.work_chain_id.clone())),
            )
            .map(drop)
            .map_err(failed)
    }

    /// Every recorded shard, by work chain then id.
    pub fn all(trx: &Trx) -> Result<Vec<ChainShard>> {
        Ok(trx
            .chain_shard()
            .find_many(
                FindMany::default()
                    .order_by(chain_shard::work_chain_id().asc())
                    .order_by(chain_shard::key().asc()),
            )
            .map_err(failed)?
            .into_iter()
            .map(|row| ChainShard {
                id: row.key,
                work_chain_id: row.work_chain_id.unwrap_or_default(),
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chains_and_shards_round_trip() {
        let trx = crate::core::trx::test_trx();
        Chain {
            id: "c1".into(),
            store_id: "s1".into(),
        }
        .save(&trx)
        .unwrap();
        for (id, work) in [("sh2", "c1"), ("sh1", "c1"), ("sh0", "c0")] {
            ChainShard {
                id: id.into(),
                work_chain_id: work.into(),
            }
            .save(&trx)
            .unwrap();
        }
        assert_eq!(Chain::all(&trx).unwrap()[0].store_id, "s1");
        let shards = ChainShard::all(&trx).unwrap();
        assert_eq!(
            shards.iter().map(|shard| shard.id.as_str()).collect::<Vec<_>>(),
            ["sh0", "sh1", "sh2"]
        );
    }
}
