//! The provider-neutral collection and settlement pass used by `aseman-meter`.

use std::collections::BTreeMap;

use aseman_domain::WorkloadId;
use aseman_domain::finance::{
    Dimension, UsageSample, interval_between, price, price_list_at, settle,
};
use aseman_domain::vmm::Usage;
use aseman_ports::finance::{Ledger, PricingStore, UsageStore};
use aseman_ports::vmm::{VmmClient, WorkloadFilter};
use aseman_ports::{PortError, PortResult};

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MeterReport {
    pub workloads_seen: usize,
    pub samples_recorded: usize,
    pub intervals_recorded: usize,
    pub intervals_settled: usize,
}

pub struct RunMeterPass<'a> {
    pub vmm: &'a dyn VmmClient,
    pub usage: &'a dyn UsageStore,
    pub pricing: &'a dyn PricingStore,
    pub ledger: &'a dyn Ledger,
    pub revenue_account: &'a str,
    pub page_size: usize,
    pub settlement_batch: usize,
}

fn usage_sample(workload: WorkloadId, reading: Usage) -> UsageSample {
    UsageSample {
        workload_id: *workload.as_uuid(),
        provider_sample_id: format!("a501:{}:{}", reading.window_end_millis, reading.sequence),
        provider: "a501".to_owned(),
        collected_at_millis: reading.window_end_millis,
        // Memory peak and storage bytes are gauges. Treating them as cumulative
        // counters would charge their change rather than their time integral.
        cumulative: BTreeMap::from([
            (Dimension::CpuMillis, reading.cpu_millis),
            (Dimension::NetworkIngressBytes, reading.network_rx_bytes),
            (Dimension::NetworkEgressBytes, reading.network_tx_bytes),
        ]),
    }
}

impl RunMeterPass<'_> {
    /// Collect every visible workload and settle the oldest pending intervals.
    ///
    /// # Errors
    ///
    /// A provider is unavailable or returned an unpriceable interval.
    pub fn execute(&self) -> PortResult<MeterReport> {
        let mut report = MeterReport::default();
        let mut cursor = None;
        loop {
            let page = self.vmm.workloads(
                &WorkloadFilter::default(),
                cursor.as_deref(),
                self.page_size.max(1),
            )?;
            for workload in page.items {
                report.workloads_seen += 1;
                let reading = self.vmm.usage(workload.id)?;
                let sample = usage_sample(workload.id, reading);
                let previous = self
                    .usage
                    .previous_sample(sample.workload_id, sample.collected_at_millis)?;
                match self.usage.record_sample(&sample) {
                    Ok(()) => report.samples_recorded += 1,
                    Err(PortError::Conflict) => continue,
                    Err(error) => return Err(error),
                }
                if let Some(previous) = previous {
                    let interval = interval_between(&previous, &sample)
                        .map_err(|error| PortError::Failed(error.to_string()))?;
                    match self.usage.record_interval(&interval) {
                        Ok(()) => report.intervals_recorded += 1,
                        Err(PortError::Conflict) => {}
                        Err(error) => return Err(error),
                    }
                }
            }
            cursor = page.next_cursor;
            if cursor.is_none() {
                break;
            }
        }

        let prices = self.pricing.price_lists()?;
        for interval in self.usage.unsettled(self.settlement_batch.max(1))? {
            let list = price_list_at(&prices, interval.interval_start_millis)
                .ok_or_else(|| PortError::Failed("no price list covers the interval".to_owned()))?;
            let charge =
                price(&interval, list).map_err(|error| PortError::Failed(error.to_string()))?;
            let workload = self
                .vmm
                .workload(WorkloadId::from_uuid(interval.workload_id))?
                .ok_or(PortError::NotFound)?;
            let wallet = format!("wallet:creature:{}", workload.labels.creature_id);
            let record = settle(
                &charge,
                &wallet,
                self.revenue_account,
                interval.interval_end_millis,
            )
            .map_err(|error| PortError::Failed(error.to_string()))?;
            self.ledger.commit(&record)?;
            report.intervals_settled += 1;
        }
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a501_gauges_are_not_misrepresented_as_cumulative_billing_counters() {
        let sample = usage_sample(
            WorkloadId::new(),
            Usage {
                window_start_millis: 1_000,
                window_end_millis: 2_000,
                sequence: 7,
                cpu_millis: 11,
                memory_peak_bytes: 12,
                network_rx_bytes: 13,
                network_tx_bytes: 14,
                storage_bytes: 15,
                invocations: 16,
            },
        );
        assert_eq!(sample.provider_sample_id, "a501:2000:7");
        assert_eq!(sample.cumulative[&Dimension::CpuMillis], 11);
        assert_eq!(sample.cumulative[&Dimension::NetworkIngressBytes], 13);
        assert_eq!(sample.cumulative[&Dimension::NetworkEgressBytes], 14);
        assert_eq!(sample.cumulative.len(), 3);
    }
}
