use serde::Deserialize;
use std::path::Path;

use crate::error::{Result, VdsmError};

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Config {
    pub daemon: DaemonConfig,
    pub rpc: RpcConfig,
    pub libvirt: LibvirtConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            daemon: DaemonConfig::default(),
            rpc: RpcConfig::default(),
            libvirt: LibvirtConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct DaemonConfig {
    pub user: String,
    pub group: String,
    pub state_dir: String,
    pub log_level: String,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            user: "vdsm".into(),
            group: "vdsm".into(),
            state_dir: "/var/lib/vdsm".into(),
            log_level: "info".into(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct RpcConfig {
    pub bind: String,
    pub port: u16,
    /// Set false for local dev (plain TCP). Production = true with the
    /// engine-pushed cert at the paths below.
    pub tls_enabled: bool,
    pub tls_cert: String,
    pub tls_key: String,
    pub tls_ca: String,
    /// Framing mode: "line" (newline-delimited JSON-RPC, useful for
    /// `openssl s_client` smoke tests) or "stomp" (STOMP 1.2, what
    /// real ovirt-engine speaks).
    pub framing: String,
}

impl Default for RpcConfig {
    fn default() -> Self {
        Self {
            bind: "0.0.0.0".into(),
            port: 54321,
            tls_enabled: true,
            tls_cert: "/etc/pki/vdsm/certs/vdsmcert.pem".into(),
            tls_key: "/etc/pki/vdsm/keys/vdsmkey.pem".into(),
            tls_ca: "/etc/pki/vdsm/certs/cacert.pem".into(),
            framing: "line".into(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct LibvirtConfig {
    pub uri: String,
}

impl Default for LibvirtConfig {
    fn default() -> Self {
        Self {
            uri: "qemu:///system".into(),
        }
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| VdsmError::Config(format!("{}: {e}", path.display())))?;
        let cfg: Config = toml::from_str(&text)?;
        Ok(cfg)
    }
}
