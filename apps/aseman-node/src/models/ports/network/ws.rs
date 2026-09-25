use aseman_network_legacy::TlsConfig;

/// The WebSocket client API driver interface.
pub trait IWs: Send + Sync {
    fn listen(&self, port: i64, tls_config: Option<TlsConfig>);
}
