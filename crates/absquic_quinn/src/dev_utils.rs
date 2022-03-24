//! helper utilities to aid in development

use once_cell::sync::Lazy;
use std::sync::Arc;

static LOCAL_EPHEM_CERT: Lazy<(rustls::Certificate, rustls::PrivateKey)> =
    Lazy::new(|| {
        let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()])
            .unwrap();
        let pk = rustls::PrivateKey(cert.serialize_private_key_der());
        let cert = rustls::Certificate(cert.serialize_der().unwrap());
        (cert, pk)
    });

/// get the "localhost" ephemeral self signed tls certificate
pub fn local_ephem_tls_cert() -> (rustls::Certificate, rustls::PrivateKey) {
    LOCAL_EPHEM_CERT.clone()
}

/// get a simple server config based on a single cert / private key
pub fn simple_server_config(
    cert: rustls::Certificate,
    pk: rustls::PrivateKey,
) -> quinn_proto::ServerConfig {
    quinn_proto::ServerConfig::with_single_cert(vec![cert], pk).unwrap()
}

/// get a trusting client config that accepts all certificates
pub fn trusting_client_config() -> quinn_proto::ClientConfig {
    let client_config = rustls::ClientConfig::builder()
        .with_safe_defaults()
        .with_custom_certificate_verifier(Arc::new(V))
        .with_no_client_auth();
    quinn_proto::ClientConfig::new(Arc::new(client_config))
}

struct V;

impl rustls::client::ServerCertVerifier for V {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::Certificate,
        _intermediates: &[rustls::Certificate],
        _server_name: &rustls::ServerName,
        _scts: &mut dyn Iterator<Item = &[u8]>,
        _ocsp_response: &[u8],
        _now: std::time::SystemTime,
    ) -> Result<rustls::client::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::ServerCertVerified::assertion())
    }
}
