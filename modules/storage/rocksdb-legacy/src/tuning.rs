//! Shared RocksDB memory tuning for every legacy store in the process (moved from
//! the node so core storage no longer names RocksDB types; the node re-exports it for
//! the OpenRaft and Hashgraph stores until RL-012/RL-011 remove them).
//!
//! Every store the node opens (the hashgraph consensus DB in
//! `network::chain::hashgraph::rocks_store`, the application key/value DB in
//! `storage`, and the cluster raft DB) used to open with bare
//! `Options::default()`. That default sets `max_open_files = -1` (unlimited):
//! RocksDB keeps a table reader open for *every* SST file for the life of the
//! process, and each reader pins that file's index and filter blocks in RAM.
//!
//! The consensus path writes a frame, round, event and block to the hashgraph
//! DB on every decided round (see `Hashgraph::process_decided_rounds` /
//! `insert_event_and_run_consensus`), and the app DB takes a write per signal.
//! Nothing is pruned, so the SST count climbs all day and the pinned
//! index/filter memory climbs with it — resident memory that only ever grows
//! (the ~1.2 GiB-over-12-h growth this addresses). heaptrack traced the bulk of
//! the retained bytes to `rocksdb::Arena::AllocateNewBlock` under those store
//! writes.
//!
//! [`tuned_options`] bounds that footprint without changing any stored data or
//! consensus behaviour:
//!   * `max_open_files` is finite, so the set of open table readers (and their
//!     pinned metadata) can't grow without bound.
//!   * Index and filter blocks are routed into a **single process-wide** LRU
//!     block cache (`shared_block_cache`) and marked evictable, so their total
//!     size is capped globally across every store rather than per-open-file.
//!   * The write buffer is given a definite size/count so memtable memory is
//!     bounded too.
//!
//! Everything is env-tunable so an operator can widen or shrink the budget
//! without a rebuild:
//!   * `CASPAR_ROCKSDB_MAX_OPEN_FILES` (default 512; negative restores the
//!     unlimited default)
//!   * `CASPAR_ROCKSDB_BLOCK_CACHE_MB`  (default 128, shared across all stores)
//!   * `CASPAR_ROCKSDB_WRITE_BUFFER_MB` (default 32, per store)

use std::sync::OnceLock;

use rocksdb::{BlockBasedOptions, Cache, Options};

fn config() -> aseman_config::LegacyAdapterConfig {
    aseman_config::legacy_adapter_snapshot().cloned().unwrap_or(
        aseman_config::LegacyAdapterConfig {
            main_port: String::new(),
            login_grant_required: false,
            public_storage_max_bytes: 10 * 1024 * 1024,
            questdb_port: 8812,
            rocksdb_max_open_files: 512,
            rocksdb_block_cache_mb: 128,
            rocksdb_write_buffer_mb: 32,
            babble_data_dir: None,
            babble_frame_cache: 25,
            babble_frame_retention: 25,
            is_head: false,
            blockchain_api_port: 1337,
            ip_address: String::new(),
            home_dir: None,
            user_profile_dir: None,
        },
    )
}

/// One LRU block cache shared by every RocksDB instance in the process, so the
/// index/filter/data blocks of all stores draw from a single bounded budget.
fn shared_block_cache() -> &'static Cache {
    static CELL: OnceLock<Cache> = OnceLock::new();
    CELL.get_or_init(|| {
        let mb = config().rocksdb_block_cache_mb.max(8);
        Cache::new_lru_cache(mb * 1024 * 1024)
    })
}

/// Build a fresh `Options` with the bounded-memory settings applied. The caller
/// adds anything store-specific (e.g. `create_if_missing`).
pub fn tuned_options() -> Options {
    let mut opts = Options::default();

    // Cap the open-table-reader set. -1 (or any negative value) keeps RocksDB's
    // unbounded default for operators who explicitly want it.
    opts.set_max_open_files(config().rocksdb_max_open_files);

    // Route index & filter blocks through the shared, bounded LRU cache and
    // make them evictable instead of pinned per open file.
    let mut bbt = BlockBasedOptions::default();
    bbt.set_block_cache(shared_block_cache());
    bbt.set_cache_index_and_filter_blocks(true);
    // Keep the L0 metadata pinned so hot reads stay fast, but everything else is
    // capped by the cache above.
    bbt.set_pin_l0_filter_and_index_blocks_in_cache(true);
    opts.set_block_based_table_factory(&bbt);

    // Bound memtable memory too (definite size × count instead of the growing
    // default), so the write side has a fixed ceiling as well.
    let wbuf_mb = config().rocksdb_write_buffer_mb.max(4);
    opts.set_write_buffer_size(wbuf_mb * 1024 * 1024);
    opts.set_max_write_buffer_number(2);

    opts
}
