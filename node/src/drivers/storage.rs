//! Translation of `drivers/storage/storage.go`.
//!
//! `Storage` implements [`IStorage`]: the key/value database is RocksDB
//! (via `rocksdb::TransactionDB`, the same family used by the hashgraph
//! store); the time-series database is QuestDB exposed through its PG-wire
//! interface, accessed via an `r2d2`-pooled `postgres` client. The Go
//! original spun on connect for QuestDB until the schema was available; the
//! translation preserves that behaviour for the `storage` table.

use std::fs;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use anyhow::{anyhow, Result};
use postgres::tls::NoTls;
use r2d2_postgres::PostgresConnectionManager;
use rocksdb::{TransactionDB, TransactionDBOptions};
use uuid::Uuid;

use crate::models::core::ICore;
use crate::models::packet::signal_tags::{decode_tags, encode_tags};
use crate::models::packet::{BuildPacket, LogPacket, LogQuery};
use crate::models::ports::storage::{IStorage, KvDb, TsDb};
use crate::models::transaction::ITrx;

/// Hard cap on rows one `read_store_logs` call may return. A store's log is
/// unbounded, so an unclamped page size would let one request pull the whole
/// history into memory.
const MAX_LOG_ROWS: i64 = 500;

/// Concrete [`IStorage`] implementation.
pub struct Storage {
    _app: Arc<dyn ICore>,
    storage_root: String,
    kvdb: KvDb,
    tsdb: TsDb,
    lock: Mutex<()>,
}

impl Storage {
    /// `NewStorage(core, storageRoot, baseDbPath, _logsDbPath,
    /// _searcherDbPath)`. The trailing arguments are kept for parity but the
    /// log/searcher tables live inside the QuestDB instance, not on disk.
    pub fn new(
        app: Arc<dyn ICore>,
        storage_root: &str,
        base_db_path: &str,
        _logs_db_path: &str,
        _searcher_db_path: &str,
        questdb_port: u16,
    ) -> Result<Arc<Storage>> {
        fs::create_dir_all(base_db_path).map_err(|e| anyhow!("mkdir {}: {}", base_db_path, e))?;
        // Bounded-memory options instead of `open_default`: this DB takes a
        // write per signal and is never fully pruned, so the default unlimited
        // `max_open_files` grew resident memory without bound as SST files
        // accumulated. See `crate::drivers::rocks_tuning`.
        let mut kv_opts = crate::drivers::rocks_tuning::tuned_options();
        kv_opts.create_if_missing(true);
        let kvdb: Arc<TransactionDB> = Arc::new(
            TransactionDB::open(&kv_opts, &TransactionDBOptions::default(), base_db_path)
                .map_err(|e| anyhow!("open kvdb {}: {}", base_db_path, e))?,
        );

        let conn_str = format!(
            "host=localhost port={} user=admin password=quest dbname=qdb sslmode=disable",
            questdb_port
        );
        let manager = PostgresConnectionManager::new(
            conn_str
                .parse()
                .map_err(|e| anyhow!("parse pg config: {}", e))?,
            NoTls,
        );
        // Bound the wait for a connection. Signal persistence runs INSIDE a state
        // modification (a store signal must be recorded before it is fanned out),
        // so a sick or overloaded QuestDB must fail the one request quickly
        // rather than hold the node's state lock for r2d2's 30-second default —
        // which would stall every other action on the node behind it.
        let tsdb = r2d2::Pool::builder()
            .connection_timeout(Duration::from_secs(3))
            .build(manager)
            .map_err(|e| anyhow!("pool: {}", e))?;

        // Mirror Go's "retry until storage table is creatable" loop.
        loop {
            let mut client = match tsdb.get() {
                Ok(c) => c,
                Err(_) => {
                    thread::sleep(Duration::from_secs(2));
                    continue;
                }
            };
            match client.execute(
                "create table if not exists storage(id text, store_id text, user_id text, data text, tags text, time bigint, edited boolean);",
                &[],
            ) {
                Ok(_) => break,
                Err(e) => {
                    eprintln!("create storage table: {}", e);
                    thread::sleep(Duration::from_secs(2));
                }
            }
        }
        {
            // A node created before signal tags existed has a `storage` table
            // without the column; add it so every insert below has a target.
            // Idempotent, and the create above already includes it on a fresh
            // node.
            //
            // The ALTER is allowed to fail (the column may already exist, and
            // not every QuestDB build words that the same way), but the RESULT
            // is not taken on faith: the column is then read back, and a node
            // that would silently drop every signal it is asked to persist
            // refuses to start instead.
            let mut client = tsdb.get().map_err(|e| anyhow!("pool get: {}", e))?;
            let _ = client.execute(
                "alter table storage add column if not exists tags text;",
                &[],
            );
            // QuestDB can report CREATE IF NOT EXISTS as success (or "already
            // exists") for a ghost table that SELECT then cannot see. ALTER
            // cannot fix that. Drop and create for real, then require SELECT.
            if client
                .query("select tags from storage limit 1", &[])
                .is_err()
            {
                let _ = client.execute("drop table if exists storage;", &[]);
                client
                    .execute(
                        "create table storage(id text, store_id text, user_id text, data text, tags text, time bigint, edited boolean);",
                        &[],
                    )
                    .map_err(|e| anyhow!("recreate storage table: {e}"))?;
            }
            client
                .query("select tags from storage limit 1", &[])
                .map_err(|e| {
                    anyhow!(
                        "storage table has no usable `tags` column ({e}). Signal \
                         persistence would silently drop every message, so the node \
                         will not start."
                    )
                })?;
        }
        {
            let mut client = tsdb.get().map_err(|e| anyhow!("pool get: {}", e))?;
            client
                .execute(
                    "create table if not exists buildlogs(id text, build_id text, machine_id text, vm_id text, log_type text, data text, time bigint);",
                    &[],
                )
                .map_err(|e| anyhow!("create buildlogs: {}", e))?;
            let _ = client.execute(
                "alter table buildlogs add column if not exists vm_id text;",
                &[],
            );
            let _ = client.execute(
                "alter table buildlogs add column if not exists log_type text;",
                &[],
            );
            let _ = client.execute(
                "alter table buildlogs add column if not exists time bigint;",
                &[],
            );
        }

        Ok(Arc::new(Storage {
            _app: app,
            storage_root: storage_root.to_string(),
            kvdb,
            tsdb,
            lock: Mutex::new(()),
        }))
    }
}

