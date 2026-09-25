//! Independent Phase 8 metering composition root.

use std::str::FromStr;
use std::time::Duration;

use aseman_application::meter::RunMeterPass;
use aseman_config::MeterConfig;
use aseman_finance_ledger::PostgresFinance;
use aseman_vmm_http::client::{ClientTls, HttpVmmClient};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let once = std::env::args().any(|argument| argument == "--once");
    let config = MeterConfig::from_process()?;
    let database_url = aseman_config::read_secret_file(&config.database_url_secret, 16 * 1024)?;
    let finance = PostgresFinance::connect_config(
        postgres::Config::from_str(database_url.trim())?,
        config.database_pool_size,
    )?;
    finance.migrate()?;
    let tls = ClientTls {
        server_roots_pem: aseman_config::read_secret_file(&config.vmm_server_ca, 1024 * 1024)?
            .into_bytes(),
        identity_pem: aseman_config::read_secret_file(&config.vmm_identity_secret, 1024 * 1024)?
            .into_bytes(),
    };
    let vmm = HttpVmmClient::new(
        &config.vmm_endpoint,
        &tls,
        &config.vmm_owner,
        Duration::from_millis(config.vmm_deadline_millis),
    )?;

    loop {
        let report = RunMeterPass {
            vmm: &vmm,
            usage: &finance,
            pricing: &finance,
            ledger: &finance,
            revenue_account: &config.revenue_account,
            page_size: config.page_size,
            settlement_batch: config.settlement_batch,
        }
        .execute()?;
        eprintln!(
            "meter pass: workloads={} samples={} intervals={} settled={}",
            report.workloads_seen,
            report.samples_recorded,
            report.intervals_recorded,
            report.intervals_settled
        );
        if once {
            return Ok(());
        }
        std::thread::sleep(Duration::from_secs(config.poll_interval_seconds.max(1)));
    }
}
