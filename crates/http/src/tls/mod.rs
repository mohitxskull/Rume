//! TLS acceptor integration (`rustls` + `tokio-rustls` + `hyper-util`) for Rume.

pub mod acceptor;
#[cfg(feature = "acme")]
pub mod acme;
pub mod config;

pub use acceptor::serve_tls_listener;
#[cfg(feature = "acme")]
pub use acme::AcmeConfig;
pub use config::{TlsConfig, TlsError};

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_CERT_PEM: &[u8] = b"\
-----BEGIN CERTIFICATE-----
MIIBwDCCAWWgAwIBAgIUIi3+ZI/tJFK4wTVlTFPD6QkG8XAwCgYIKoZIzj0EAwIw
FDESMBAGA1UEAwwJbG9jYWxob3N0MCAXDTI2MTEyNjEyMTYwMloYDzIxMDAxMDI5
MTIxNjAyWjAUMRIwEAYDVQQDDAlsb2NhbGhvc3QwWTATBgcqhkjOPQIBBggqhkjO
PQMBBwNCAATIw5glQvkynffHCd5iEclrqe+juD6xFx8A1F43+PqLjpra4wZ3non1
Dp6cLKCLOtYHLB05Rf2+7jxnMmyDVrryo4GSMIGPMB0GA1UdDgQWBBQQ8rRAB+Bs
+SsH4WcR+cJEbH+hTzAfBgNVHSMEGDAWgBQQ8rRAB+Bs+SsH4WcR+cJEbH+hTzAM
BgNVHRMBAf8EAjAAMA4GA1UdDwEB/wQEAwIFoDATBgNVHSUEDDAKBggrBgEFBQcD
ATAaBgNVHREEEzARgglsb2NhbGhvc3SHBH8AAAEwCgYIKoZIzj0EAwIDSQAwRgIh
APyYGMmbXQV05SjOkoWARBTijDUQ9v3rvdTvfFFJ8HXSAiEA/rJYP8/AcQalmlJL
MOAtrICgSzS3UyLAhl16728zqGc=
-----END CERTIFICATE-----
";

    const TEST_KEY_PEM: &[u8] = b"\
-----BEGIN PRIVATE KEY-----
MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgo2otzH6WgCrTvKb8
Mad8NskKE+yg2Tqk/S0mX41X1AGhRANCAATIw5glQvkynffHCd5iEclrqe+juD6x
Fx8A1F43+PqLjpra4wZ3non1Dp6cLKCLOtYHLB05Rf2+7jxnMmyDVrry
-----END PRIVATE KEY-----
";

    #[test]
    fn tls_config_from_pem_bytes_negotiates_h2_and_http11_alpn() {
        let cfg = TlsConfig::from_pem_bytes(TEST_CERT_PEM, TEST_KEY_PEM)
            .expect("valid PEM cert and key must build TlsConfig");
        let alpn = cfg.alpn_protocols();
        assert_eq!(alpn, &[b"h2".to_vec(), b"http/1.1".to_vec()]);
    }

    #[test]
    fn tls_config_rejects_invalid_pem_or_missing_key() {
        let bad_cert = TlsConfig::from_pem_bytes(b"not a pem", TEST_KEY_PEM);
        assert!(bad_cert.is_err(), "malformed certificate PEM must fail");

        let missing_key = TlsConfig::from_pem_bytes(TEST_CERT_PEM, b"");
        assert!(missing_key.is_err(), "missing private key PEM must fail");
    }
}