impl IStorage for Storage {
    fn storage_root(&self) -> String {
        self.storage_root.clone()
    }

    fn kv_db(&self) -> KvDb {
        self.kvdb.clone()
    }

    fn ts_db(&self) -> TsDb {
        self.tsdb.clone()
    }

    fn gen_id(&self, t: &dyn ITrx, origin: &str) -> String {
        // This mutex exists ONLY to make the id-counter read-modify-write below
        // atomic across concurrent callers. It must NOT be taken by the QuestDB
        // (tsdb) log/read helpers: holding it across a blocking QuestDB round
        // trip serialises every id mint behind log I/O, so a log-flooding VM
        // could starve createMachine/createProgram into a request timeout.
        let _guard = self.lock.lock().unwrap();
        if origin == "global" {
            let bytes = t.get_bytes("globalIdCounter");
            let mut counter: i64 = if bytes.is_empty() {
                0
            } else if bytes.len() >= 8 {
                i64::from_be_bytes(bytes[..8].try_into().unwrap())
            } else {
                0
            };
            counter += 1;
            t.put_bytes("globalIdCounter", counter.to_be_bytes().to_vec());
            format!("{}@{}", counter, origin)
        } else {
            // Use the kvdb directly when origin != "global", matching Go.
            let key = b"localIdCounter";
            let counter = {
                let val = self.kvdb.get(key).unwrap_or_default().unwrap_or_default();
                let mut counter: i64 = if val.len() >= 8 {
                    i64::from_be_bytes(val[..8].try_into().unwrap())
                } else {
                    0
                };
                counter += 1;
                let _ = self.kvdb.put(key, counter.to_be_bytes());
                counter
            };
            format!("{}@{}", counter, origin)
        }
    }

