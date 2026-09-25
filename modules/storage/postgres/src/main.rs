use aseman_config::{read_json_file, read_secret_file};
use aseman_contracts::capsule_provider_v1::capsule_storage_server::CapsuleStorageServer;
use aseman_contracts::module_control_v1::module_control_server::ModuleControlServer;
use aseman_storage_postgres::{
    PostgresCapsuleRepository, PostgresProviderConfig, PostgresStorageService,
};
use std::sync::Arc;
use tonic::transport::Server;

const CONNECTION_URI_SECRET: &str = "/run/secrets/aseman-postgres-core-uri";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args().skip(1);
    let address = arguments
        .next()
        .ok_or("module listen address is required")?
        .parse()?;
    let configuration_path = arguments
        .next()
        .ok_or("module configuration path is required")?;
    if arguments.next().is_some() {
        return Err("unexpected module arguments".into());
    }
    let config: PostgresProviderConfig = read_json_file(configuration_path)?;
    let connection_uri = read_secret_file(CONNECTION_URI_SECRET, 4096)?;
    let repository = Arc::new(PostgresCapsuleRepository::connect(&connection_uri)?);
    if config.apply_migrations {
        repository.migrate()?;
    }
    let provider = PostgresStorageService::new(repository);
    tokio::runtime::Runtime::new()?.block_on(async move {
        Server::builder()
            .add_service(ModuleControlServer::new(provider.clone()))
            .add_service(CapsuleStorageServer::new(provider))
            .serve_with_shutdown(address, async {
                let _ = tokio::signal::ctrl_c().await;
            })
            .await
    })?;
    Ok(())
}
