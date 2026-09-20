use aseman_contracts::module_control_v1::module_control_server::ModuleControlServer;
use aseman_contracts::module_sample_v1::sample_server::SampleServer;
use aseman_sample_provider::SampleProvider;
use tonic::transport::Server;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let address = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "127.0.0.1:9081".to_owned())
        .parse()?;
    let provider = SampleProvider::new();
    Server::builder()
        .add_service(ModuleControlServer::new(provider.clone()))
        .add_service(SampleServer::new(provider))
        .serve_with_shutdown(address, async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}
