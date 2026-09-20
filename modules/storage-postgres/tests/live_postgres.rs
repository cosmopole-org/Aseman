use aseman_contracts::capsule::{
    CapsuleEnvelope, CapsuleId, CapsuleKind, CapsuleQuery, ProviderCapabilities, QueryError,
};
use aseman_storage_conformance::{StorageProviderHarness, query_error, reference_core_user_suite};
use aseman_storage_postgres::PostgresCapsuleRepository;
use aseman_storage_postgres::PostgresStorageService;
use postgres::{Client, NoTls};
use std::sync::Arc;

struct PostgresHarness {
    connection_uri: String,
    repository: Arc<PostgresCapsuleRepository>,
}

impl StorageProviderHarness for PostgresHarness {
    fn reset(&mut self) -> Result<(), QueryError> {
        let mut client = Client::connect(&self.connection_uri, NoTls).map_err(|error| {
            query_error(
                aseman_contracts::capsule::QueryErrorCode::Unavailable,
                error.to_string(),
            )
        })?;
        client
            .batch_execute("TRUNCATE TABLE aseman_core.users CASCADE")
            .map_err(|error| {
                query_error(
                    aseman_contracts::capsule::QueryErrorCode::Unavailable,
                    error.to_string(),
                )
            })
    }

    fn capabilities(&self) -> ProviderCapabilities {
        PostgresCapsuleRepository::capabilities()
    }

    fn put(
        &mut self,
        capsule: CapsuleEnvelope,
        expected_revision: Option<u64>,
    ) -> Result<(), QueryError> {
        self.repository
            .put(&capsule, expected_revision)
            .map_err(Into::into)
    }

    fn get(
        &self,
        kind: &CapsuleKind,
        id: &CapsuleId,
    ) -> Result<Option<CapsuleEnvelope>, QueryError> {
        self.repository.get(kind, id).map_err(Into::into)
    }

    fn query(&self, query: &CapsuleQuery) -> Result<Vec<CapsuleEnvelope>, QueryError> {
        self.repository.query(query).map_err(Into::into)
    }
}

#[test]
fn live_postgres_passes_storage_and_grpc_conformance() {
    let Some(connection_uri) = aseman_config::IntegrationTestConfig::from_process().postgres_url
    else {
        eprintln!("ASEMAN_TEST_POSTGRES_URL is absent; skipping disposable PostgreSQL test");
        return;
    };
    let repository = Arc::new(PostgresCapsuleRepository::connect(&connection_uri).unwrap());
    repository.migrate().unwrap();
    repository.migrate().unwrap();
    let mut harness = PostgresHarness {
        connection_uri: connection_uri.clone(),
        repository: Arc::clone(&repository),
    };
    let report = reference_core_user_suite().run(&mut harness).unwrap();
    assert_eq!(report.provider_id, "postgres-core-v1");

    tokio::runtime::Runtime::new().unwrap().block_on(async move {
        let socket = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = socket.local_addr().unwrap();
        drop(socket);
        let provider = PostgresStorageService::new(repository);
        let server = tokio::spawn(async move {
            tonic::transport::Server::builder()
                .add_service(
                    aseman_contracts::module_control_v1::module_control_server::ModuleControlServer::new(
                        provider.clone(),
                    ),
                )
                .add_service(
                    aseman_contracts::capsule_provider_v1::capsule_storage_server::CapsuleStorageServer::new(
                        provider,
                    ),
                )
                .serve(address)
                .await
        });
        let endpoint = format!("http://{address}");
        let mut client = None;
        for _ in 0..30 {
            match aseman_contracts::capsule_provider_v1::capsule_storage_client::CapsuleStorageClient::connect(
                endpoint.clone(),
            )
            .await
            {
                Ok(value) => {
                    client = Some(value);
                    break;
                }
                Err(_) => tokio::time::sleep(std::time::Duration::from_millis(10)).await,
            }
        }
        let mut client = client.expect("storage gRPC service must start");
        let described = client
            .describe(aseman_contracts::capsule_provider_v1::DescribeRequest {
                meta: Some(aseman_contracts::module_control_v1::RequestMetadata {
                    request_id: "live-describe".to_owned(),
                    trace_id: "live-trace".to_owned(),
                    deadline_unix_millis: i64::MAX,
                    cancellation_id: "live-cancel".to_owned(),
                    idempotency_key: "live-idempotency".to_owned(),
                }),
            })
            .await
            .unwrap()
            .into_inner();
        assert_eq!(described.provider_id, "postgres-core-v1");
        assert!(described.error.is_none());
        server.abort();
    });

    let mut client = Client::connect(&connection_uri, NoTls).unwrap();
    let id = uuid::Uuid::parse_str("09090909-0909-0909-0909-090909090909").unwrap();
    let row = client
        .query_one(
            "SELECT tombstone, username, octet_length(capsule_cbor) \
             FROM aseman_core.users WHERE id = $1",
            &[&id],
        )
        .unwrap();
    assert!(row.get::<_, bool>(0));
    assert_eq!(row.get::<_, Option<String>>(1), None);
    assert!(row.get::<_, Option<i32>>(2).unwrap_or_default() > 0);

    let class_tables: i64 = client
        .query_one(
            "SELECT count(*) FROM information_schema.tables \
             WHERE table_schema IN ('aseman_telemetry', 'aseman_audit', 'aseman_finance', \
                                    'aseman_outbox', 'aseman_realtime')",
            &[],
        )
        .unwrap()
        .get(0);
    assert_eq!(class_tables, 13);

    client
        .batch_execute(
            "TRUNCATE TABLE aseman_audit.audit_events; \
             INSERT INTO aseman_audit.audit_events (\
               id, schema_version, revision, created_at_micros, updated_at_micros, \
               previous_integrity, integrity_hash, owner_type, owner_id, owner_name, \
               tombstone, capsule_cbor, stream_id, sequence, actor, action, target, \
               decision, policy_version, trace_id, occurred_at_micros, details, \
               previous_event_integrity\
             ) VALUES (\
               'aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa', 1, 1, 10, 10, NULL, \
               decode(repeat('00', 32), 'hex'), 'global', NULL, NULL, FALSE, \
               decode('a0', 'hex'), 'security', 1, 'system', 'policy.evaluate', \
               'resource', 'allow', 1, 'trace-live', 10, NULL, NULL\
             )",
        )
        .unwrap();
    let immutable = client.execute(
        "UPDATE aseman_audit.audit_events SET decision = 'deny' \
         WHERE id = 'aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa'",
        &[],
    );
    assert_eq!(
        immutable.unwrap_err().as_db_error().unwrap().code().code(),
        "55000"
    );
    let duplicate_sequence = client.batch_execute(
        "INSERT INTO aseman_audit.audit_events (\
           id, schema_version, revision, created_at_micros, updated_at_micros, \
           previous_integrity, integrity_hash, owner_type, owner_id, owner_name, \
           tombstone, capsule_cbor, stream_id, sequence, actor, action, target, \
           decision, policy_version, trace_id, occurred_at_micros, details, \
           previous_event_integrity\
         ) VALUES (\
           'bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb', 1, 1, 11, 11, NULL, \
           decode(repeat('11', 32), 'hex'), 'global', NULL, NULL, FALSE, \
           decode('a0', 'hex'), 'security', 1, 'system', 'policy.evaluate', \
           'other', 'deny', 1, 'trace-live-2', 11, NULL, NULL\
         )",
    );
    assert_eq!(
        duplicate_sequence
            .unwrap_err()
            .as_db_error()
            .unwrap()
            .code()
            .code(),
        "23505"
    );
}
