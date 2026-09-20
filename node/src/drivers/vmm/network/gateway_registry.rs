use crate::drivers::vmm::network::gateway_types::VmGatewayEndpoint;
use crate::drivers::vmm::prelude::*;

pub(crate) static VM_GATEWAY_ENDPOINTS: Lazy<Arc<Mutex<HashMap<String, VmGatewayEndpoint>>>> =
    Lazy::new(|| Arc::new(Mutex::new(HashMap::new())));

pub(crate) struct VmGatewayRegistry;

impl VmGatewayRegistry {
    pub(crate) fn register(endpoint: VmGatewayEndpoint) {
        let key = endpoint.key();
        let mut map = VM_GATEWAY_ENDPOINTS.lock().unwrap();
        map.insert(key, endpoint);
    }

    pub(crate) fn unregister(machine_id: &str, vm_id: &str) {
        let key = format!("{}::{}", machine_id.trim(), vm_id.trim());
        let mut map = VM_GATEWAY_ENDPOINTS.lock().unwrap();
        map.remove(&key);
    }

    pub(crate) fn resolve(machine_id: &str, vm_id: &str) -> Option<VmGatewayEndpoint> {
        let key = format!("{}::{}", machine_id.trim(), vm_id.trim());
        let map = VM_GATEWAY_ENDPOINTS.lock().unwrap();
        map.get(&key).cloned()
    }

    pub(crate) fn list() -> Vec<VmGatewayEndpoint> {
        let map = VM_GATEWAY_ENDPOINTS.lock().unwrap();
        map.values().cloned().collect()
    }
}
