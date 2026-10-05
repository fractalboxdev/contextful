//! A store root's `config.toml`: node identity, at-rest encryption, bucket sync and the
//! canonical store a replica reads from, and store-wide connector policy.

use super::sync::SyncConfig;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The store root's `config.toml`. An unknown table or key refuses to parse.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoreConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sync: Option<SyncConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node: Option<NodeConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encryption: Option<EncryptionConfig>,
    /// Present on a consuming replica: the canonical store it reads from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replica: Option<ReplicaConfig>,
    /// Store-wide connector policy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connector: Option<ConnectorPolicy>,
}

/// `[connector]`: store-wide connector policy.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectorPolicy {
    /// The store-wide switch of `connector.package.pin-requirement`: every local component
    /// artifact this store lands from carries a pin.
    #[serde(default)]
    pub require_pin: bool,
    /// OCI registry bearer bindings by exact authority (`connector.package.oci-registry-bearer`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub registry: BTreeMap<String, RegistryAuthorization>,
}

/// One OCI registry's authorization template.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistryAuthorization {
    pub authorization: String,
}

/// `[node]`: this machine's configured node id (`store.lay-out.node-id-order`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
}

/// `[encryption]`: where the project key comes from (`store.encrypt.key-binding`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EncryptionConfig {
    pub key_source: String,
}

/// `[replica]`: the canonical store a replica answers for.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReplicaConfig {
    pub of: String,
}
