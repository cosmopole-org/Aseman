//! PostgreSQL implementation of the transitional [`ITrx`] contract.
//!
//! This adapter is intentionally separate from the RocksDB adapter.  It translates the
//! compatibility API into normalized PostgreSQL object, index, relation, document, and
//! opaque-value operations supplied by `aseman-storage-postgres`.  It is available for
//! conformance/backfill work but is not selected as authority until the removal-ledger
//! cutover gate passes.

#![cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "ADR 0031: backfill/comparison surface exercised by tests until a family cutover composes it"
    )
)]

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

use anyhow::{Result, anyhow};
use aseman_storage_postgres::compatibility::{
    PostgresCompatibilityTransaction, PostgresCompatibilityTransactionFactory,
};
use serde_json::{Map, Value};

use crate::models::transaction::ITrx;
use crate::models::update::Update;

/// Reusable pool/factory for PostgreSQL compatibility transactions.
pub struct PostgresTrxFactory {
    inner: PostgresCompatibilityTransactionFactory,
}

impl PostgresTrxFactory {
    pub fn connect(connection_uri: &str, max_connections: u32) -> Result<Self> {
        Ok(Self {
            inner: PostgresCompatibilityTransactionFactory::connect(
                connection_uri,
                max_connections,
            )?,
        })
    }

    pub fn begin(&self, readonly: bool) -> Result<Arc<PostgresTrx>> {
        Ok(Arc::new(PostgresTrx {
            transaction: self.inner.begin(readonly)?,
            state: Mutex::new(TransactionState::default()),
        }))
    }
}

/// One PostgreSQL-backed compatibility transaction.
pub struct PostgresTrx {
    transaction: PostgresCompatibilityTransaction,
    state: Mutex<TransactionState>,
}

#[derive(Default)]
struct TransactionState {
    changes: Vec<Update>,
    first_error: Option<String>,
    finalized: bool,
}

impl PostgresTrx {
    fn fail(&self, error: impl ToString) {
        let mut state = self.state.lock().unwrap();
        if state.first_error.is_none() {
            state.first_error = Some(error.to_string());
        }
    }

    fn read<T>(&self, result: aseman_storage_postgres::StorageResult<T>) -> Option<T> {
        match result {
            Ok(value) => Some(value),
            Err(error) => {
                self.fail(error);
                None
            }
        }
    }

    fn changed(&self, typ: &str, key: String, value: Vec<u8>) {
        self.state.lock().unwrap().changes.push(Update {
            typ: typ.to_owned(),
            key,
            val: value,
        });
    }

    fn write(
        &self,
        result: aseman_storage_postgres::StorageResult<()>,
        typ: &str,
        key: String,
        value: Vec<u8>,
    ) {
        match result {
            Ok(()) => self.changed(typ, key, value),
            Err(error) => self.fail(error),
        }
    }

    fn value(&self, key: &str) -> Vec<u8> {
        self.read(self.transaction.value(key))
            .flatten()
            .unwrap_or_default()
    }

    fn keys(&self, prefix: &str) -> Vec<String> {
        self.read(self.transaction.keys_with_prefix(prefix))
            .unwrap_or_default()
    }

    fn index_json(&self, key: &str, path: &str, object: &Map<String, Value>, merge: bool) {
        let mut document = if merge {
            self.transaction
                .document(key, path)
                .ok()
                .flatten()
                .and_then(|value| value.as_object().cloned())
                .unwrap_or_default()
        } else {
            Map::new()
        };
        merge_objects(&mut document, object);
        self.put_document_value(key, path, Value::Object(document));

        let mut fields = object.keys().collect::<Vec<_>>();
        fields.sort();
        for field in fields {
            let value = &object[field];
            if value.is_null() {
                continue;
            }
            let child_path = format!("{path}.{field}");
            if let Value::Object(child) = value {
                self.index_json(key, &child_path, child, merge);
            } else {
                self.put_document_value(key, &child_path, value.clone());
            }
        }
    }

    fn put_document_value(&self, key: &str, path: &str, value: Value) {
        let physical = format!("json::{key}::{path}");
        let bytes = serde_json::to_vec(&value).unwrap_or_default();
        self.write(
            self.transaction.put_document(key, path, &value),
            "put",
            physical,
            bytes,
        );
    }

