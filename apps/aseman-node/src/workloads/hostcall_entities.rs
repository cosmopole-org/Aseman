//! The CRUD-style host calls guests make: creatures, stores, programs, resource
//! stores and entities, signals, and the micro calls.

use serde_json::{Map, Value, json};

use crate::state::entity_ports::EntityPorts;
use crate::state::{Creature, StorePermissions};
use crate::storage::Trx;
use aseman_domain::program::ResourceEntityRef;
use aseman_ports::BlobStore;

use super::driver::{NodeWorkloads, check_bool, check_i64, check_str};

fn number_from_input(input: &Value, key: &str, def: i64) -> i64 {
    check_i64(input, key, def)
}

fn bool_from_input(input: &Value, key: &str, def: bool) -> bool {
    check_bool(input, key, def)
}

impl NodeWorkloads {
    pub(crate) fn handle_creature_crud(
        &self,
        op: &str,
        input: &Value,
        req_id: i64,
    ) -> (String, i64) {
        match op {
            "create" => {
                let mut id = check_str(input, "id", "");
                if id.is_empty() {
                    id = self.gen_id("vm.creature");
                }
                let typ = check_str(input, "type", "agent");
                let username = check_str(input, "username", "");
                let public_key = check_str(input, "publicKey", "");
                let chain_id = check_str(input, "chainId", "main");
                let subchain_id = check_str(input, "subchainId", "main");
                let owner_id = check_str(input, "ownerId", "");
                let balance = number_from_input(input, "balance", 0);
                let id_owned = id.clone();
                let owner_owned = owner_id.clone();
                let refused = self.app.with_outcome(false, false, |t: &Trx, outcome| {
                    let record = aseman_domain::creature::CreatureRecord {
                        id: id_owned.clone(),
                        creature_type: typ.clone(),
                        username: username.clone(),
                        public_key: public_key.clone(),
                        chain_id: chain_id.clone(),
                        subchain_id: subchain_id.clone(),
                        owner_id: owner_owned.clone(),
                    };
                    // LD-14: an existing identity or username is refused instead
                    // of overwritten.
                    let creatures = crate::state::creature_ports::CreaturePorts { trx: t };
                    match aseman_ports::CreatureDirectory::create(&creatures, &record) {
                        Err(aseman_ports::PortError::Conflict) => {
                            *outcome = true;
                            return Ok(());
                        }
                        other => other.map_err(|error| anyhow::anyhow!("{error}"))?,
                    }
                    aseman_ports::CreatureBalances::open(&creatures, &id_owned, balance)
                        .map_err(|error| anyhow::anyhow!("{error}"))?;
                    Ok(())
                });
                if refused {
                    return (
                        r#"{"ok":false,"error":"creature id or username already exists"}"#.into(),
                        req_id,
                    );
                }
                (format!("{{\"ok\":true,\"id\":\"{}\"}}", id), req_id)
            }
            "update" => {
                let id = check_str(input, "id", "");
                if id.is_empty() {
                    return (r#"{"ok":false,"error":"id is required"}"#.into(), req_id);
                }
                let input_owned = input.clone();
                let id_owned = id.clone();
                let outcome = self.app.with_outcome(false, Ok(()), |t: &Trx, outcome| {
                    let creatures = crate::state::creature_ports::CreaturePorts { trx: t };
                    // LD-13: a missing creature is refused instead of being
                    // recreated as a partial record.
                    let Some(mut record) =
                        aseman_ports::CreatureDirectory::creature(&creatures, &id_owned)
                            .map_err(|error| anyhow::anyhow!("{error}"))?
                    else {
                        *outcome = Err("creature not found");
                        return Ok(());
                    };
                    let field = |name: &str| {
                        input_owned
                            .get(name)
                            .and_then(Value::as_str)
                            .map(str::to_string)
                    };
                    if let Some(v) = field("type") {
                        record.creature_type = v;
                    }
                    if let Some(v) = field("username") {
                        record.username = v;
                    }
                    if let Some(v) = field("publicKey") {
                        record.public_key = v;
                    }
                    if let Some(v) = field("chainId") {
                        record.chain_id = v;
                    }
                    if let Some(v) = field("subchainId") {
                        record.subchain_id = v;
                    }
                    if let Some(v) = field("ownerId") {
                        record.owner_id = v;
                    }
                    match aseman_ports::CreatureDirectory::update(&creatures, &record) {
                        Err(aseman_ports::PortError::Conflict) => {
                            *outcome = Err("username already exists");
                            return Ok(());
                        }
                        other => other.map_err(|error| anyhow::anyhow!("{error}"))?,
                    }
                    if let Some(v) = input_owned.get("balance").and_then(Value::as_f64) {
                        aseman_ports::CreatureBalances::set_balance(
                            &creatures, &id_owned, v as i64,
                        )
                        .map_err(|error| anyhow::anyhow!("{error}"))?;
                    }
                    Ok(())
                });
                if let Err(message) = outcome {
                    return (json!({"ok": false, "error": message}).to_string(), req_id);
                }
                (format!("{{\"ok\":true,\"id\":\"{}\"}}", id), req_id)
            }
            "delete" => {
                let mut id = check_str(input, "id", "");
                if id.is_empty() {
                    id = check_str(input, "creatureId", "");
                }
                if id.is_empty() {
                    id = check_str(input, "userId", "");
                }
                if id.is_empty() {
                    return (r#"{"ok":false,"error":"id is required"}"#.into(), req_id);
                }
                let id_owned = id.clone();
                if let Err(error) = self.app.in_action(|t: &Trx| {
                    // Memberships go through the store port; the
                    // `Store::list(.., -1, -1)` walk here was always empty (LD-12).
                    let ports = crate::state::store_ports::MembershipPorts { trx: t };
                    ports
                        .remove_member_everywhere(&id_owned)
                        .map_err(|error| anyhow::anyhow!("{error}"))?;
                    let creatures = crate::state::creature_ports::CreaturePorts { trx: t };
                    aseman_ports::CreatureDirectory::delete(&creatures, &id_owned)
                        .map_err(|error| anyhow::anyhow!("{error}"))?;
                    aseman_ports::CreatureBalances::close(&creatures, &id_owned)
                        .map_err(|error| anyhow::anyhow!("{error}"))?;
                    Ok(())
                }) {
                    eprintln!("storage: {error}");
                }
                (format!("{{\"ok\":true,\"id\":\"{}\"}}", id), req_id)
            }
            "get" => {
                let id = check_str(input, "id", "");
                if id.is_empty() {
                    return (r#"{"ok":false,"error":"id is required"}"#.into(), req_id);
                }
                let id_owned = id.clone();
                let slot = self
                    .app
                    .with_outcome(true, Creature::default(), |t: &Trx, outcome| {
                        let creatures = crate::state::creature_ports::CreaturePorts { trx: t };
                        // Legacy answers a missing id with an empty creature; kept.
                        let found = aseman_application::creature::GetCreature {
                            directory: &creatures,
                            balances: &creatures,
                        }
                        .by_id(&id_owned);
                        *outcome = match found {
                            Ok(found) => crate::state::creature_ports::creature_view(
                                found.record,
                                found.balance,
                            ),
                            Err(_) => Creature {
                                id: id_owned.clone(),
                                ..Default::default()
                            },
                        };
                        Ok(())
                    });
                let creature = slot.clone();
                let out = json!({"ok": true, "creature": creature});
                (serde_json::to_string(&out).unwrap_or_default(), req_id)
            }
            "list" => {
                let offset = number_from_input(input, "offset", 0);
                let mut count = number_from_input(input, "count", 100);
                if count <= 0 {
                    count = 100;
                }
                let creatures = self
                    .app
                    .read(|t| {
                        let creatures = crate::state::creature_ports::CreaturePorts { trx: t };
                        Ok(aseman_application::creature::GetCreature {
                            directory: &creatures,
                            balances: &creatures,
                        }
                        .list(None, offset, Some(count))
                        .map(|list| {
                            list.into_iter()
                                .map(|found| {
                                    crate::state::creature_ports::creature_view(
                                        found.record,
                                        found.balance,
                                    )
                                })
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default())
                    })
                    .unwrap_or_default();
                let out = json!({"ok": true, "creatures": creatures});
                (serde_json::to_string(&out).unwrap_or_default(), req_id)
            }
            _ => (
                r#"{"ok":false,"error":"unsupported creature op"}"#.into(),
                req_id,
            ),
        }
    }

    /// Program CRUD reads/updates for VM host calls — the read side that the
    /// existing `createProgram`/`deleteProgram` lacked. A program's metadata is
    /// stored under `ProgMeta::{id}` (e.g. an MCP manifest); `listByMachine`
    /// enumerates the programs of a machine creature via the
    /// `machinePrograms::{machineId}::{programId}` links.
    pub(crate) fn handle_program_crud(
        &self,
        op: &str,
        input: &Value,
        req_id: i64,
    ) -> (String, i64) {
        match op {
            "create" => {
                let mut machine_id = check_str(input, "machineId", "");
                if machine_id.is_empty() {
                    machine_id = check_str(input, "appId", "");
                }
                if machine_id.is_empty() {
                    return (
                        r#"{"ok":false,"error":"machineId is required"}"#.into(),
                        req_id,
                    );
                }
                let mut id = check_str(input, "programId", "");
                if id.is_empty() {
                    id = check_str(input, "id", "");
                }
                if id.is_empty() {
                    id = self.gen_id("vm.program");
                }
                let runtime = check_str(input, "runtime", "wasm");
                let path = check_str(input, "path", "");
                let comment = check_str(input, "comment", "");
                let metadata = input
                    .get("metadata")
                    .cloned()
                    .unwrap_or_else(|| Value::Object(Map::new()));
                let id_owned = id.clone();
                let machine_id_owned = machine_id.clone();
                let create_error =
                    self.app
                        .with_outcome(false, String::new(), |t: &Trx, outcome| {
                            let programs = crate::state::program_ports::ProgramPorts { trx: t };
                            let record = aseman_domain::program::ProgramRecord {
                                id: id_owned.clone(),
                                machine_id: machine_id_owned.clone(),
                                runtime: runtime.clone(),
                                path: path.clone(),
                                comment: comment.clone(),
                            };
                            // LD-17: no partial machine is written for a missing machine.
                            match aseman_ports::ProgramDirectory::create_program(&programs, &record)
                            {
                                Err(aseman_ports::PortError::Conflict) => {
                                    *outcome = "program already exists".to_string();
                                    return Ok(());
                                }
                                other => other.map_err(|error| anyhow::anyhow!("{error}"))?,
                            }
                            programs
                                .merge_metadata_value(&id_owned, &metadata)
                                .map_err(|error| anyhow::anyhow!("{error}"))?;
                            Ok(())
                        });
                let error = create_error.clone();
                if !error.is_empty() {
                    return (json!({"ok": false, "error": error}).to_string(), req_id);
                }
                (
                    format!(
                        "{{\"ok\":true,\"programId\":\"{}\",\"machineId\":\"{}\"}}",
                        id, machine_id
                    ),
                    req_id,
                )
            }
            "delete" => {
                let mut id = check_str(input, "programId", "");
                if id.is_empty() {
                    id = check_str(input, "id", "");
                }
                if id.is_empty() {
                    return (
                        r#"{"ok":false,"error":"programId is required"}"#.into(),
                        req_id,
                    );
                }
                let id_owned = id.clone();
                if let Err(error) = self.app.in_action(|t: &Trx| {
                    // LD-17: the program itself is removed, not only its relation.
                    let programs = crate::state::program_ports::ProgramPorts { trx: t };
                    aseman_ports::ProgramDirectory::delete_program(&programs, &id_owned)
                        .map_err(|error| anyhow::anyhow!("{error}"))?;
                    aseman_ports::ProgramMetadata::delete_program_metadata(&programs, &id_owned)
                        .map_err(|error| anyhow::anyhow!("{error}"))?;
                    Ok(())
                }) {
                    eprintln!("storage: {error}");
                }
                (format!("{{\"ok\":true,\"programId\":\"{}\"}}", id), req_id)
            }
            "get" => {
                let mut id = check_str(input, "programId", "");
                if id.is_empty() {
                    id = check_str(input, "id", "");
                }
                if id.is_empty() {
                    return (
                        r#"{"ok":false,"error":"programId is required"}"#.into(),
                        req_id,
                    );
                }
                let (program, metadata) = self
                    .app
                    .read(|trx| {
                        let programs = crate::state::program_ports::ProgramPorts { trx };
                        Ok((
                            programs.program_or_empty(&id),
                            programs
                                .metadata_object(&id, "metadata")
                                .unwrap_or_default(),
                        ))
                    })
                    .unwrap_or_default();
                let metadata = Value::Object(metadata);
                let out = json!({"ok": true, "program": program, "metadata": metadata});
                (serde_json::to_string(&out).unwrap_or_default(), req_id)
            }
            "list" => {
                let offset = number_from_input(input, "offset", 0);
                let mut count = number_from_input(input, "count", 100);
                if count <= 0 {
                    count = 100;
                }
                let slot = self.app.with_outcome(true, Vec::new(), |t: &Trx, outcome| {
                    if let Ok(list) = aseman_ports::ProgramDirectory::programs(
                        &crate::state::program_ports::ProgramPorts { trx: t },
                        offset,
                        Some(count),
                    ) {
                        *outcome = list
                            .into_iter()
                            .map(crate::state::program_ports::program_view)
                            .collect();
                    }
                    Ok(())
                });
                let programs = slot.clone();
                (
                    serde_json::to_string(&json!({"ok": true, "programs": programs}))
                        .unwrap_or_default(),
                    req_id,
                )
            }
            "listByMachine" => {
                let mut machine_id = check_str(input, "machineId", "");
                if machine_id.is_empty() {
                    machine_id = check_str(input, "appId", "");
                }
                if machine_id.is_empty() {
                    return (
                        r#"{"ok":false,"error":"machineId is required"}"#.into(),
                        req_id,
                    );
                }
                let mid = machine_id.clone();
                let slot = self.app.with_outcome(true, Vec::new(), |t: &Trx, outcome| {
                    if let Ok(list) = aseman_ports::ProgramDirectory::programs_of_machine(
                        &crate::state::program_ports::ProgramPorts { trx: t },
                        &mid,
                    ) {
                        *outcome = list
                            .into_iter()
                            .map(crate::state::program_ports::program_view)
                            .collect();
                    }
                    Ok(())
                });
                let programs = slot.clone();
                (
                    serde_json::to_string(&json!({"ok": true, "programs": programs}))
                        .unwrap_or_default(),
                    req_id,
                )
            }
            "update" => {
                let mut id = check_str(input, "programId", "");
                if id.is_empty() {
                    id = check_str(input, "id", "");
                }
                if id.is_empty() {
                    return (
                        r#"{"ok":false,"error":"programId is required"}"#.into(),
                        req_id,
                    );
                }
                let input_owned = input.clone();
                let id_owned = id.clone();
                let missing = self.app.with_outcome(false, false, |t: &Trx, outcome| {
                    let programs = crate::state::program_ports::ProgramPorts { trx: t };
                    // LD-13: a missing program is refused instead of created partially.
                    let Some(mut p) = aseman_ports::ProgramDirectory::program(&programs, &id_owned)
                        .map_err(|error| anyhow::anyhow!("{error}"))?
                    else {
                        *outcome = true;
                        return Ok(());
                    };
                    if let Some(v) = input_owned.get("comment").and_then(Value::as_str) {
                        p.comment = v.to_string();
                    }
                    if let Some(v) = input_owned.get("runtime").and_then(Value::as_str) {
                        p.runtime = v.to_string();
                    }
                    if let Some(v) = input_owned.get("path").and_then(Value::as_str) {
                        p.path = v.to_string();
                    }
                    aseman_ports::ProgramDirectory::update_program(&programs, &p)
                        .map_err(|error| anyhow::anyhow!("{error}"))?;
                    if let Some(md) = input_owned.get("metadata") {
                        programs
                            .merge_metadata_value(&id_owned, md)
                            .map_err(|error| anyhow::anyhow!("{error}"))?;
                    }
                    Ok(())
                });
                if missing {
                    return (
                        r#"{"ok":false,"error":"program does not exist"}"#.into(),
                        req_id,
                    );
                }
                (format!("{{\"ok\":true,\"id\":\"{}\"}}", id), req_id)
            }
            _ => (
                r#"{"ok":false,"error":"unsupported program op"}"#.into(),
                req_id,
            ),
        }
    }

    pub(crate) fn handle_resource_store_crud(
        &self,
        op: &str,
        input: &Value,
        req_id: i64,
    ) -> (String, i64) {
        match op {
            "create" | "update" => {
                let mut store_id = check_str(input, "storeId", "");
                let machine_id = check_str(input, "machineId", "");
                if store_id.is_empty() {
                    store_id = self.gen_id("vm.store");
                }
                let name = check_str(input, "name", &store_id);
                let metadata = input
                    .get("metadata")
                    .cloned()
                    .unwrap_or_else(|| Value::Object(Map::new()));
                let store_id_owned = store_id.clone();
                let machine_id_owned = machine_id.clone();
                let name_owned = name.clone();
                let refused = self
                    .app
                    .with_outcome(false, String::new(), |t: &Trx, outcome| {
                        // LD-21: an update without a machine keeps the owner, and a new
                        // store needs a machine to own it.
                        let metadata = if metadata.is_object() {
                            metadata.to_string()
                        } else {
                            "{}".to_owned()
                        };
                        match aseman_ports::VmResourceStores::put_resource_store(
                            &crate::state::program_ports::ProgramPorts { trx: t },
                            &store_id_owned,
                            &name_owned,
                            &machine_id_owned,
                            &metadata,
                        ) {
                            Err(aseman_ports::PortError::Failed(message)) => {
                                *outcome = message;
                                Ok(())
                            }
                            other => other.map_err(|error| anyhow::anyhow!("{error}")),
                        }
                    });
                let refusal = refused.clone();
                if !refusal.is_empty() {
                    return (json!({"ok": false, "error": refusal}).to_string(), req_id);
                }
                (
                    format!("{{\"ok\":true,\"storeId\":\"{}\"}}", store_id),
                    req_id,
                )
            }
            "delete" => {
                let store_id = check_str(input, "storeId", "");
                if store_id.is_empty() {
                    return (
                        r#"{"ok":false,"error":"storeId is required"}"#.into(),
                        req_id,
                    );
                }
                let store_id_owned = store_id.clone();
                if let Err(error) = self.app.in_action(|t: &Trx| {
                    // LD-06: the documents and the ownership link are really removed.
                    aseman_ports::VmResourceStores::delete_resource_store(
                        &crate::state::program_ports::ProgramPorts { trx: t },
                        &store_id_owned,
                    )
                    .map_err(|error| anyhow::anyhow!("{error}"))
                }) {
                    eprintln!("storage: {error}");
                }
                (
                    format!("{{\"ok\":true,\"storeId\":\"{}\"}}", store_id),
                    req_id,
                )
            }
            "get" => {
                let store_id = check_str(input, "storeId", "");
                if store_id.is_empty() {
                    return (
                        r#"{"ok":false,"error":"storeId is required"}"#.into(),
                        req_id,
                    );
                }
                let (core, meta) = self
                    .app
                    .read(|t| {
                        let Some(store) = aseman_ports::VmResourceStores::resource_store(
                            &crate::state::program_ports::ProgramPorts { trx: t },
                            &store_id,
                        )
                        .map_err(|error| anyhow::anyhow!("{error}"))?
                        else {
                            return Ok((Map::new(), Map::new()));
                        };
                        let core = Map::from_iter([
                            ("id".to_owned(), json!(store.id)),
                            ("name".to_owned(), json!(store.name)),
                            ("machineId".to_owned(), json!(store.machine_id)),
                        ]);
                        Ok((
                            core,
                            serde_json::from_str(&store.metadata).unwrap_or_default(),
                        ))
                    })
                    .unwrap_or_default();
                let (core, meta) = (Value::Object(core), Value::Object(meta));
                let out = json!({"ok": true, "store": {"core": core, "metadata": meta}});
                (serde_json::to_string(&out).unwrap_or_default(), req_id)
            }
            "list" => {
                let machine_id = check_str(input, "machineId", "");
                let slot = self.app.with_outcome(true, Vec::new(), |t: &Trx, outcome| {
                    let programs = crate::state::program_ports::ProgramPorts { trx: t };
                    let filter = (!machine_id.is_empty()).then_some(machine_id.as_str());
                    let mut listed = Vec::new();
                    for store_id in
                        aseman_ports::VmResourceStores::resource_stores(&programs, filter)
                            .map_err(|error| anyhow::anyhow!("{error}"))?
                    {
                        let owner =
                            aseman_ports::VmResourceStores::resource_store(&programs, &store_id)
                                .map_err(|error| anyhow::anyhow!("{error}"))?
                                .map(|store| store.machine_id)
                                .unwrap_or_default();
                        // Legacy returned the ownership link keys; the wire keeps
                        // that format. Listing all stores used to return nothing.
                        listed.push(["link::vmOwnedStore::", &owner, "::", &store_id].concat());
                    }
                    *outcome = listed;
                    Ok(())
                });
                let stores = slot.clone();
                let out = json!({"ok": true, "stores": stores});
                (serde_json::to_string(&out).unwrap_or_default(), req_id)
            }
            _ => (
                r#"{"ok":false,"error":"unsupported store op"}"#.into(),
                req_id,
            ),
        }
    }

    pub(crate) fn handle_resource_entity_create(
        &self,
        input: &Value,
        req_id: i64,
    ) -> (String, i64) {
        let store_id = check_str(input, "storeId", "");
        if store_id.is_empty() {
            return (
                r#"{"ok":false,"error":"storeId is required"}"#.into(),
                req_id,
            );
        }
        let entity_type = check_str(input, "entityType", "default");
        let mut entity_id = check_str(input, "entityId", "");
        if entity_id.is_empty() {
            entity_id = self.gen_id("vm.entity");
        }
        let payload = input.get("payload").cloned().unwrap_or_else(|| json!({}));
        let data = check_str(input, "data", "");
        let reference = ResourceEntityRef {
            store_id,
            entity_type,
            entity_id: entity_id.clone(),
        };
        let blobs = crate::blobs::node_blobs(&self.app.tools().storage());
        let path = blobs
            .local_path(&reference.data_key())
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_default();
        let failure = self.app.with_outcome(false, None, |t: &Trx, outcome| {
            aseman_application::program::PutResourceEntity {
                entities: &EntityPorts { trx: t },
                blobs: &blobs,
            }
            .execute(&reference, &payload.to_string(), data.as_bytes())
            .map_err(|error| {
                *outcome = Some(error.to_string());
                anyhow::anyhow!("{error}")
            })
        });
        if let Some(error) = failure {
            let out = json!({"ok": false, "error": error});
            return (out.to_string(), req_id);
        }
        let out = json!({"ok": true, "entityId": entity_id, "path": path});
        (out.to_string(), req_id)
    }

    pub(crate) fn handle_resource_entity_delete(
        &self,
        input: &Value,
        req_id: i64,
    ) -> (String, i64) {
        let store_id = check_str(input, "storeId", "");
        if store_id.is_empty() {
            return (
                r#"{"ok":false,"error":"storeId is required"}"#.into(),
                req_id,
            );
        }
        let entity_type = check_str(input, "entityType", "default");
        let entity_id = check_str(input, "entityId", "");
        if entity_id.is_empty() {
            return (
                r#"{"ok":false,"error":"entityId is required"}"#.into(),
                req_id,
            );
        }
        let reference = ResourceEntityRef {
            store_id,
            entity_type,
            entity_id,
        };
        let blobs = crate::blobs::node_blobs(&self.app.tools().storage());
        let failure = self.app.with_outcome(false, None, |t: &Trx, outcome| {
            aseman_application::program::DeleteResourceEntity {
                entities: &EntityPorts { trx: t },
                blobs: &blobs,
            }
            .execute(&reference)
            .map_err(|error| {
                *outcome = Some(error.to_string());
                anyhow::anyhow!("{error}")
            })
        });
        if let Some(error) = failure {
            let out = json!({"ok": false, "error": error});
            return (out.to_string(), req_id);
        }
        (r#"{"ok":true}"#.into(), req_id)
    }

    /// Run a registered shell action on behalf of a VM.
    ///
    /// `caller` is the node's own answer to "which creature is calling" —
    /// resolved from the VM context the docker gateway verifies (or the id an
    /// in-process runtime stamps on the packet), never from anything the guest
    /// can write. It is the ONLY identity a creature may act as.
    ///
    /// Two modes, and the difference matters:
    ///   * `asSelf: true` — act as the calling creature, through the applet
    ///     signature path. This is how a container performs an action that is
    ///     genuinely its own (uploading media it produced), with the record
    ///     showing the creature that did it.
    ///   * otherwise — the caller names the identity, which for an anonymous
    ///     action (`/creatures/login`) is deliberately empty. A creature cannot
    ///     reach a *user's* authenticated action this way: without a real
    ///     signature the guard refuses it.
    pub(crate) fn handle_exec_shell_action(
        &self,
        caller: &str,
        input: &Value,
        req_id: i64,
    ) -> (String, i64) {
        let path = check_str(input, "path", "");
        if path.is_empty() {
            return (r#"{"ok":false,"error":"path is required"}"#.into(), req_id);
        }
        let as_self = input
            .get("asSelf")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let owner = self.app.owner_id();
        let (user_id, signature) = if as_self {
            let caller = caller.trim();
            if caller.is_empty() {
                // Acting as an unidentifiable creature would mean acting as
                // nobody in particular — refuse instead of falling back to an
                // identity the caller did not earn.
                return (
                    r#"{"ok":false,"error":"caller identity unavailable"}"#.into(),
                    req_id,
                );
            }
            (caller.to_string(), "#appletsign".to_string())
        } else {
            (
                check_str(input, "userId", &owner),
                check_str(input, "signature", ""),
            )
        };
        let payload = serde_json::to_vec(&input.get("payload").cloned().unwrap_or(Value::Null))
            .unwrap_or_default();
        let request = crate::actions::dispatch::SignedRequest {
            path: &path,
            packet: crate::actions::guard::SignedPacket {
                user_id: &user_id,
                payload: &payload,
                signature: &signature,
            },
        };
        let answer = match self
            .app
            .router()
            .dispatch(&request, crate::actions::guard::Entry::Inside)
        {
            Ok(value) => json!({"ok": true, "statusCode": 0, "result": value}),
            Err(refusal) => json!({
                "ok": false,
                "statusCode": refusal.code(),
                "error": refusal.message(),
            }),
        };
        (answer.to_string(), req_id)
    }

    pub(crate) fn handle_micro_host_action(
        &self,
        op: &str,
        input: &Value,
        req_id: i64,
    ) -> (String, i64) {
        match op {
            "genId" => {
                let source = check_str(input, "source", "vm.micro");
                let id = self.gen_id(&source);
                (format!("{{\"ok\":true,\"id\":\"{}\"}}", id), req_id)
            }
            "createAccess" | "updateAccess" => {
                let user_id = check_str(input, "userId", "");
                if user_id.is_empty() {
                    return (
                        r#"{"ok":false,"error":"userId is required"}"#.into(),
                        req_id,
                    );
                }
                let store_id = check_str(input, "storeId", "");
                if store_id.is_empty() {
                    return (
                        r#"{"ok":false,"error":"storeId is required"}"#.into(),
                        req_id,
                    );
                }
                // A grant states what the member may do. It is required, not
                // defaulted: a caller that forgets it would otherwise mint a
                // member who can do nothing (and look like a bug in signalling)
                // or, worse under a different default, a viewer who can post.
                let permissions: Vec<String> = input
                    .get("permissions")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| v.as_str().map(|s| s.to_string()))
                            .collect()
                    })
                    .unwrap_or_default();
                if permissions.is_empty() {
                    return (
                        r#"{"ok":false,"error":"permissions is required (e.g. [\"read\",\"signal\"])"}"#
                            .into(),
                        req_id,
                    );
                }
                let perms = StorePermissions::from_list(&permissions);
                if perms.is_empty() {
                    return (
                        r#"{"ok":false,"error":"permissions names no known flag"}"#.into(),
                        req_id,
                    );
                }
                let user_id_owned = user_id.clone();
                let store_id_owned = store_id.clone();
                if let Err(error) = self.app.in_action(|t: &Trx| {
                    let ports = crate::state::store_ports::MembershipPorts { trx: t };
                    aseman_ports::StoreAccess::join(&ports, &store_id_owned, &user_id_owned, perms)
                        .map_err(|error| anyhow::anyhow!("{error}"))
                }) {
                    eprintln!("storage: {error}");
                }
                let out = json!({"ok": true, "permissions": perms});
                (serde_json::to_string(&out).unwrap_or_default(), req_id)
            }
            "deleteAccess" => {
                let user_id = check_str(input, "userId", "");
                if user_id.is_empty() {
                    return (
                        r#"{"ok":false,"error":"userId is required"}"#.into(),
                        req_id,
                    );
                }
                let store_id = check_str(input, "storeId", "");
                if store_id.is_empty() {
                    return (
                        r#"{"ok":false,"error":"storeId is required"}"#.into(),
                        req_id,
                    );
                }
                let user_id_owned = user_id.clone();
                let store_id_owned = store_id.clone();
                if let Err(error) = self.app.in_action(|t: &Trx| {
                    let ports = crate::state::store_ports::MembershipPorts { trx: t };
                    aseman_ports::StoreAccess::leave(&ports, &store_id_owned, &user_id_owned)
                        .map_err(|error| anyhow::anyhow!("{error}"))
                }) {
                    eprintln!("storage: {error}");
                }
                (r#"{"ok":true}"#.into(), req_id)
            }
            "readSignals" => {
                // The store-log read, for creatures and connected containers:
                // the same tag-filtered query `/stores/history` serves the app,
                // so an agent backbone reconstructs a conversation from exactly
                // the rows the client sees, with no second transcript anywhere.
                let store_id = check_str(input, "storeId", "");
                if store_id.is_empty() {
                    return (
                        r#"{"ok":false,"error":"storeId is required"}"#.into(),
                        req_id,
                    );
                }
                let str_list = |key: &str| -> Vec<String> {
                    input
                        .get(key)
                        .and_then(Value::as_array)
                        .map(|a| {
                            a.iter()
                                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                                .collect()
                        })
                        .unwrap_or_default()
                };
                let query = aseman_domain::signal_tags::LogQuery {
                    tags_all: str_list("tagsAll"),
                    tags_any: str_list("tagsAny"),
                    before_time: check_i64(input, "beforeTime", 0),
                    after_time: check_i64(input, "afterTime", 0),
                    count: check_i64(input, "count", 100),
                };
                let query = match query.validated() {
                    Ok(q) => q,
                    Err(e) => {
                        let out = json!({"ok": false, "error": format!("{}", e)});
                        return (serde_json::to_string(&out).unwrap_or_default(), req_id);
                    }
                };
                let (history_store, history_query) = (store_id.clone(), query.clone());
                let history = self.app.with_outcome(
                    true,
                    Err(anyhow::anyhow!("state unavailable")),
                    |trx: &Trx, outcome| {
                        let signals = aseman_ports::SignalLog::history(
                            &crate::state::store_ports::SignalPorts { trx },
                            &history_store,
                            &history_query,
                        )
                        .map(|signals| {
                            signals
                                .into_iter()
                                .map(crate::state::store_ports::log_packet)
                                .collect::<Vec<_>>()
                        })
                        .map_err(|error| anyhow::anyhow!("{error}"));
                        *outcome = signals;
                        Ok(())
                    },
                );
                let packets = match history {
                    Ok(p) => p,
                    Err(e) => {
                        // A creature must be able to tell "no history" from "the
                        // log is unreachable"; an empty list for both would have
                        // an agent reason over a conversation it never read.
                        let out = json!({"ok": false, "error": format!("{}", e)});
                        return (serde_json::to_string(&out).unwrap_or_default(), req_id);
                    }
                };
                let out = json!({"ok": true, "storeId": store_id, "signals": packets});
                (serde_json::to_string(&out).unwrap_or_default(), req_id)
            }
            "hasAccessToStore" => {
                let machine_id = check_str(input, "machineId", "");
                if machine_id.is_empty() {
                    return (
                        r#"{"ok":false,"error":"machineId is required"}"#.into(),
                        req_id,
                    );
                }
                let store_id = check_str(input, "storeId", "");
                if store_id.is_empty() {
                    return (
                        r#"{"ok":false,"error":"storeId is required"}"#.into(),
                        req_id,
                    );
                }
                let allowed = self
                    .app
                    .tools()
                    .security()
                    .has_access_to_store(&machine_id, &store_id);
                let out = json!({"ok": true, "allowed": allowed});
                (serde_json::to_string(&out).unwrap_or_default(), req_id)
            }
            "signalUser" => {
                let key = check_str(input, "key", "");
                let user_id = check_str(input, "userId", "");
                let packet = check_str(input, "packet", "{}");
                let value = serde_json::from_str::<Value>(&packet).unwrap_or(Value::Null);
                self.app
                    .tools()
                    .signaler()
                    .signal_user(&key, &user_id, value);
                (r#"{"ok":true}"#.into(), req_id)
            }
            "signalGroup" => {
                let key = check_str(input, "key", "");
                let group_id = check_str(input, "groupId", "");
                let packet = check_str(input, "packet", "{}");
                let except: Vec<String> = input
                    .get("except")
                    .and_then(Value::as_array)
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default();
                let value = serde_json::from_str::<Value>(&packet).unwrap_or(Value::Null);
                self.app
                    .tools()
                    .signaler()
                    .signal_group(&key, &group_id, value, except);
                (r#"{"ok":true}"#.into(), req_id)
            }
            "joinGroup" => {
                let group_id = check_str(input, "groupId", "");
                let user_id = check_str(input, "userId", "");
                self.app.tools().signaler().join_group(&group_id, &user_id);
                (r#"{"ok":true}"#.into(), req_id)
            }
            _ => (
                r#"{"ok":false,"error":"unsupported micro op"}"#.into(),
                req_id,
            ),
        }
    }

    pub(crate) fn handle_store_crud(&self, op: &str, input: &Value, req_id: i64) -> (String, i64) {
        match op {
            "create" => {
                let mut store_id = check_str(input, "storeId", "");
                let mut creator_id = check_str(input, "creatorId", "");
                if creator_id.is_empty() {
                    creator_id = check_str(input, "userId", "");
                }
                let tag = check_str(input, "tag", "");
                let parent_id = check_str(input, "parentId", "");
                let is_public = bool_from_input(input, "isPublic", false);
                let pers_hist = bool_from_input(input, "persHist", false);
                let metadata = input.get("metadata").cloned().unwrap_or_else(|| json!({}));
                if store_id.is_empty() {
                    store_id = self.gen_id("store");
                }
                // A store must have a creator to own it (ADR 0018, ADR 0026 routing).
                if creator_id.is_empty() {
                    return (
                        r#"{"ok":false,"error":"creatorId is required"}"#.into(),
                        req_id,
                    );
                }
                let store_id_owned = store_id.clone();
                let creator_id_owned = creator_id.clone();
                let metadata_owned = metadata.clone();
                let refused = self.app.with_outcome(false, "", |t: &Trx, outcome| {
                    let stores = crate::state::store_ports::StorePorts { trx: t };
                    let record = aseman_domain::store::StoreRecord {
                        id: store_id_owned.clone(),
                        tag: tag.clone(),
                        parent_id: parent_id.clone(),
                        is_public,
                        persistent_history: pers_hist,
                        member_count: 1,
                        signal_count: 0,
                    };
                    match aseman_ports::StoreDirectory::create_store(
                        &stores,
                        &record,
                        &creator_id_owned,
                    ) {
                        Err(aseman_ports::PortError::Conflict) => {
                            *outcome = "store already exists";
                            return Ok(());
                        }
                        other => other.map_err(|error| anyhow::anyhow!("{error}"))?,
                    }
                    stores
                        .merge_metadata_value(&store_id_owned, &metadata_owned)
                        .map_err(|error| anyhow::anyhow!("{error}"))?;
                    // The creator administers the store they just made.
                    let ports = crate::state::store_ports::MembershipPorts { trx: t };
                    aseman_ports::StoreAccess::join(
                        &ports,
                        &store_id_owned,
                        &creator_id_owned,
                        StorePermissions::owner(),
                    )
                    .map_err(|error| anyhow::anyhow!("{error}"))?;
                    Ok(())
                });
                let refusal = refused;
                if !refusal.is_empty() {
                    return (json!({"ok": false, "error": refusal}).to_string(), req_id);
                }
                let out = json!({"ok": true, "storeId": store_id});
                (serde_json::to_string(&out).unwrap_or_default(), req_id)
            }
            "update" => {
                let store_id = check_str(input, "storeId", "");
                if store_id.is_empty() {
                    return (
                        r#"{"ok":false,"error":"storeId is required"}"#.into(),
                        req_id,
                    );
                }
                let input_owned = input.clone();
                let store_id_owned = store_id.clone();
                if let Err(error) = self.app.in_action(|t: &Trx| {
                    let stores = crate::state::store_ports::StorePorts { trx: t };
                    // A missing store stays a no-op, as before.
                    let Some(mut store) =
                        aseman_ports::StoreDirectory::store(&stores, &store_id_owned)
                            .map_err(|error| anyhow::anyhow!("{error}"))?
                    else {
                        return Ok(());
                    };
                    if let Some(v) = input_owned.get("isPublic").and_then(Value::as_bool) {
                        store.is_public = v;
                    }
                    if let Some(v) = input_owned.get("persHist").and_then(Value::as_bool) {
                        store.persistent_history = v;
                    }
                    if let Some(v) = input_owned.get("tag").and_then(Value::as_str) {
                        store.tag = v.to_string();
                    }
                    aseman_ports::StoreDirectory::update_store(&stores, &store)
                        .map_err(|error| anyhow::anyhow!("{error}"))?;
                    if let Some(md) = input_owned.get("metadata") {
                        stores
                            .merge_metadata_value(&store_id_owned, md)
                            .map_err(|error| anyhow::anyhow!("{error}"))?;
                    }
                    Ok(())
                }) {
                    eprintln!("storage: {error}");
                }
                (
                    format!("{{\"ok\":true,\"storeId\":\"{}\"}}", store_id),
                    req_id,
                )
            }
            "delete" => {
                let store_id = check_str(input, "storeId", "");
                if store_id.is_empty() {
                    return (
                        r#"{"ok":false,"error":"storeId is required"}"#.into(),
                        req_id,
                    );
                }
                let store_id_owned = store_id.clone();
                if let Err(error) = self.app.in_action(|t: &Trx| {
                    // LD-06: the metadata document is really removed with the store.
                    let stores = crate::state::store_ports::StorePorts { trx: t };
                    aseman_ports::StoreDirectory::delete_store(&stores, &store_id_owned)
                        .map_err(|error| anyhow::anyhow!("{error}"))?;
                    aseman_ports::StoreMetadata::delete_store_metadata(&stores, &store_id_owned)
                        .map_err(|error| anyhow::anyhow!("{error}"))?;
                    // Membership links outlive the object unless we drop them:
                    // listStores walks hasaccess, and a later getStore still
                    // echoes the requested id, which is how a deleted space
                    // came back as an untitled project.
                    let ports = crate::state::store_ports::MembershipPorts { trx: t };
                    let members = aseman_ports::StoreAccess::members(&ports, &store_id_owned)
                        .map_err(|error| anyhow::anyhow!("{error}"))?;
                    for (member_id, _) in members {
                        aseman_ports::StoreAccess::leave(&ports, &store_id_owned, &member_id)
                            .map_err(|error| anyhow::anyhow!("{error}"))?;
                    }
                    Ok(())
                }) {
                    eprintln!("storage: {error}");
                }
                (
                    format!("{{\"ok\":true,\"storeId\":\"{}\"}}", store_id),
                    req_id,
                )
            }
            "get" => {
                let store_id = check_str(input, "storeId", "");
                if store_id.is_empty() {
                    return (
                        r#"{"ok":false,"error":"storeId is required"}"#.into(),
                        req_id,
                    );
                }
                let (store, meta) = self
                    .app
                    .read(|trx| {
                        let stores = crate::state::store_ports::StorePorts { trx };
                        Ok((
                            stores.store_or_empty(&store_id),
                            stores
                                .metadata_object(&store_id, "metadata")
                                .unwrap_or_default(),
                        ))
                    })
                    .unwrap_or_default();
                let meta = Value::Object(meta);
                let out = json!({"ok": true, "store": store, "metadata": meta});
                (serde_json::to_string(&out).unwrap_or_default(), req_id)
            }
            "list" => {
                let user_id = check_str(input, "userId", "");
                let slot = self.app.with_outcome(true, Vec::new(), |t: &Trx, outcome| {
                    let list = if user_id.is_empty() {
                        // The legacy `Store::list("obj::Store::", ..)` searched links and
                        // was always empty; list the first 50 stores instead.
                        aseman_ports::StoreDirectory::stores(
                            &crate::state::store_ports::StorePorts { trx: t },
                            0,
                            Some(50),
                        )
                        .map(|records| {
                            records
                                .into_iter()
                                .map(crate::state::store_ports::store_view)
                                .collect()
                        })
                        .map_err(|error| anyhow::anyhow!("{error}"))
                    } else {
                        let ports = crate::state::store_ports::MembershipPorts { trx: t };
                        ports
                            .member_stores(&user_id, 50)
                            .map_err(|error| anyhow::anyhow!("{error}"))
                    };
                    if let Ok(list) = list {
                        *outcome = list;
                    }
                    Ok(())
                });
                let stores = slot.clone();
                let out = json!({"ok": true, "stores": stores});
                (serde_json::to_string(&out).unwrap_or_default(), req_id)
            }
            // List the creatures with access to a store. An optional `type`
            // filter (e.g. "machine", "human") returns only creatures of that
            // type — each resolved to its full Creature record.
            "listAccess" | "listMembers" | "readMembers" => {
                let store_id = check_str(input, "storeId", "");
                if store_id.is_empty() {
                    return (
                        r#"{"ok":false,"error":"storeId is required"}"#.into(),
                        req_id,
                    );
                }
                let want_type = check_str(input, "type", "");
                let sid = store_id.clone();
                let want_owned = want_type.clone();
                let slot = self.app.with_outcome(true, Vec::new(), |t: &Trx, outcome| {
                    let ports = crate::state::store_ports::MembershipPorts { trx: t };
                    let members = aseman_ports::StoreAccess::members(&ports, &sid)
                        .map_err(|error| anyhow::anyhow!("{error}"))?;
                    let mut out: Vec<Creature> = Vec::new();
                    for (member_id, _) in members {
                        let c = (crate::state::creature_ports::CreaturePorts { trx: t })
                            .creature_or_empty(&member_id);
                        if c.id.is_empty() {
                            continue;
                        }
                        if !want_owned.is_empty() && c.type_name != want_owned {
                            continue;
                        }
                        out.push(c);
                    }
                    *outcome = out;
                    Ok(())
                });
                let members = slot.clone();
                let out =
                    json!({"ok": true, "storeId": store_id, "type": want_type, "members": members});
                (serde_json::to_string(&out).unwrap_or_default(), req_id)
            }
            _ => (
                r#"{"ok":false,"error":"unsupported store op"}"#.into(),
                req_id,
            ),
        }
    }

    /// Post one signal into a store on behalf of the calling VM.
    ///
    /// The signaller is the node-stamped `machineId` — the identity the gateway
    /// resolved for this container, which is also what `/stores/signal` checks
    /// the `signal` permission against. A caller-supplied `userId` is NOT
    /// required and NOT used for identity: a creature never declares who it is.
    pub(crate) fn handle_signal_store(&self, input: &Value, req_id: i64) -> (String, i64) {
        let machine_id = check_str(input, "machineId", "");
        if machine_id.is_empty() {
            return (
                r#"{"ok":false,"error":"machineId is required (the node stamps it)"}"#.into(),
                req_id,
            );
        }
        let typ_and_temp = check_str(input, "type", "");
        let mut typ = typ_and_temp.clone();
        let mut temp = false;
        if let Some(t) = input.get("temp").and_then(Value::as_bool) {
            temp = t;
        } else {
            let parts: Vec<&str> = typ_and_temp.split('|').collect();
            if let Some(t) = parts.first() {
                typ = t.to_string();
            }
            if parts.len() > 1 {
                temp = parts[1] == "true";
            }
        }
        let store_id = check_str(input, "storeId", "");
        if store_id.is_empty() {
            return (
                r#"{"ok":false,"error":"storeId is required"}"#.into(),
                req_id,
            );
        }
        // The signaller is the calling VM, which the node already knows. Anything
        // the caller puts in `userId` is carried on the packet as authorship
        // metadata only — it can never be the identity a permission is checked
        // against, and its absence must never reject the signal.
        let user_id = check_str(input, "userId", &machine_id);
        let data = check_str(input, "data", "");
        // Tags the calling creature attaches to the packet — the labels
        // `stores/history` later filters on. Malformed tags fail the signal in
        // the action body rather than being dropped here.
        let tags: Vec<String> = input
            .get("tags")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();
        let body = json!({
            "type": typ,
            "data": data,
            "storeId": store_id,
            "userId": user_id,
            "tags": tags,
            "temp": temp,
        });
        let router = self.app.router();
        let Some(operation) = router.operation("/stores/signal") else {
            return (
                r#"{"ok":false,"error":"/stores/signal is not registered"}"#.into(),
                req_id,
            );
        };
        let caller = crate::actions::Caller {
            user_id: machine_id,
            store_id,
            source: self.app.id(),
        };
        // The operation's own answer goes back to the creature: one whose signals
        // are refused (no `signal` permission, an unwritable log) must learn it.
        let answer = match router.execute(&caller, operation, body.to_string().as_bytes(), true) {
            Ok(Value::Object(mut fields)) => {
                fields.insert("ok".to_owned(), Value::Bool(true));
                Value::Object(fields)
            }
            Ok(other) => json!({"ok": true, "result": other}),
            Err(error) => json!({"ok": false, "error": error.to_string()}),
        };
        (answer.to_string(), req_id)
    }

    /// Mint an id for `source` (its own short transaction).
    pub(super) fn gen_id(&self, source: &str) -> String {
        self.app.tools().storage().gen_id(source)
    }
}

#[cfg(test)]
mod exec_shell_action_tests {
    use serde_json::json;

    /// The identity rule, expressed the way `handle_exec_shell_action` applies
    /// it. `asSelf` acts as the node-resolved caller and nothing else — a guest
    /// naming a `userId` alongside it cannot redirect who it acts as, and an
    /// unresolvable caller is refused rather than falling back to the node owner
    /// (which would hand a container the platform's own authority).
    fn acting_identity(
        caller: &str,
        input: &serde_json::Value,
        owner: &str,
    ) -> Option<(String, String)> {
        let as_self = input
            .get("asSelf")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        if as_self {
            let caller = caller.trim();
            if caller.is_empty() {
                return None;
            }
            return Some((caller.to_string(), "#appletsign".to_string()));
        }
        let user_id = input
            .get("userId")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| owner.to_string());
        let signature = input
            .get("signature")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_string();
        Some((user_id, signature))
    }

    #[test]
    fn as_self_acts_as_the_resolved_caller_only() {
        let input = json!({"asSelf": true, "userId": "1@global", "signature": "forged"});
        let (user, sig) = acting_identity("42@global", &input, "1@global").unwrap();
        assert_eq!(user, "42@global", "the guest-named userId is ignored");
        assert_eq!(
            sig, "#appletsign",
            "a creature authenticates through the applet path"
        );
    }

    #[test]
    fn as_self_without_a_resolved_caller_is_refused() {
        let input = json!({"asSelf": true});
        assert!(
            acting_identity("", &input, "1@global").is_none(),
            "an unidentifiable caller must not fall back to the node owner",
        );
    }

    #[test]
    fn an_explicitly_empty_user_stays_anonymous() {
        // How an anonymous caller reaches an anon-guarded action: no identity, no
        // signature.
        let input = json!({"userId": ""});
        let (user, sig) = acting_identity("42@global", &input, "1@global").unwrap();
        assert_eq!(user, "");
        assert_eq!(sig, "");
    }

    #[test]
    fn an_omitted_user_still_defaults_to_the_owner() {
        let input = json!({"path": "/creatures/authenticate"});
        let (user, _) = acting_identity("42@global", &input, "1@global").unwrap();
        assert_eq!(
            user, "1@global",
            "unchanged for callers that name no identity"
        );
    }
}

#[cfg(test)]
mod signal_store_tests {
    use serde_json::json;

    /// Who a store signal is attributed to, expressed the way
    /// `handle_signal_store` resolves it.
    ///
    /// This is the rule that shipped wrong: the hostcall REQUIRED a `userId` the
    /// docker gateway never stamps and no creature sends, so every signal the
    /// agent backbone posted — every step, every tool call, every answer — was
    /// refused before it reached the action.
    fn signaller(input: &serde_json::Value) -> Option<String> {
        let machine_id = input
            .get("machineId")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if machine_id.is_empty() {
            return None;
        }
        Some(machine_id.to_string())
    }

    #[test]
    fn a_caller_that_declares_no_user_is_still_accepted() {
        // Exactly what the backbone sends: the node's own stamp, nothing else.
        let input = json!({"machineId": "170@global", "storeId": "7@global", "type": "all"});
        assert_eq!(signaller(&input).as_deref(), Some("170@global"));
    }

    #[test]
    fn the_signaller_is_the_stamped_machine_not_a_declared_user() {
        // A creature naming somebody else does not become them: the permission
        // check runs against the identity the node stamped.
        let input = json!({"machineId": "170@global", "userId": "1@global", "storeId": "7@global"});
        assert_eq!(signaller(&input).as_deref(), Some("170@global"));
    }

    #[test]
    fn an_unstamped_call_is_refused() {
        assert!(signaller(&json!({"storeId": "7@global"})).is_none());
    }
}
