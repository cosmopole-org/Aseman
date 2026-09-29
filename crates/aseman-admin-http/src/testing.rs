//! Throwaway mutual-TLS identities for tests: one CA, and identities it issues for
//! `localhost` and `127.0.0.1`, written as [`TlsFiles`] under a directory.

use std::path::Path;

use aseman_config::TlsFiles;
use rcgen::{BasicConstraints, CertificateParams, CertifiedIssuer, IsCa, KeyPair};

/// A CA that issues test identities.
pub struct TestAuthority {
    issuer: CertifiedIssuer<'static, KeyPair>,
}

impl TestAuthority {
    /// # Panics
    ///
    /// When the CA cannot be generated.
    #[must_use]
    pub fn new(name: &str) -> Self {
        let mut params = CertificateParams::new(Vec::<String>::new()).expect("CA parameters");
        params
            .distinguished_name
            .push(rcgen::DnType::CommonName, name.to_owned());
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        Self {
            issuer: CertifiedIssuer::self_signed(params, KeyPair::generate().expect("CA key"))
                .expect("CA certificate"),
        }
    }

    /// An identity named `name` for `localhost`, written under `directory`.
    ///
    /// # Panics
    ///
    /// When the identity cannot be generated or written.
    #[must_use]
    pub fn identity(&self, directory: &Path, name: &str) -> TlsFiles {
        std::fs::create_dir_all(directory).expect("identity directory");
        let key = KeyPair::generate().expect("identity key");
        let certificate =
            CertificateParams::new(vec!["localhost".to_owned(), "127.0.0.1".to_owned()])
                .expect("identity parameters")
                .signed_by(&key, &self.issuer)
                .expect("identity certificate");
        let files = TlsFiles {
            certificate: directory.join(format!("{name}-cert.pem")),
            key_secret: directory.join(format!("{name}-key.pem")),
            ca: directory.join(format!("{name}-ca.pem")),
        };
        std::fs::write(&files.certificate, certificate.pem()).expect("write certificate");
        std::fs::write(&files.key_secret, key.serialize_pem()).expect("write key");
        std::fs::write(&files.ca, self.issuer.pem()).expect("write CA");
        files
    }
}
