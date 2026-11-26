//! ACME configuration for automated TLS certificate provisioning (`rustls-acme`).

use std::fmt::Debug;
use std::path::PathBuf;
use std::sync::Arc;

use futures_util::StreamExt;
use rustls_acme::acme::{LETS_ENCRYPT_PRODUCTION_DIRECTORY, LETS_ENCRYPT_STAGING_DIRECTORY};
use rustls_acme::caches::{DirCache, NoCache};

use super::config::{TlsError, default_crypto_provider};

/// Configuration for automated ACME certificate provisioning (`rustls-acme`).
#[derive(Clone, Debug)]
pub struct AcmeConfig {
    pub(crate) domains: Vec<String>,
    pub(crate) contact: Vec<String>,
    pub(crate) directory_url: String,
    pub(crate) cache_dir: Option<PathBuf>,
}

impl AcmeConfig {
    /// Create a new ACME configuration for the given domain names.
    ///
    /// Defaults to the Let's Encrypt staging directory so development builds do
    /// not exhaust production rate limits; call [`lets_encrypt_production(true)`](Self::lets_encrypt_production)
    /// or [`directory_url`](Self::directory_url) for production certificates.
    pub fn new(domains: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            domains: domains.into_iter().map(Into::into).collect(),
            contact: Vec::new(),
            directory_url: LETS_ENCRYPT_STAGING_DIRECTORY.to_string(),
            cache_dir: None,
        }
    }

    /// Set the ACME directory URL.
    pub fn directory_url(mut self, url: impl Into<String>) -> Self {
        self.directory_url = url.into();
        self
    }

    /// Select Let's Encrypt production (`true`) or staging (`false`) directory.
    pub fn lets_encrypt_production(mut self, production: bool) -> Self {
        self.directory_url = if production {
            LETS_ENCRYPT_PRODUCTION_DIRECTORY
        } else {
            LETS_ENCRYPT_STAGING_DIRECTORY
        }
        .to_string();
        self
    }

    /// Set contact emails for the ACME account (auto-prefixing `mailto:` if omitted).
    pub fn contact(mut self, emails: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.contact = emails
            .into_iter()
            .map(Into::into)
            .map(normalize_contact)
            .collect();
        self
    }

    /// Add a single contact email for the ACME account.
    pub fn contact_push(mut self, email: impl Into<String>) -> Self {
        self.contact.push(normalize_contact(email.into()));
        self
    }

    /// Set the filesystem directory for caching ACME account keys and certificates.
    pub fn cache_dir(mut self, path: impl Into<PathBuf>) -> Self {
        self.cache_dir = Some(path.into());
        self
    }

    pub(crate) fn build_acceptor(
        &self,
        alpn_protocols: &[Vec<u8>],
    ) -> Result<(tokio_rustls::TlsAcceptor, tokio::task::JoinHandle<()>), TlsError> {
        let provider = default_crypto_provider();
        let base = rustls_acme::AcmeConfig::new_with_provider(&self.domains, provider.clone())
            .directory(&self.directory_url)
            .contact(&self.contact);

        let configured: rustls_acme::AcmeConfig<Box<dyn Debug>> = match &self.cache_dir {
            Some(dir) => base.cache_with_boxed_err(DirCache::new(dir.clone())),
            None => base.cache_with_boxed_err(NoCache::<std::io::Error>::default()),
        };

        let mut state = configured.state();
        let mut server_config = rustls::ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(|err| TlsError::Rustls(err.to_string()))?
            .with_no_client_auth()
            .with_cert_resolver(state.resolver());
        server_config.alpn_protocols = {
            let mut alpn = alpn_protocols.to_vec();
            let acme_alpn = rustls_acme::acme::ACME_TLS_ALPN_NAME.to_vec();
            if !alpn.contains(&acme_alpn) {
                alpn.push(acme_alpn);
            }
            alpn
        };

        let driver = tokio::spawn(async move {
            while let Some(event) = state.next().await {
                match event {
                    Ok(ok) => tracing::info!(?ok, "ACME certificate event"),
                    Err(err) => tracing::warn!(error = ?err, "ACME certificate event failed"),
                }
            }
        });

        Ok((
            tokio_rustls::TlsAcceptor::from(Arc::new(server_config)),
            driver,
        ))
    }
}

fn normalize_contact(email: String) -> String {
    if email.starts_with("mailto:") {
        email
    } else {
        format!("mailto:{email}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn acme_build_acceptor_includes_acme_tls_alpn() {
        let cfg = AcmeConfig::new(["example.com"]).contact(["ops@example.com"]);
        let alpn = [b"h2".to_vec(), b"http/1.1".to_vec()];
        let (acceptor, driver) = cfg
            .build_acceptor(&alpn)
            .expect("AcmeConfig::build_acceptor should succeed");
        let server_alpn = &acceptor.config().alpn_protocols;
        assert!(
            server_alpn.contains(&rustls_acme::acme::ACME_TLS_ALPN_NAME.to_vec()),
            "ACME acceptor must advertise acme-tls/1 ALPN, got: {server_alpn:?}"
        );
        assert!(server_alpn.contains(&b"h2".to_vec()));
        assert!(server_alpn.contains(&b"http/1.1".to_vec()));
        driver.abort();
    }
}
