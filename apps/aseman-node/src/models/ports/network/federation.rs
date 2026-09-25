use serde_json::Value;

use crate::util::GoError;
use aseman_network_legacy::TlsConfig;

/// Callback delivering a federation response — payload, status code, error.
pub type FedRequestCallback = Box<dyn Fn(Vec<u8>, i64, Option<GoError>) + Send + Sync>;

/// The federation (inter-organization) network driver interface.
pub trait IFederation: Send + Sync {
    fn listen(&self, port: i64, tls_config: Option<TlsConfig>);
    fn send_fed_request(
        &self,
        dest_org: &str,
        request_id: &str,
        user_id: &str,
        path: &str,
        payload: Vec<u8>,
        signature: &str,
    );
    fn send_fed_response(&self, dest_org: &str, request_id: &str, res_code: i64, res: Value);
    fn send_fed_update(
        &self,
        dest_org: &str,
        key: &str,
        update_pack: Value,
        target_type: &str,
        target_id_val: &str,
        exceptions: Vec<String>,
    );
    #[allow(clippy::too_many_arguments)]
    fn send_fed_request_by_callback(
        &self,
        dest_org: &str,
        request_id: &str,
        user_id: &str,
        path: &str,
        payload: Vec<u8>,
        signature: &str,
        callback: FedRequestCallback,
    );
}
