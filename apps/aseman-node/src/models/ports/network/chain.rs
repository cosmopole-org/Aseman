use aseman_network_legacy::TlsConfig;

/// Pipeline callback. Receives a batch of payloads and a per-payload
/// emit callback; returns the keys of the messages that were forwarded.
pub type PipelineFn =
    Box<dyn Fn(Vec<Vec<u8>>, Box<dyn Fn(Vec<u8>) + Send + Sync>) -> Vec<String> + Send + Sync>;

/// The blockchain network driver interface.
pub trait IChain: Send + Sync {
    fn listen(&self, port: i64, tls_config: Option<TlsConfig>);
    fn restore_from_storage(&self);
    fn submit_trx(&self, chain_id: &str, machine_id: &str, typ: &str, payload: Vec<u8>);
    fn register_pipeline(&self, pipeline: PipelineFn);
    fn notify_new_machine_created(&self, chain_id: &str, machine_id: &str);
    fn create_temp_chain(&self, store_id: &str) -> String;
    fn create_work_chain(&self, store_id: &str) -> String;
    fn create_shard_chain(
        &self,
        chain_id: &str,
        shard_chain_id: &str,
        peers: Vec<String>,
    ) -> String;
    fn peers(&self) -> Vec<String>;
    fn user_owns_origin(&self, user_id: &str, origin: &str) -> bool;
    fn get_node_owner_id(&self, origin: &str) -> String;
    fn close(&self);
}
