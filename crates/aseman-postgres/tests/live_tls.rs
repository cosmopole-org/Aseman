//! Verified TLS against a live server that offers it (`ASEMAN_TEST_POSTGRES_TLS_URL`,
//! whose `sslrootcert` names the server's CA). Absent, the suite is skipped.

use aseman_postgres::Database;

fn tls_url() -> Option<String> {
    aseman_config::IntegrationTestConfig::from_process().postgres_tls_url
}

/// `url` without its `sslrootcert`, so only the bundled roots are trusted.
fn without_root(url: &str) -> String {
    let mut parsed = url::Url::parse(url).unwrap();
    let kept: Vec<(String, String)> = parsed
        .query_pairs()
        .filter(|(key, _)| key != "sslrootcert")
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    parsed.query_pairs_mut().clear().extend_pairs(kept);
    parsed.to_string()
}

#[test]
fn a_verified_connection_is_encrypted_and_an_untrusted_server_is_refused() {
    let Some(url) = tls_url() else {
        return;
    };
    let mut client = Database::parse(&url).unwrap().connect().unwrap();
    let encrypted: bool = client
        .query_one(
            "SELECT ssl FROM pg_stat_ssl WHERE pid = pg_backend_pid()",
            &[],
        )
        .unwrap()
        .get(0);
    assert!(encrypted, "the connection is not encrypted");

    // The same server, without the CA that issued its certificate: refused, not
    // silently accepted and not downgraded.
    let error = Database::parse(&without_root(&url))
        .unwrap()
        .connect()
        .err()
        .expect("an unverifiable server was accepted");
    // The refusal is the certificate check, reported through the error's sources.
    let mut chain = error.to_string();
    let mut source = std::error::Error::source(&error);
    while let Some(cause) = source {
        chain.push_str(&format!(": {cause}"));
        source = cause.source();
    }
    assert!(chain.to_lowercase().contains("certificate"), "{chain}");
}
