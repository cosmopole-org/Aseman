//! TLS for every PostgreSQL connection: rustls (ring) verifying the server's
//! certificate and host name against the bundled Mozilla roots plus the database
//! URL's `sslrootcert`. A server that does not offer TLS is reached in plaintext only
//! when the URL's `sslmode` allows it (`prefer`, the default, or `disable`).

use std::future::Future;
use std::io;
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use postgres::tls::{ChannelBinding, MakeTlsConnect, TlsConnect, TlsStream};
use rustls::pki_types::{InvalidDnsNameError, ServerName};
use rustls::{ClientConfig, RootCertStore};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

/// Makes a verified TLS connection per PostgreSQL connection.
#[derive(Clone)]
pub struct TlsConnector {
    config: Arc<ClientConfig>,
}

impl std::fmt::Debug for TlsConnector {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("TlsConnector")
    }
}

impl TlsConnector {
    /// A connector trusting the Mozilla roots and, when given, the CA certificates
    /// in `extra_roots` (PEM).
    ///
    /// # Errors
    ///
    /// An unreadable or invalid CA file.
    pub fn new(extra_roots: Option<&Path>) -> Result<Self, String> {
        let mut roots = RootCertStore {
            roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
        };
        if let Some(path) = extra_roots {
            let pem = std::fs::read(path)
                .map_err(|error| format!("read sslrootcert {}: {error}", path.display()))?;
            let mut added = 0;
            for certificate in rustls_pemfile::certs(&mut pem.as_slice()) {
                let certificate = certificate
                    .map_err(|error| format!("parse sslrootcert {}: {error}", path.display()))?;
                roots
                    .add(certificate)
                    .map_err(|error| format!("trust sslrootcert {}: {error}", path.display()))?;
                added += 1;
            }
            if added == 0 {
                return Err(format!(
                    "sslrootcert {} holds no certificate",
                    path.display()
                ));
            }
        }
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let config = ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(|error| error.to_string())?
            .with_root_certificates(roots)
            .with_no_client_auth();
        Ok(Self {
            config: Arc::new(config),
        })
    }
}

impl<S> MakeTlsConnect<S> for TlsConnector
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    type Stream = Stream<S>;
    type TlsConnect = Connect;
    type Error = InvalidDnsNameError;

    fn make_tls_connect(&mut self, domain: &str) -> Result<Connect, InvalidDnsNameError> {
        Ok(Connect {
            connector: tokio_rustls::TlsConnector::from(self.config.clone()),
            server: ServerName::try_from(domain.to_owned())?,
        })
    }
}

/// One connection's TLS handshake.
pub struct Connect {
    connector: tokio_rustls::TlsConnector,
    server: ServerName<'static>,
}

impl<S> TlsConnect<S> for Connect
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    type Stream = Stream<S>;
    type Error = io::Error;
    type Future = Pin<Box<dyn Future<Output = io::Result<Stream<S>>> + Send>>;

    fn connect(self, stream: S) -> Self::Future {
        Box::pin(async move {
            self.connector
                .connect(self.server, stream)
                .await
                .map(|stream| Stream(Box::new(stream)))
        })
    }
}

/// A TLS stream to PostgreSQL.
pub struct Stream<S>(Box<tokio_rustls::client::TlsStream<S>>);

impl<S: AsyncRead + AsyncWrite + Unpin> AsyncRead for Stream<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut *self.0).poll_read(context, buffer)
    }
}

impl<S: AsyncRead + AsyncWrite + Unpin> AsyncWrite for Stream<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut *self.0).poll_write(context, buffer)
    }

    fn poll_flush(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut *self.0).poll_flush(context)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut *self.0).poll_shutdown(context)
    }
}

impl<S: AsyncRead + AsyncWrite + Unpin> TlsStream for Stream<S> {
    fn channel_binding(&self) -> ChannelBinding {
        ChannelBinding::none()
    }
}
