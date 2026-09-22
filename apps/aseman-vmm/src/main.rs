//! `aseman-vmm`: the provider-neutral VMM service (plan 04, ADR 0029).
//!
//! It serves A501 to the nodes admitted by `ASEMAN_VMM_CLIENTS` over mutual TLS,
//! keeps workloads, operations, idempotency keys, and events in PostgreSQL, and does
//! infrastructure work through the A504 backend at `ASEMAN_VMM_BACKEND_ENDPOINT`.
//! A worker thread runs the executor, the observer, reconciliation, and retention.

use std::io::BufReader;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use aseman_config::{VmmServiceConfig, read_secret_file};
use aseman_contracts::vmm::IDEMPOTENCY_RETENTION_MILLIS;
use aseman_ports::ClockPort;
use aseman_storage_postgres::vmm::PostgresVmmStore;
use aseman_vmm_backend_grpc::client::GrpcBackend;
use aseman_vmm_http::server::{ServerTls, VmmHttpState, health_router, serve};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};

/// Events are kept this long for readers to catch up; older positions resync.
const EVENT_RETENTION_MILLIS: i64 = 24 * 60 * 60 * 1000;

struct SystemClock;

impl ClockPort for SystemClock {
    fn unix_millis(&self) -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| {
                i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
            })
    }
}

type Failure = Box<dyn std::error::Error>;

fn certificates(path: &str) -> Result<Vec<CertificateDer<'static>>, Failure> {
    let bytes = std::fs::read(path)?;
    Ok(rustls_pemfile::certs(&mut BufReader::new(bytes.as_slice())).collect::<Result<_, _>>()?)
}

fn private_key(secret: &str) -> Result<PrivateKeyDer<'static>, Failure> {
    let pem = read_secret_file(secret, 64 * 1024)?;
    rustls_pemfile::private_key(&mut BufReader::new(pem.as_bytes()))?
        .ok_or_else(|| "the TLS key secret holds no private key".into())
}

fn report(task: &str, error: impl std::fmt::Display) {
    eprintln!("aseman-vmm: {task} failed: {error}");
}

/// One pass of the background work. Failures are reported and retried next pass.
fn tick(state: &VmmHttpState, clock: &SystemClock, last_retention: &mut i64) {
    let service = state.service();
    if let Err(error) = service.execute_pending(500) {
        report("the executor", error);
    }
    match service.observe() {
        Ok(observed) if !observed.undesired.is_empty() => eprintln!(
            "aseman-vmm: {} backend instances have no workload (left alone for adoption)",
            observed.undesired.len()
        ),
        Ok(_) => {}
        Err(error) => report("observation", error),
    }
    if let Err(error) = service.reconcile() {
        report("reconciliation", error);
    }
    let now = clock.unix_millis();
    if now - *last_retention >= 60_000 {
        *last_retention = now;
        if let Err(error) = state
            .idempotency
            .purge_before(now - IDEMPOTENCY_RETENTION_MILLIS)
        {
            report("idempotency retention", error);
        }
        if let Err(error) = state.events.truncate_before(now - EVENT_RETENTION_MILLIS) {
            report("event retention", error);
        }
    }
}

fn main() -> Result<(), Failure> {
    let config = VmmServiceConfig::from_process()?;
    let clock = Arc::new(SystemClock);
    let store = Arc::new(PostgresVmmStore::connect(
        &read_secret_file(&config.database_url_secret, 4096)?,
        config.database_pool_size,
    )?);
    store.migrate()?;
    let backend = Arc::new(GrpcBackend::connect(
        &config.backend_endpoint,
        Duration::from_secs(60),
    )?);
    let state = Arc::new(VmmHttpState {
        workloads: store.clone(),
        operations: store.clone(),
        events: store.clone(),
        idempotency: store,
        backend,
        clock: clock.clone(),
        max_request_bytes: config.max_request_bytes,
    });
    let tls = ServerTls {
        certificate_chain: certificates(&config.tls_certificate)?,
        private_key: private_key(&config.tls_key_secret)?,
        client_roots: certificates(&config.client_ca)?,
        clients: config
            .clients
            .iter()
            .map(|(node, fingerprint)| (*fingerprint, node.clone()))
            .collect(),
    };
    let running = Arc::new(AtomicBool::new(true));
    let worker = {
        let state = state.clone();
        let running = running.clone();
        let interval = Duration::from_millis(config.reconcile_interval_millis.max(50));
        std::thread::spawn(move || {
            let mut last_retention = 0;
            while running.load(Ordering::Relaxed) {
                tick(&state, &clock, &mut last_retention);
                std::thread::sleep(interval);
            }
        })
    };
    let runtime = tokio::runtime::Runtime::new()?;
    let outcome: Result<(), Failure> = runtime.block_on(async {
        let listener = tokio::net::TcpListener::bind(&config.listen).await?;
        if let Some(health) = &config.health_listen {
            let health_listener = tokio::net::TcpListener::bind(health).await?;
            let router = health_router(state.clone());
            tokio::spawn(async move {
                let _ = axum::serve(health_listener, router).await;
            });
        }
        serve(listener, tls, state.clone(), async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
        .map_err(Into::into)
    });
    running.store(false, Ordering::Relaxed);
    let _ = worker.join();
    outcome
}
