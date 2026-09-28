//! A902 backup and clean-restore drill against real PostgreSQL clusters.
//!
//! Runs the shipped `asemanctl backup` against a source cluster holding migrated core
//! storage plus one creature guest database, then `asemanctl restore` onto a second,
//! empty cluster, and checks that core rows, guest rows, and the guest role survive,
//! that a resumed restore repeats no completed step, and that a non-empty target and a
//! manifest signed by an untrusted key are both refused.
//!
//! Needs two clusters so the restore target is genuinely clean:
//! `ASEMAN_TEST_POSTGRES_URL` (source) and `ASEMAN_TEST_POSTGRES_RESTORE_URL` (target),
//! each an administrative URL. The PostgreSQL client tools must be on `PATH` at a major
//! version no older than the target server. Skips when either URL is absent.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

const MIGRATIONS: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../modules/storage/postgres/migrations"
);

fn psql(url: &str, sql: &str) -> String {
    let output = Command::new("psql")
        .arg(format!("--dbname={url}"))
        .args(["--no-psqlrc", "-At", "-v", "ON_ERROR_STOP=1", "-c", sql])
        .output()
        .expect("run psql");
    assert!(
        output.status.success(),
        "psql failed for {sql}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

fn psql_file(url: &str, path: &Path) {
    let output = Command::new("psql")
        .arg(format!("--dbname={url}"))
        .args(["--no-psqlrc", "-q", "-v", "ON_ERROR_STOP=1", "-f"])
        .arg(path)
        .output()
        .expect("run psql");
    assert!(
        output.status.success(),
        "{}: {}",
        path.display(),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn retarget(url: &str, database: &str) -> String {
    let scheme = url.find("://").expect("postgres URL") + 3;
    let slash = url[scheme..]
        .find('/')
        .map_or(url.len(), |index| scheme + index);
    let query = url[slash..].split_once('?').map(|(_, query)| query);
    match query {
        Some(query) => format!("{}/{database}?{query}", &url[..slash]),
        None => format!("{}/{database}", &url[..slash]),
    }
}

struct Node {
    data_dir: PathBuf,
    state_dir: PathBuf,
}

impl Node {
    fn new(root: &Path, name: &str, database_url: &str) -> Self {
        let data_dir = root.join(name).join("data");
        let state_dir = root.join(name).join("state");
        let secrets = root.join(name).join("secrets");
        for dir in [&data_dir, &state_dir, &secrets] {
            fs::create_dir_all(dir).unwrap();
        }
        fs::write(secrets.join("database-url"), database_url).unwrap();
        fs::write(secrets.join("guest-proxy-url"), database_url).unwrap();
        fs::write(
            data_dir.join(".env"),
            format!(
                "ASEMAN_NODE_ID=restore-drill\n\
                 ASEMAN_NODE_PRIVATE_KEY_SECRET={}\n\
                 ASEMAN_CORE_STORAGE_PROVIDER=postgres\n\
                 ASEMAN_DATABASE_URL_SECRET={}\n\
                 ASEMAN_GUEST_PROXY_URL_SECRET={}\n\
                 ASEMAN_GUEST_PROXY_ROLE=aseman_guest_proxy\n\
                 ASEMAN_LEGACY_STORAGE_ROOT_PATH={}\n",
                secrets.join("node-key").display(),
                secrets.join("database-url").display(),
                secrets.join("guest-proxy-url").display(),
                data_dir.join("storage").display(),
            ),
        )
        .unwrap();
        Self {
            data_dir,
            state_dir,
        }
    }

    fn asemanctl(&self, arguments: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_asemanctl"))
            .args(arguments)
            .arg("--data-dir")
            .arg(&self.data_dir)
            .arg("--state-dir")
            .arg(&self.state_dir)
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .env_remove("ASEMAN_OPERATOR_SIGNING_KEY")
            .output()
            .expect("run asemanctl")
    }
}

fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[test]
fn postgres_backup_restores_onto_a_clean_cluster() {
    let environment = aseman_config::IntegrationTestConfig::from_process();
    let (Some(source_admin), Some(target_admin)) =
        (environment.postgres_url, environment.postgres_restore_url)
    else {
        eprintln!(
            "ASEMAN_TEST_POSTGRES_URL and ASEMAN_TEST_POSTGRES_RESTORE_URL are required; skipping"
        );
        return;
    };
    let unique = format!(
        "{:024x}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let unique = &unique[unique.len() - 24..];
    let core = format!("aseman_drill_{unique}");
    let guest = format!("aseman_guest_{unique}");
    let role = format!("aseman_creature_{unique}");
    let root = std::env::temp_dir().join(format!("aseman-restore-drill-{unique}"));
    fs::create_dir_all(&root).unwrap();

    // Source: migrated core storage, one binding, and a guest database with data.
    psql(&source_admin, &format!("CREATE DATABASE {core}"));
    let source = retarget(&source_admin, &core);
    let mut migrations: Vec<PathBuf> = fs::read_dir(MIGRATIONS)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "sql"))
        .collect();
    migrations.sort();
    for migration in &migrations {
        psql_file(&source, migration);
    }
    psql(&source_admin, &format!("CREATE ROLE {role} NOLOGIN"));
    psql(
        &source_admin,
        &format!("CREATE DATABASE {guest} TEMPLATE template0"),
    );
    let source_guest = retarget(&source_admin, &guest);
    psql(
        &source_guest,
        &format!(
            "CREATE TABLE notes(id int PRIMARY KEY, body text); \
             INSERT INTO notes SELECT n, 'note ' || n FROM generate_series(1, 250) n; \
             GRANT SELECT ON notes TO {role}"
        ),
    );
    // One user, the creature it owns, and that creature's guest database binding.
    let user = psql(&source, "SELECT gen_random_uuid()");
    let creature = psql(&source, "SELECT gen_random_uuid()");
    psql(
        &source,
        &format!(
            "INSERT INTO aseman_core.users (id, schema_version, revision, created_at_micros, \
             updated_at_micros, integrity_hash, owner_type, capsule_cbor, username, \
             public_key, status) VALUES ('{user}', 1, 1, 1, 1, \
             decode(repeat('ef', 32), 'hex'), 'global', '\\x00', 'drill', '\\x01', 'active')"
        ),
    );
    psql(
        &source,
        &format!(
            "INSERT INTO aseman_core.creatures (id, schema_version, revision, \
             created_at_micros, updated_at_micros, integrity_hash, owner_type, capsule_cbor, \
             username, creature_type, public_key, status, owner) VALUES ('{creature}', 1, 1, \
             1, 1, decode(repeat('cd', 32), 'hex'), 'global', '\\x00', 'drill', 'human', \
             '\\x01', 'active', '{user}')"
        ),
    );
    psql(
        &source,
        &format!(
            "INSERT INTO aseman_core.guest_database_bindings (id, schema_version, revision, \
             created_at_micros, updated_at_micros, integrity_hash, owner_type, capsule_cbor, \
             provider_id, database_name, role_name, generation, schema_catalog_revision, \
             status, creature) VALUES (gen_random_uuid(), 1, 1, 1, 1, \
             decode(repeat('ab', 32), 'hex'), 'global', '\\x00', 'postgres', '{guest}', \
             '{role}', 1, 1, 'active', '{creature}')"
        ),
    );

    let key = root.join("operator.key");
    fs::write(&key, "11".repeat(32)).unwrap();
    let backup_dir = root.join("backup");
    let source_node = Node::new(&root, "source", &source);
    let backup = source_node.asemanctl(&[
        "backup",
        "--out",
        backup_dir.to_str().unwrap(),
        "--signing-key",
        key.to_str().unwrap(),
        "--allow-running",
    ]);
    assert!(backup.status.success(), "backup failed: {}", text(&backup));
    let dumps = backup_dir.join("snapshot/postgres");
    for artifact in ["globals.sql", "core.dump", "databases.json"] {
        assert!(dumps.join(artifact).exists(), "{artifact} missing");
    }
    assert!(dumps.join(format!("guest/{guest}.dump")).exists());

    // Target: an independently provisioned, empty cluster.
    psql(&target_admin, &format!("CREATE DATABASE {core}"));
    let target = retarget(&target_admin, &core);
    let target_node = Node::new(&root, "target", &target);

    // A manifest is trusted only under the operator's key.
    let untrusted = root.join("untrusted.key");
    fs::write(&untrusted, "22".repeat(32)).unwrap();
    let refused = target_node.asemanctl(&[
        "restore",
        "--from",
        backup_dir.to_str().unwrap(),
        "--signing-key",
        untrusted.to_str().unwrap(),
    ]);
    assert!(!refused.status.success());
    assert!(
        text(&refused).contains("not signed by the trusted operator key"),
        "{}",
        text(&refused)
    );
    fs::remove_dir_all(&target_node.state_dir).unwrap();
    fs::create_dir_all(&target_node.state_dir).unwrap();

    let restore = target_node.asemanctl(&[
        "restore",
        "--from",
        backup_dir.to_str().unwrap(),
        "--signing-key",
        key.to_str().unwrap(),
    ]);
    // Health gates on a running node, which this drill neither starts nor requires;
    // every step before it — verification, preparation, restore, catalog, and
    // integrity — must pass.
    let restore_text = text(&restore);
    for step in [
        "verify-manifest … ok",
        "prepare-target … ok",
        "restore-stores … ok",
        "apply-catalog … ok",
        "verify-integrity … ok",
    ] {
        assert!(
            restore_text.contains(step),
            "{step} missing: {restore_text}"
        );
    }

    assert_eq!(
        psql(
            &target,
            "SELECT count(*) FROM aseman_core.guest_database_bindings"
        ),
        "1"
    );
    let target_guest = retarget(&target_admin, &guest);
    assert_eq!(psql(&target_guest, "SELECT count(*) FROM notes"), "250");
    assert_eq!(
        psql(
            &target_guest,
            &format!("SELECT has_table_privilege('{role}', 'notes', 'SELECT')")
        ),
        "t"
    );

    // Re-running repeats no completed step: the restore is never replayed onto the
    // now non-empty target.
    let resumed = target_node.asemanctl(&[
        "restore",
        "--from",
        backup_dir.to_str().unwrap(),
        "--signing-key",
        key.to_str().unwrap(),
    ]);
    let resumed_text = text(&resumed);
    assert!(!resumed_text.contains("restore-stores"), "{resumed_text}");
    assert!(!resumed_text.contains("verify-manifest"), "{resumed_text}");

    // A fresh restore onto the now non-empty target is refused before any change.
    fs::remove_dir_all(&target_node.state_dir).unwrap();
    fs::create_dir_all(&target_node.state_dir).unwrap();
    let overwrite = target_node.asemanctl(&[
        "restore",
        "--from",
        backup_dir.to_str().unwrap(),
        "--signing-key",
        key.to_str().unwrap(),
    ]);
    assert!(
        text(&overwrite).contains("needs an empty target"),
        "{}",
        text(&overwrite)
    );

    for admin in [&source_admin, &target_admin] {
        for statement in [
            format!("DROP DATABASE IF EXISTS {core} WITH (FORCE)"),
            format!("DROP DATABASE IF EXISTS {guest} WITH (FORCE)"),
            format!("DROP ROLE IF EXISTS {role}"),
        ] {
            psql(admin, &statement);
        }
    }
    let _ = fs::remove_dir_all(&root);
}
