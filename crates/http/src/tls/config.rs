//! TLS configuration (`TlsConfig`, PEM loading, and error types).

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use rustls::ServerConfig;
use rustls::crypto::CryptoProvider;
use rustls_pki_types::pem::PemObject;
use rustls_pki_types::{CertificateDer, PrivateKeyDer};

const DEFAULT_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const DEFAULT_MAX_CONNECTIONS: usize = 4096;

/// Errors returned while loading or configuring TLS.
#[derive(Debug, thiserror::Error)]
pub enum TlsError {
    /// I/O error reading PEM files.
    #[error("TLS I/O error: {0}")]
    Io(#[from] std::io::Error),
    /// Invalid certificate PEM data.
    #[error("invalid TLS certificate: {0}")]
    InvalidCertificate(String),
    /// Invalid private key PEM data.
    #[error("invalid TLS private key: {0}")]
    InvalidPrivateKey(String),
    /// No certificates found in PEM input.
    #[error("no TLS certificates found in PEM input")]
    MissingCertificate,
    /// No private key found in PEM input.
    #[error("no TLS private key found in PEM input")]
    MissingPrivateKey,
    /// Underlying `rustls` configuration error.
    #[error("rustls configuration error: {0}")]
    Rustls(String),
}

#[derive(Clone, Debug)]
enum TlsConfigKind {
    Static(Arc<ServerConfig>),
    #[cfg(feature = "acme")]
    Acme(super::AcmeConfig),
}

/// TLS server configuration for Rume.
#[derive(Clone, Debug)]
pub struct TlsConfig {
    kind: TlsConfigKind,
    alpn_protocols: Vec<Vec<u8>>,
    pub(crate) handshake_timeout: Duration,
    pub(crate) max_connections: usize,
}

impl TlsConfig {
    /// Load a certificate chain and private key from PEM files on disk.
    pub fn from_pem_files(
        cert_path: impl AsRef<Path>,
        key_path: impl AsRef<Path>,
    ) -> Result<Self, TlsError> {
        let cert_pem = std::fs::read(cert_path)?;
        let key_pem = std::fs::read(key_path)?;
        Self::from_pem_bytes(&cert_pem, &key_pem)
    }

    /// Parse a certificate chain and private key from in-memory PEM bytes.
    pub fn from_pem_bytes(cert_pem: &[u8], key_pem: &[u8]) -> Result<Self, TlsError> {
        let certs = parse_certs(cert_pem)?;
        let key = parse_private_key(key_pem)?;

        let alpn_protocols = default_alpn_protocols();
        let mut server_config = ServerConfig::builder_with_provider(default_crypto_provider())
            .with_safe_default_protocol_versions()
            .map_err(|err| TlsError::Rustls(err.to_string()))?
            .with_no_client_auth()
            .with_single_cert(certs, key)
            .map_err(|err| TlsError::Rustls(err.to_string()))?;
        server_config.alpn_protocols = alpn_protocols.clone();

        Ok(Self {
            kind: TlsConfigKind::Static(Arc::new(server_config)),
            alpn_protocols,
            handshake_timeout: DEFAULT_HANDSHAKE_TIMEOUT,
            max_connections: DEFAULT_MAX_CONNECTIONS,
        })
    }

    /// Configure automated certificate provisioning via ACME (`rustls-acme`).
    #[cfg(feature = "acme")]
    pub fn acme(config: super::AcmeConfig) -> Self {
        Self {
            kind: TlsConfigKind::Acme(config),
            alpn_protocols: default_alpn_protocols(),
            handshake_timeout: DEFAULT_HANDSHAKE_TIMEOUT,
            max_connections: DEFAULT_MAX_CONNECTIONS,
        }
    }

    /// Set the bounded TLS handshake timeout (default: 10 seconds).
    pub fn handshake_timeout(mut self, timeout: Duration) -> Self {
        self.handshake_timeout = timeout;
        self
    }

    /// Set the maximum number of concurrent TLS connections and in-flight handshakes (default: 4096).
    pub fn max_connections(mut self, max: usize) -> Self {
        self.max_connections = max.max(1);
        self
    }

    /// Return the configured ALPN protocols in preference order.
    pub fn alpn_protocols(&self) -> &[Vec<u8>] {
        &self.alpn_protocols
    }

    #[cfg(test)]
    pub(crate) fn with_extra_alpn_for_tests(mut self, proto: Vec<u8>) -> Self {
        self.alpn_protocols.push(proto);
        if let TlsConfigKind::Static(ref server_cfg) = self.kind {
            let mut updated = (**server_cfg).clone();
            updated.alpn_protocols = self.alpn_protocols.clone();
            self.kind = TlsConfigKind::Static(Arc::new(updated));
        }
        self
    }

    pub(crate) fn build_acceptor(
        &self,
    ) -> Result<
        (
            tokio_rustls::TlsAcceptor,
            Option<tokio::task::JoinHandle<()>>,
        ),
        TlsError,
    > {
        match &self.kind {
            TlsConfigKind::Static(server_config) => Ok((
                tokio_rustls::TlsAcceptor::from(Arc::clone(server_config)),
                None,
            )),
            #[cfg(feature = "acme")]
            TlsConfigKind::Acme(acme_cfg) => {
                let (acceptor, driver) = acme_cfg.build_acceptor(&self.alpn_protocols)?;
                Ok((acceptor, Some(driver)))
            }
        }
    }
}

fn default_alpn_protocols() -> Vec<Vec<u8>> {
    vec![b"h2".to_vec(), b"http/1.1".to_vec()]
}

pub(crate) fn default_crypto_provider() -> Arc<CryptoProvider> {
    // Why: when both `aws-lc-rs` and `ring` features are unified across workspace
    // dependencies, `ServerConfig::builder()` panics without an explicit provider.
    CryptoProvider::get_default()
        .cloned()
        .unwrap_or_else(|| Arc::new(rustls::crypto::aws_lc_rs::default_provider()))
}

fn parse_certs(cert_pem: &[u8]) -> Result<Vec<CertificateDer<'static>>, TlsError> {
    let certs: Vec<CertificateDer<'static>> = CertificateDer::pem_slice_iter(cert_pem)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|err| TlsError::InvalidCertificate(err.to_string()))?;
    if certs.is_empty() {
        return Err(TlsError::MissingCertificate);
    }
    Ok(certs)
}

fn parse_private_key(key_pem: &[u8]) -> Result<PrivateKeyDer<'static>, TlsError> {
    match PrivateKeyDer::from_pem_slice(key_pem) {
        Ok(key) => Ok(key),
        Err(rustls_pki_types::pem::Error::NoItemsFound) => Err(TlsError::MissingPrivateKey),
        Err(err) => Err(TlsError::InvalidPrivateKey(err.to_string())),
    }
}