    fn log_time_sieries(
        &self,
        store_id: &str,
        user_id: &str,
        data: &str,
        tags: &[String],
        time_val: i64,
    ) -> Result<LogPacket> {
        // No storage.lock here: the tsdb pool is thread-safe and QuestDB handles
        // concurrent inserts; the lock is reserved for gen_id's counter (see there).
        //
        // A failed insert is returned, never swallowed. This row IS the message:
        // reporting success for a write that did not happen would show the sender
        // a message that quietly disappears on their next reload, which is far
        // worse than telling them the send failed.
        let id = Uuid::new_v4().to_string();
        let encoded = encode_tags(tags);
        let mut client = self
            .tsdb
            .get()
            .map_err(|e| anyhow!("signal log unavailable: {}", e))?;
        client
            .execute(
                "INSERT INTO storage (id, store_id, user_id, data, tags, time, edited) VALUES ($1, $2, $3, $4, $5, $6, $7)",
                &[&id, &store_id, &user_id, &data, &encoded, &time_val, &false],
            )
            .map_err(|e| anyhow!("signal log write failed: {}", e))?;
        Ok(LogPacket {
            id,
            user_id: user_id.to_string(),
            data: data.to_string(),
            store_id: store_id.to_string(),
            tags: tags.to_vec(),
            time: time_val,
            edited: false,
        })
    }

    fn update_log(
        &self,
        store_id: &str,
        user_id: &str,
        signal_id: &str,
        data: &str,
        time_val: i64,
    ) -> LogPacket {
        // No storage.lock (tsdb pool is thread-safe; lock is gen_id-only).
        if let Ok(mut client) = self.tsdb.get() {
            let _ = client.execute(
                "update storage set data = $1 where store_id = $2 and id = $3 and edited = $4",
                &[&data, &store_id, &signal_id, &true],
            );
        }
        LogPacket {
            id: signal_id.to_string(),
            user_id: user_id.to_string(),
            data: data.to_string(),
            store_id: store_id.to_string(),
            // Tags are immutable: an edit rewrites the payload, never the labels
            // a reader filtered on to find the packet in the first place.
            tags: Vec::new(),
            time: time_val,
            edited: true,
        }
    }

    fn read_store_logs(&self, store_id: &str, query: &LogQuery) -> Result<Vec<LogPacket>> {
        // No storage.lock (tsdb pool is thread-safe; lock is gen_id-only).
        //
        // An unreachable or failing log is an ERROR, not an empty conversation.
        // Returning `[]` here would render a space that has plenty of history as
        // a blank chat, which reads as data loss and hides the real fault.
        let mut client = self
            .tsdb
            .get()
            .map_err(|e| anyhow!("signal log unavailable: {}", e))?;
        let count = if query.count <= 0 || query.count > MAX_LOG_ROWS {
            MAX_LOG_ROWS
        } else {
            query.count
        };
        // Tag predicates are inlined rather than bound, because they are
        // composed into a variable-length `LIKE` list. That is injection-safe
        // only because every tag is validated first (no quote, no separator, no
        // wildcard — see signal_tags), so reject the whole read if one is not.
        let mut clauses: Vec<String> = vec!["store_id = $1".to_string()];
        for tag in &query.tags_all {
            crate::models::packet::validate_tag(tag)?;
            clauses.push(format!("tags LIKE '%|{}|%'", tag));
        }
        if !query.tags_any.is_empty() {
            let mut anys: Vec<String> = Vec::with_capacity(query.tags_any.len());
            for tag in &query.tags_any {
                crate::models::packet::validate_tag(tag)?;
                anys.push(format!("tags LIKE '%|{}|%'", tag));
            }
            clauses.push(format!("({})", anys.join(" OR ")));
        }
        if query.before_time > 0 {
            clauses.push(format!("time < {}", query.before_time));
        }
        if query.after_time > 0 {
            clauses.push(format!("time > {}", query.after_time));
        }
        // QuestDB takes no bound parameter in LIMIT, so the (validated i64)
        // count is inlined the same way read_vm_logs does it.
        let sql = format!(
            "SELECT id, user_id, data, tags, time, edited FROM storage WHERE {} order by time desc limit {}",
            clauses.join(" AND "),
            count
        );
        let rows = client
            .query(&sql, &[&store_id])
            .map_err(|e| anyhow!("signal log read failed: {}", e))?;
        Ok(rows
            .into_iter()
            .map(|row| {
                let tags: Option<String> = row.get(3);
                LogPacket {
                    id: row.get(0),
                    user_id: row.get(1),
                    data: row.get(2),
                    store_id: store_id.to_string(),
                    tags: decode_tags(tags.as_deref().unwrap_or("")),
                    time: row.get(4),
                    edited: row.get(5),
                }
            })
            .collect())
    }