    fn put_raw_value(&self, key: &str, value: Vec<u8>) {
        if let Some((kind, object_id, column)) = parse_object_key(key) {
            self.write(
                self.transaction
                    .put_object_column(kind, object_id, column, &value),
                "put",
                key.to_owned(),
                value,
            );
            return;
        }
        if let Some((kind, from, to, from_value)) = parse_index_key(key) {
            self.write(
                self.transaction
                    .put_secondary_index(kind, from, to, from_value, &value),
                "put",
                key.to_owned(),
                value,
            );
            return;
        }
        if let Some(logical_key) = key.strip_prefix("link::") {
            let Ok(text) = String::from_utf8(value.clone()) else {
                self.fail(format!("relation value for {key} is not UTF-8"));
                return;
            };
            let (relation_type, scope, member) = relation_parts(logical_key);
            self.write(
                self.transaction
                    .put_relation(&relation_type, &scope, &member, logical_key, &text),
                "put",
                key.to_owned(),
                value,
            );
            return;
        }
        if let Some((document_key, path)) = parse_document_key(key) {
            let Ok(document) = serde_json::from_slice(&value) else {
                self.fail(format!("document value for {key} is not valid JSON"));
                return;
            };
            self.write(
                self.transaction.put_document(document_key, path, &document),
                "put",
                key.to_owned(),
                value,
            );
            return;
        }
        self.write(
            self.transaction.put_opaque(key, &value),
            "put",
            key.to_owned(),
            value,
        );
    }
}

impl ITrx for PostgresTrx {
    fn commit(&self) -> Result<()> {
        let error = {
            let mut state = self.state.lock().unwrap();
            if state.finalized {
                return Ok(());
            }
            state.finalized = true;
            state.first_error.clone()
        };
        if let Some(error) = error {
            let _ = self.transaction.rollback();
            return Err(anyhow!("state commit failed: {error}"));
        }
        self.transaction
            .commit()
            .map_err(|error| anyhow!("state commit failed: {error}"))
    }

    fn discard(&self) {
        let should_rollback = {
            let mut state = self.state.lock().unwrap();
            if state.finalized {
                false
            } else {
                state.finalized = true;
                true
            }
        };
        if should_rollback && let Err(error) = self.transaction.rollback() {
            self.fail(error);
        }
    }

    fn del_key(&self, key: &str) {
        self.write(
            self.transaction.delete_key(key),
            "del",
            key.to_owned(),
            Vec::new(),
        );
    }

    fn get_by_prefix(&self, prefix: &str) -> Vec<String> {
        self.keys(prefix)
    }

    fn has_obj(&self, typ: &str, key: &str) -> bool {
        !self.value(&format!("obj::{typ}::{key}::|")).is_empty()
    }

    fn get_index(
        &self,
        typ: &str,
        from_column: &str,
        to_column: &str,
        from_column_val: &str,
    ) -> String {
        String::from_utf8_lossy(&self.value(&index_key(
            typ,
            from_column,
            to_column,
            from_column_val,
        )))
        .into_owned()
    }

    fn put_index(
        &self,
        typ: &str,
        from_column: &str,
        to_column: &str,
        from_column_val: &str,
        to_column_val: Vec<u8>,
    ) {
        let key = index_key(typ, from_column, to_column, from_column_val);
        self.write(
            self.transaction.put_secondary_index(
                typ,
                from_column,
                to_column,
                from_column_val,
                &to_column_val,
            ),
            "put",
            key,
            to_column_val,
        );
    }

    fn del_index(&self, typ: &str, from: &str, to: &str, from_value: &str) {
        self.del_key(&index_key(typ, from, to, from_value));
    }

    fn has_index(&self, typ: &str, from: &str, to: &str, from_value: &str) -> bool {
        self.read(
            self.transaction
                .value(&index_key(typ, from, to, from_value)),
        )
        .flatten()
        .is_some()
    }

    fn get_column(&self, typ: &str, object_id: &str, column: &str) -> Vec<u8> {
        self.value(&format!("obj::{typ}::{object_id}::{column}"))
    }

    fn get_links_list(
        &self,
        prefix: &str,
        _offset: i64,
        _count: i64,
        should_be_global: &[bool],
    ) -> Result<Vec<String>> {
        let global = matches!(should_be_global.first(), Some(true));
        Ok(self
            .keys(&format!("link::{prefix}"))
            .into_iter()
            .filter(|key| !global || key.ends_with("@global"))
            .filter_map(|key| key.strip_prefix("link::").map(str::to_owned))
            .collect())
    }

