//! The legacy signal and build-log families served from PostgreSQL
//! (`ASEMAN_SIGNAL_LOG_PROVIDER=postgres`): the same rows, ordering, bounds, tag
//! filters, and edit semantics the node relied on under QuestDB.
//!
//! Needs `ASEMAN_TEST_POSTGRES_URL` (an administrative URL); skips without it.

use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use aseman_domain::signal_tags::LogQuery;
use aseman_storage_rocksdb::{LegacyBuildLogRow, LegacySignalRow, QuestDbTimeSeries};

fn psql(url: &str, sql: &str) {
    let output = Command::new("psql")
        .arg(format!("--dbname={url}"))
        .args(["--no-psqlrc", "-q", "-v", "ON_ERROR_STOP=1", "-c", sql])
        .output()
        .expect("run psql");
    assert!(
        output.status.success(),
        "{sql}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn retarget(url: &str, database: &str) -> String {
    let scheme = url.find("://").expect("postgres URL") + 3;
    let slash = url[scheme..]
        .find('/')
        .map_or(url.len(), |index| scheme + index);
    match url[slash..].split_once('?') {
        Some((_, query)) => format!("{}/{database}?{query}", &url[..slash]),
        None => format!("{}/{database}", &url[..slash]),
    }
}

fn signal(id: &str, tags: &str, time_millis: i64) -> LegacySignalRow {
    LegacySignalRow {
        id: id.to_owned(),
        store_id: "store-1".to_owned(),
        user_id: "user-1".to_owned(),
        data: format!("body {id}"),
        encoded_tags: tags.to_owned(),
        time_millis,
        edited: false,
    }
}

fn query(tags_all: &[&str], tags_any: &[&str], count: i64) -> LogQuery {
    LogQuery {
        tags_all: tags_all.iter().map(|tag| (*tag).to_owned()).collect(),
        tags_any: tags_any.iter().map(|tag| (*tag).to_owned()).collect(),
        before_time: 0,
        after_time: 0,
        count,
    }
}

#[test]
fn signal_and_build_logs_round_trip_through_postgres() {
    let Some(admin) = aseman_config::IntegrationTestConfig::from_process().postgres_url else {
        eprintln!("ASEMAN_TEST_POSTGRES_URL is absent; skipping");
        return;
    };
    let database = format!(
        "aseman_signal_log_{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    psql(&admin, &format!("CREATE DATABASE {database}"));
    let url = retarget(&admin, &database);

    let log = QuestDbTimeSeries::connect_postgres(&url).unwrap();
    // Connecting again finds the schema in place.
    let log = {
        drop(log);
        QuestDbTimeSeries::connect_postgres(&url).unwrap()
    };

    for (id, tags, time) in [
        ("s1", "|alpha|", 1_000),
        ("s2", "|alpha|beta|", 2_000),
        ("s3", "|gamma|", 3_000),
    ] {
        log.insert_signal(&signal(id, tags, time)).unwrap();
    }
    // A duplicate id is refused rather than silently doubling a message.
    assert!(log.insert_signal(&signal("s1", "|alpha|", 1_000)).is_err());

    let newest_first: Vec<String> = log
        .read_signals("store-1", &query(&[], &[], 0))
        .unwrap()
        .into_iter()
        .map(|row| row.id)
        .collect();
    assert_eq!(newest_first, ["s3", "s2", "s1"]);
    let alpha: Vec<String> = log
        .read_signals("store-1", &query(&["alpha"], &[], 10))
        .unwrap()
        .into_iter()
        .map(|row| row.id)
        .collect();
    assert_eq!(alpha, ["s2", "s1"]);
    let any: Vec<String> = log
        .read_signals("store-1", &query(&[], &["beta", "gamma"], 10))
        .unwrap()
        .into_iter()
        .map(|row| row.id)
        .collect();
    assert_eq!(any, ["s3", "s2"]);
    assert_eq!(
        log.read_signals("store-1", &query(&[], &[], 1))
            .unwrap()
            .len(),
        1
    );
    assert!(
        log.read_signals("store-1", &query(&["bad'tag"], &[], 10))
            .is_err()
    );
    assert!(
        log.read_signals("other", &query(&[], &[], 10))
            .unwrap()
            .is_empty()
    );

    let picked = log.pick_signals("store-1", &["s1".to_owned(), "s3".to_owned()]);
    let mut picked_ids: Vec<&str> = picked.iter().map(|row| row.id.as_str()).collect();
    picked_ids.sort_unstable();
    assert_eq!(picked_ids, ["s1", "s3"]);

    // Characterized legacy edit semantics: only a row already marked edited changes.
    log.update_signal("store-1", "s1", "changed");
    let unchanged = log.pick_signals("store-1", &["s1".to_owned()]);
    assert_eq!(unchanged[0].data, "body s1");
    let mut edited = signal("s4", "|alpha|", 4_000);
    edited.edited = true;
    log.insert_signal(&edited).unwrap();
    log.update_signal("store-1", "s4", "changed");
    assert_eq!(
        log.pick_signals("store-1", &["s4".to_owned()])[0].data,
        "changed"
    );

    for (index, log_type) in ["stdout", "stderr", "stdout", "stdout"].iter().enumerate() {
        log.insert_build_log(&LegacyBuildLogRow {
            id: format!("b{index}"),
            build_id: "build-1".to_owned(),
            machine_id: "machine-1".to_owned(),
            vm_id: "vm-1".to_owned(),
            log_type: (*log_type).to_owned(),
            data: format!("line {index}"),
            time_millis: 10_000 + index as i64,
        });
    }
    let stdout: Vec<String> = log
        .read_build_logs("vm-1", "stdout", 0, 10)
        .into_iter()
        .map(|row| row.id)
        .collect();
    assert_eq!(stdout, ["b3", "b2", "b0"]);
    let page: Vec<String> = log
        .read_build_logs("vm-1", "stdout", 1, 2)
        .into_iter()
        .map(|row| row.id)
        .collect();
    assert_eq!(page, ["b2", "b0"]);

    drop(log);
    psql(&admin, &format!("DROP DATABASE {database} WITH (FORCE)"));
}