    fn pick_store_logs(&self, store_id: &str, ids: Vec<String>) -> Vec<LogPacket> {
        if ids.is_empty() {
            return Vec::new();
        }
        let mut client = match self.tsdb.get() {
            Ok(c) => c,
            Err(_) => return Vec::new(),
        };
        // QuestDB's PG-wire doesn't support arrays in $-params universally;
        // inline the id list, matching Go's `strings.Join` formatting.
        let quoted = ids
            .iter()
            .map(|s| format!("'{}'", s.replace('\'', "''")))
            .collect::<Vec<_>>()
            .join(",");
        let query = format!(
            "SELECT id, user_id, data, tags, time, edited FROM storage WHERE store_id = $1 and id in ({})",
            quoted
        );
        let rows = match client.query(&query, &[&store_id]) {
            Ok(r) => r,
            Err(_) => return Vec::new(),
        };
        rows.into_iter()
            .map(|row| {
                let tags: Option<String> = row.get(3);
                LogPacket {
                    id: row.get(0),
                    user_id: row.get(1),
                    data: row.get(2),
                    store_id: store_id.to_string(),
                    tags: decode_tags(tags.as_deref().unwrap_or("")),
                    time: row.get(4),
                    edited: row.get(5),
                }
            })
            .collect()
    }

    fn log_vm(&self, vm_id: &str, log_type: &str, data: &str, time_val: i64) -> BuildPacket {
        // No storage.lock: a running VM streams many log lines through here, and
        // holding the gen_id counter mutex across each blocking QuestDB insert
        // would stall creature/program creation for other callers (the tsdb pool
        // is thread-safe and QuestDB handles concurrent inserts).
        let id = Uuid::new_v4().to_string();
        let log_type = if log_type.is_empty() {
            "runtime"
        } else {
            log_type
        };
        let time_val = if time_val == 0 {
            chrono::Utc::now().timestamp_millis()
        } else {
            time_val
        };
        if let Ok(mut client) = self.tsdb.get() {
            let _ = client.execute(
                "INSERT INTO buildlogs (id, build_id, machine_id, vm_id, log_type, data, time) VALUES ($1, $2, $3, $4, $5, $6, $7)",
                &[&id, &"", &"", &vm_id, &log_type, &data, &time_val],
            );
        }
        BuildPacket {
            id,
            build_id: String::new(),
            creature_id: String::new(),
            vm_id: vm_id.to_string(),
            log_type: log_type.to_string(),
            time: time_val,
            data: data.to_string(),
        }
    }

    fn read_vm_logs(
        &self,
        vm_id: &str,
        log_type: &str,
        offset: i64,
        count: i64,
    ) -> Vec<BuildPacket> {
        // No storage.lock (tsdb pool is thread-safe; lock is gen_id-only).
        let count = if count <= 0 { 100 } else { count };
        let offset = offset.max(0);
        let mut client = match self.tsdb.get() {
            Ok(c) => c,
            Err(_) => return Vec::new(),
        };
        // QuestDB does not support the `LIMIT n OFFSET m` form (it rejects the
        // OFFSET token) nor bound integer params in LIMIT; it uses `LIMIT lo, hi`
        // where `lo` rows are skipped and rows up to `hi` are returned. Translate
        // offset/count into that range inline (both are validated i64s, so this
        // is injection-safe). NOTE: lo is the skip count (offset), not offset+1 —
        // with ORDER BY time DESC, offset+1 would drop the most-recent row.
        let lo = offset;
        let hi = offset + count;
        let rows = if log_type.is_empty() {
            client.query(
                &format!(
                    "SELECT id, build_id, machine_id, vm_id, log_type, data, time FROM buildlogs WHERE vm_id = $1 ORDER BY time DESC LIMIT {}, {}",
                    lo, hi
                ),
                &[&vm_id],
            )
        } else {
            client.query(
                &format!(
                    "SELECT id, build_id, machine_id, vm_id, log_type, data, time FROM buildlogs WHERE vm_id = $1 AND log_type = $2 ORDER BY time DESC LIMIT {}, {}",
                    lo, hi
                ),
                &[&vm_id, &log_type],
            )
        };
        let rows = match rows {
            Ok(r) => r,
            Err(_) => return Vec::new(),
        };
        rows.into_iter()
            .map(|row| BuildPacket {
                id: row.get(0),
                build_id: row.get(1),
                creature_id: row.get(2),
                vm_id: row.get(3),
                log_type: row.get(4),
                data: row.get(5),
                time: row.get(6),
            })
            .collect()
    }
}