    fn search_link_vals_list(
        &self,
        typ: &str,
        from_column: &str,
        to_column: &str,
        word: &str,
        filter: &HashMap<String, String>,
        offset: i64,
        count: i64,
    ) -> Result<Vec<String>> {
        let mut matched = Vec::new();
        let entries = self
            .read(
                self.transaction
                    .secondary_index_entries(typ, from_column, to_column),
            )
            .unwrap_or_default();
        for (indexed, bytes) in entries {
            if !indexed.contains(word) {
                continue;
            }
            let value = String::from_utf8_lossy(&bytes).into_owned();
            if columns_match(&self.get_obj(typ, &value), filter, &HashMap::new()) {
                matched.push(value);
            }
        }
        Ok(window(matched, offset, count))
    }

    fn search_link_keys_list_by_prefix(
        &self,
        prefix: &str,
        typ: &str,
        filter: &HashMap<String, String>,
        in_arr_filter: &HashMap<String, Vec<String>>,
        offset: i64,
        count: i64,
        should_be_global: &[bool],
    ) -> Result<Vec<String>> {
        let full = format!("link::{prefix}");
        let global = matches!(should_be_global.first(), Some(true));
        let matched = self
            .keys(&full)
            .into_iter()
            .filter(|key| !global || key.ends_with("@global"))
            .filter_map(|key| key.strip_prefix(&full).map(str::to_owned))
            .filter(|object_id| columns_match(&self.get_obj(typ, object_id), filter, in_arr_filter))
            .collect();
        Ok(window(matched, offset, count))
    }

    fn get_obj_list(
        &self,
        typ: &str,
        object_ids: &[String],
        query: &HashMap<String, String>,
        meta: &[i64],
    ) -> Result<HashMap<String, HashMap<String, Vec<u8>>>> {
        if !(object_ids.len() == 1 && object_ids[0] == "*") {
            return Ok(object_ids
                .iter()
                .map(|id| (id.clone(), self.get_obj(typ, id)))
                .collect());
        }
        let mut grouped: BTreeMap<String, HashMap<String, Vec<u8>>> = BTreeMap::new();
        let rows = self
            .read(self.transaction.object_kind_columns(typ))
            .unwrap_or_default();
        for (id, column, value) in rows {
            grouped.entry(id).or_default().insert(column, value);
        }
        let offset = meta.first().copied().unwrap_or(0).max(0) as usize;
        let count = meta.get(1).copied().map(|value| value.max(0) as usize);
        let records = grouped
            .into_iter()
            .filter(|(_, columns)| {
                columns.contains_key("|")
                    && query.iter().all(|(column, expected)| {
                        columns.get(column).is_some_and(|value| {
                            String::from_utf8_lossy(value) == expected.as_str()
                        })
                    })
            })
            .skip(offset);
        Ok(match count {
            Some(count) => records.take(count).collect(),
            None => records.collect(),
        })
    }

    fn get_link(&self, key: &str) -> String {
        String::from_utf8_lossy(&self.value(&format!("link::{key}"))).into_owned()
    }

    fn put_link(&self, key: &str, value: &str) {
        let (relation_type, scope, member) = relation_parts(key);
        self.write(
            self.transaction
                .put_relation(&relation_type, &scope, &member, key, value),
            "put",
            format!("link::{key}"),
            value.as_bytes().to_vec(),
        );
    }

    fn put_bytes(&self, key: &str, value: Vec<u8>) {
        self.put_raw_value(key, value);
    }

    fn get_bytes(&self, key: &str) -> Vec<u8> {
        self.value(key)
    }

    fn put_string(&self, key: &str, value: &str) {
        self.put_bytes(key, value.as_bytes().to_vec());
    }

    fn get_string(&self, key: &str) -> String {
        String::from_utf8_lossy(&self.value(key)).into_owned()
    }

    fn get_obj(&self, typ: &str, key: &str) -> HashMap<String, Vec<u8>> {
        self.read(self.transaction.object_columns(typ, key))
            .unwrap_or_default()
            .into_iter()
            .collect()
    }

