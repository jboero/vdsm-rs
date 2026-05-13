//! TLS material loading.
//!
//! The cert / key paths follow real VDSM convention so an engine-pushed
//! cert lands directly where the daemon expects it:
//!
//!   /etc/pki/vdsm/certs/vdsmcert.pem  — server cert
//!   /etc/pki/vdsm/keys/vdsmkey.pem    — server private key
//!   /etc/pki/vdsm/certs/cacert.pem    — engine CA (for client cert verify)

use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::sync::Once;

use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::ServerConfig;

#[derive(Debug, thiserror::Error)]
pub enum TlsError {
    #[error("read {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("no certificates in {path}")]
    NoCerts { path: String },
    #[error("no private key in {path}")]
    NoKey { path: String },
    #[error("rustls config: {0}")]
    Rustls(#[from] rustls::Error),
}

/// Install the `ring` CryptoProvider as rustls's process-wide default.
/// Idempotent; safe to call from multiple call sites.
pub fn install_default_provider() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

pub fn load_server_config(
    cert_path: &Path,
    key_path: &Path,
) -> Result<Arc<ServerConfig>, TlsError> {
    install_default_provider();

    let cert_bytes = fs::read(cert_path).map_err(|e| TlsError::Io {
        path: cert_path.display().to_string(),
        source: e,
    })?;
    let mut cert_reader = std::io::BufReader::new(&cert_bytes[..]);
    let certs: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut cert_reader)
        .filter_map(Result::ok)
        .collect();
    if certs.is_empty() {
        return Err(TlsError::NoCerts {
            path: cert_path.display().to_string(),
        });
    }

    let key_bytes = fs::read(key_path).map_err(|e| TlsError::Io {
        path: key_path.display().to_string(),
        source: e,
    })?;
    let mut key_reader = std::io::BufReader::new(&key_bytes[..]);
    let key: PrivateKeyDer<'static> = rustls_pemfile::private_key(&mut key_reader)
        .map_err(|e| TlsError::Io {
            path: key_path.display().to_string(),
            source: e,
        })?
        .ok_or_else(|| TlsError::NoKey {
            path: key_path.display().to_string(),
        })?;

    // Day-1: no client cert verification. Engine TLS will require it; we
    // wire the WebPkiClientVerifier in a follow-up once we have a real
    // engine CA cert path under /etc/pki/vdsm/certs/cacert.pem.
    let mut cfg = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)?;

    // Honor SSLKEYLOGFILE so wireshark/tshark can decrypt captured pcaps
    // when debugging engine-side parse issues. No-op when env var is unset.
    cfg.key_log = Arc::new(rustls::KeyLogFile::new());

    Ok(Arc::new(cfg))
}
