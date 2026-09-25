//! Translation of `chain/node/graph.go`.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;

use super::node::Node;
use crate::hashgraph::{Block, Event, RoundInfo};

/// Information collected about a Hashgraph.
#[derive(Debug, Clone, Default)]
pub struct Infos {
    pub participant_events: HashMap<String, HashMap<String, Event>>,
    pub rounds: Vec<RoundInfo>,
    pub blocks: Vec<Block>,
}

/// Collects information about the underlying hashgraph for a visual
/// representation.
///
/// Go's `Graph` embedded `*Node`; the translation holds an `Arc<Node>`.
pub struct Graph {
    pub node: Arc<Node>,
}

impl Graph {
    /// Instantiates a `Graph` from a `Node`.
    pub fn new(node: Arc<Node>) -> Graph {
        Graph { node }
    }

    /// Returns all the events for every participant.
    pub fn get_participant_events(&self) -> Result<HashMap<String, HashMap<String, Event>>> {
        let mut res: HashMap<String, HashMap<String, Event>> = HashMap::new();
        let core = self.node.core.lock().unwrap();
        let repertoire = core.hg.store.repertoire_by_pub_key();

        for p in repertoire.values() {
            let root = core.hg.store.get_root(&p.pub_key_string())?;
            let start = {
                let root = root.lock().unwrap();
                if let Some(last) = root.events.last() {
                    last.core.index()
                } else {
                    -1
                }
            };

            let evs = core
                .hg
                .store
                .participant_events(&p.pub_key_string(), start)?;

            let mut per_participant: HashMap<String, Event> = HashMap::new();
            for e in evs {
                let event = core.hg.store.get_event(&e)?;
                per_participant.insert(event.hex(), event);
            }
            res.insert(p.pub_key_string(), per_participant);
        }
        Ok(res)
    }

    /// Returns all the recorded hashgraph rounds.
    pub fn get_rounds(&self) -> Vec<RoundInfo> {
        let mut res = Vec::new();
        let core = self.node.core.lock().unwrap();
        let mut round = 0i64;
        while round <= core.hg.store.last_round() {
            match core.hg.store.get_round(round) {
                Ok(r) => res.push(r),
                Err(_) => break,
            }
            round += 1;
        }
        res
    }

    /// Returns all the recorded blocks.
    pub fn get_blocks(&self) -> Vec<Block> {
        let mut res = Vec::new();
        let core = self.node.core.lock().unwrap();
        let mut block_idx = 0i64;
        while block_idx <= core.hg.store.last_block_index() {
            match core.hg.store.get_block(block_idx) {
                Ok(b) => res.push(b),
                Err(_) => break,
            }
            block_idx += 1;
        }
        res
    }

    /// Returns an `Infos` struct representing the entire hashgraph.
    pub fn get_infos(&self) -> Result<Infos> {
        Ok(Infos {
            participant_events: self.get_participant_events()?,
            rounds: self.get_rounds(),
            blocks: self.get_blocks(),
        })
    }
}