    fn put_obj(&self, typ: &str, key: &str, mut columns: HashMap<String, Vec<u8>>) {
        columns.insert("|".to_owned(), vec![1]);
        for (column, value) in columns {
            let physical = format!("obj::{typ}::{key}::{column}");
            self.write(
                self.transaction
                    .put_object_column(typ, key, &column, &value),
                "put",
                physical,
                value,
            );
        }
    }

    fn put_json(&self, key: &str, path: &str, value: &Value, merge: bool) -> Result<()> {
        let object = value
            .as_object()
            .ok_or_else(|| anyhow!("put_json expects an object at the root"))?;
        self.index_json(key, path, object, merge);
        Ok(())
    }

    fn del_json(&self, key: &str, path: &str) {
        let root = format!("json::{key}::{path}");
        let mut keys = self.keys(&format!("{root}."));
        if self.read(self.transaction.value(&root)).flatten().is_some() {
            keys.insert(0, root.clone());
        }
        match self.transaction.delete_document_tree(key, path) {
            Ok(()) => {
                if keys.is_empty() {
                    self.changed("del", root, Vec::new());
                } else {
                    for key in keys {
                        self.changed("del", key, Vec::new());
                    }
                }
            }
            Err(error) => self.fail(error),
        }
    }

    fn get_json(&self, key: &str, path: &str) -> Result<Map<String, Value>> {
        self.read(self.transaction.document(key, path))
            .flatten()
            .and_then(|value| value.as_object().cloned())
            .ok_or_else(|| anyhow!("json path not found"))
    }

    fn updates(&self) -> Vec<Update> {
        self.state.lock().unwrap().changes.clone()
    }
}

fn index_key(typ: &str, from: &str, to: &str, value: &str) -> String {
    format!("index::{typ}::{from}::{to}::{value}")
}

fn parse_object_key(key: &str) -> Option<(&str, &str, &str)> {
    let mut parts = key.strip_prefix("obj::")?.splitn(3, "::");
    Some((parts.next()?, parts.next()?, parts.next()?))
}

fn parse_index_key(key: &str) -> Option<(&str, &str, &str, &str)> {
    let mut parts = key.strip_prefix("index::")?.splitn(4, "::");
    Some((parts.next()?, parts.next()?, parts.next()?, parts.next()?))
}

fn parse_document_key(key: &str) -> Option<(&str, &str)> {
    key.strip_prefix("json::")?.rsplit_once("::")
}

fn relation_parts(key: &str) -> (String, String, String) {
    let parts = key.split("::").collect::<Vec<_>>();
    let relation_type = parts.first().copied().unwrap_or_default().to_owned();
    let member = parts
        .get(1..)
        .and_then(|tail| tail.last())
        .copied()
        .unwrap_or_default()
        .to_owned();
    let scope = if parts.len() > 2 {
        parts[1..parts.len() - 1].join("::")
    } else {
        String::new()
    };
    (relation_type, scope, member)
}

fn columns_match(
    columns: &HashMap<String, Vec<u8>>,
    exact: &HashMap<String, String>,
    one_of: &HashMap<String, Vec<String>>,
) -> bool {
    exact.iter().all(|(column, expected)| {
        columns
            .get(column)
            .is_some_and(|value| String::from_utf8_lossy(value) == expected.as_str())
    }) && one_of.iter().all(|(column, values)| {
        columns.get(column).is_some_and(|bytes| {
            let actual = String::from_utf8_lossy(bytes);
            values.iter().any(|expected| expected == actual.as_ref())
        })
    })
}

fn window(values: Vec<String>, offset: i64, count: i64) -> Vec<String> {
    values
        .into_iter()
        .skip(offset.max(0) as usize)
        .take(count.max(0) as usize)
        .collect()
}

fn merge_objects(destination: &mut Map<String, Value>, source: &Map<String, Value>) {
    for (key, value) in source {
        match (destination.get_mut(key), value) {
            (Some(Value::Object(destination)), Value::Object(source)) => {
                merge_objects(destination, source);
            }
            _ => {
                destination.insert(key.clone(), value.clone());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use aseman_storage_postgres::PostgresCapsuleRepository;
    use postgres::{Client, Config, NoTls};

    use super::*;

    #[test]
    fn relations_are_split_into_indexable_groups() {
        assert_eq!(
            relation_parts("FinanceJournalByUser::alice::00012::j1"),
            (
                "FinanceJournalByUser".to_owned(),
                "alice::00012".to_owned(),
                "j1".to_owned()
            )
        );
        assert_eq!(
            relation_parts("UserEmailToId::a@example.test"),
            (
                "UserEmailToId".to_owned(),
                String::new(),
                "a@example.test".to_owned()
            )
        );
    }

    #[test]
    fn physical_keys_are_classified_without_a_shared_kv_table() {
        assert_eq!(
            parse_object_key("obj::Creature::alice::balance"),
            Some(("Creature", "alice", "balance"))
        );
        assert_eq!(
            parse_index_key("index::Creature::username::id::alice"),
            Some(("Creature", "username", "id", "alice"))
        );
        assert_eq!(
            parse_document_key("json::Json::Creature::alice::metadata.name"),
            Some(("Json::Creature::alice", "metadata.name"))
        );
    }

    #[test]
    fn document_merge_is_recursive() {
        let mut destination = serde_json::json!({"a": {"b": 1}, "keep": true})
            .as_object()
            .unwrap()
            .clone();
        let source = serde_json::json!({"a": {"c": 2}});
        merge_objects(&mut destination, source.as_object().unwrap());
        assert_eq!(
            Value::Object(destination),
            serde_json::json!({"a": {"b": 1, "c": 2}, "keep": true})
        );
    }

    #[test]
    fn live_adapter_preserves_the_transaction_surface() {
        let Some(admin_uri) = aseman_config::IntegrationTestConfig::from_process().postgres_url
        else {
            eprintln!("ASEMAN_TEST_POSTGRES_URL is absent; skipping PostgreSQL trx test");
            return;
        };
        let database = format!("aseman_node_trx_{}", uuid::Uuid::now_v7().simple());
        let mut admin = Client::connect(&admin_uri, NoTls).unwrap();
        admin
            .batch_execute(&format!("CREATE DATABASE {database}"))
            .unwrap();
        let mut config = Config::from_str(&admin_uri).unwrap();
        config.dbname(&database);
        let repository = PostgresCapsuleRepository::from_client(config.connect(NoTls).unwrap());
        repository.migrate().unwrap();
        let (scheme, rest) = admin_uri.split_once("://").unwrap();
        let authority = rest.split('/').next().unwrap();
        let uri = format!("{scheme}://{authority}/{database}");
        let factory = PostgresTrxFactory::connect(&uri, 3).unwrap();

        let transaction = factory.begin(false).unwrap();
        transaction.put_obj(
            "Creature",
            "alice",
            HashMap::from([
                ("name".to_owned(), b"Alice".to_vec()),
                ("balance".to_owned(), 7_i64.to_le_bytes().to_vec()),
            ]),
        );
        // Raw writes to a structured physical key are classified into the object table.
        transaction.put_bytes(
            "obj::Creature::alice::balance",
            9_i64.to_le_bytes().to_vec(),
        );
        transaction.put_index("Creature", "username", "id", "alice", b"alice".to_vec());
        transaction.put_link("member::store-1::alice", "read");
        transaction
            .put_json(
                "Json::Creature::alice",
                "metadata",
                &serde_json::json!({"profile": {"name": "Alice"}}),
                false,
            )
            .unwrap();
        transaction.put_string("counter", "1");

        assert!(transaction.has_obj("Creature", "alice"));
        assert_eq!(
            transaction.get_index("Creature", "username", "id", "alice"),
            "alice"
        );
        assert_eq!(transaction.get_link("member::store-1::alice"), "read");
        assert_eq!(
            transaction
                .get_json("Json::Creature::alice", "metadata")
                .unwrap(),
            serde_json::json!({"profile": {"name": "Alice"}})
                .as_object()
                .unwrap()
                .clone()
        );
        assert!(!transaction.updates().is_empty());
        transaction.commit().unwrap();

        let transaction = factory.begin(false).unwrap();
        assert_eq!(
            transaction.get_column("Creature", "alice", "balance"),
            9_i64.to_le_bytes()
        );
        transaction.del_key("link::member::store-1::alice");
        transaction.discard();

        let transaction = factory.begin(false).unwrap();
        assert_eq!(transaction.get_link("member::store-1::alice"), "read");
        transaction.discard();

        drop(factory);
        drop(repository);
        admin
            .batch_execute(&format!("DROP DATABASE {database} WITH (FORCE)"))
            .unwrap();
    }
}
