use std::sync::{Arc, Mutex};
use std::time::Duration;
use anyhow::Result;
use mainline::{Dht, MutableItem};
use sha3::{Digest as Sha3Digest, Sha3_256};
use serde::{Deserialize, Serialize};
use crate::identity::Identity;
use crate::store::{Peer, Store};

pub struct DhtNode {
    dht: Dht,
    udp_port: u16,
    identity: Identity,
    #[allow(dead_code)]
    store: Store,
    url_state: Arc<Mutex<Option<String>>>,
}

/// Derives a 20-byte Kademlia Node ID from a 32-byte Ed25519 public key.
/// Note: Although the murmur spec outlines a 32-byte node ID space,
/// BitTorrent's Mainline DHT protocol strictly requires 20-byte IDs (160-bit),
/// so we truncate the SHA3-256 hash output to 20 bytes for compatibility.
pub fn derive_node_id(pubkey: &[u8; 32]) -> [u8; 20] {
    let mut hasher = Sha3_256::new();
    hasher.update(pubkey);
    let result = hasher.finalize();
    let mut id = [0u8; 20];
    id.copy_from_slice(&result[0..20]);
    id
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PeerRecord {
    #[serde(with = "crate::store::serde_pubkey")]
    pub pubkey: [u8; 32],
    pub tunnel_url: String,
    pub username: String,
    pub announced_at: i64,
    pub ttl_seconds: i64,
    #[serde(with = "crate::store::serde_sig")]
    pub sig: [u8; 64],
}

impl PeerRecord {
    pub fn signature_bytes(
        tunnel_url: &str,
        pubkey: &[u8; 32],
        username: &str,
        announced_at: i64,
        ttl_seconds: i64,
    ) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(tunnel_url.as_bytes());
        bytes.extend_from_slice(pubkey);
        bytes.extend_from_slice(username.as_bytes());
        bytes.extend_from_slice(&announced_at.to_be_bytes());
        bytes.extend_from_slice(&ttl_seconds.to_be_bytes());
        bytes
    }

    pub fn sign(&mut self, signing_key: &ed25519_dalek::SigningKey) {
        use ed25519_dalek::Signer;
        let bytes = Self::signature_bytes(&self.tunnel_url, &self.pubkey, &self.username, self.announced_at, self.ttl_seconds);
        let signature = signing_key.sign(&bytes);
        self.sig = signature.to_bytes();
    }

    pub fn verify(&self) -> bool {
        use ed25519_dalek::{Verifier, Signature, VerifyingKey};
        let bytes = Self::signature_bytes(&self.tunnel_url, &self.pubkey, &self.username, self.announced_at, self.ttl_seconds);
        if let Ok(verifying_key) = VerifyingKey::from_bytes(&self.pubkey) {
            if let Ok(signature) = Signature::from_slice(&self.sig) {
                return verifying_key.verify(&bytes, &signature).is_ok();
            }
        }
        false
    }
}

fn serialize_peer_records(records: &[PeerRecord]) -> Result<Vec<u8>> {
    let mut buf = Vec::new();
    ciborium::into_writer(records, &mut buf)?;
    Ok(buf)
}

fn deserialize_peer_records(bytes: &[u8]) -> Result<Vec<PeerRecord>> {
    let records: Vec<PeerRecord> = ciborium::from_reader(bytes)?;
    Ok(records)
}

pub fn cell_signing_key(cell_str: &str) -> mainline::SigningKey {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(cell_str.as_bytes());
    let seed = hasher.finalize();
    let seed_bytes: [u8; 32] = seed.into();
    mainline::SigningKey::from_bytes(&seed_bytes)
}

impl DhtNode {
    pub async fn new(
        identity: Identity,
        store: Store,
        url_state: Arc<Mutex<Option<String>>>,
    ) -> Result<Self> {
        let dht = Dht::builder()
            .extra_bootstrap(&[
                "router.bittorrent.com:6881",
                "router.utorrent.com:6881",
                "dht.transmissionbt.com:6881",
                "82.221.103.244:6881", // router.bittorrent.com IP fallback
                "212.129.33.50:6881",  // transmissionbt IP fallback
            ])
            .build()?;

        let udp_port = dht.info().local_addr().port();

        Ok(Self {
            dht,
            udp_port,
            identity,
            store,
            url_state,
        })
    }

    pub fn udp_port(&self) -> u16 {
        self.udp_port
    }

    pub async fn announce_h3_cell(&self, cell_str: &str) -> Result<()> {
        let our_url = {
            let lock = self.url_state.lock().unwrap();
            lock.clone()
        };

        let our_url = match our_url {
            Some(url) => url,
            None => {
                tracing::warn!("Cannot announce H3 cell: Tunnel URL not available yet");
                return Ok(());
            }
        };

        let cell_signing_key = cell_signing_key(cell_str);
        let cell_pubkey = cell_signing_key.verifying_key().to_bytes();
        let async_dht = self.dht.clone().as_async();

        let mut attempts = 0;
        loop {
            attempts += 1;
            let maybe_item = async_dht.get_mutable_most_recent(&cell_pubkey, None).await;
            let now = chrono::Utc::now().timestamp();

            let mut records = if let Some(ref item) = maybe_item {
                deserialize_peer_records(item.value()).unwrap_or_default()
            } else {
                Vec::new()
            };

            // Filter expired and self/invalid
            let our_pubkey = self.identity.public_key_bytes();
            records.retain(|r| r.announced_at + r.ttl_seconds >= now && r.pubkey != our_pubkey && r.verify());

            // Build our record
            let mut our_record = PeerRecord {
                pubkey: our_pubkey,
                tunnel_url: our_url.clone(),
                username: self.identity.username.clone(),
                announced_at: now,
                ttl_seconds: 45 * 60, // 45 minutes
                sig: [0u8; 64],
            };
            our_record.sign(&self.identity.signing_key);
            records.push(our_record);

            // Cap the list to keep payload under 1000 bytes
            records.sort_by_key(|r| -r.announced_at);
            records.truncate(6);

            let serialized = match serialize_peer_records(&records) {
                Ok(bytes) => bytes,
                Err(e) => {
                    tracing::error!("Failed to serialize peer records: {:?}", e);
                    break;
                }
            };

            let (seq, cas) = if let Some(ref item) = maybe_item {
                (item.seq() + 1, Some(item.seq()))
            } else {
                (1, None)
            };

            let new_item = MutableItem::new(cell_signing_key.clone(), &serialized, seq, None);
            match async_dht.put_mutable(new_item, cas).await {
                Ok(_) => {
                    tracing::info!("Announced peer record to DHT cell {}: {} peers in list", cell_str, records.len());
                    break;
                }
                Err(e) => {
                    if attempts >= 3 {
                        tracing::error!("Failed to put mutable peer record after 3 attempts: {:?}", e);
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
        }

        Ok(())
    }

    pub async fn discover_peers(&self, cell_str: &str) -> Result<Vec<Peer>> {
        let cell_signing_key = cell_signing_key(cell_str);
        let cell_pubkey = cell_signing_key.verifying_key().to_bytes();
        let async_dht = self.dht.clone().as_async();

        let maybe_item = async_dht.get_mutable_most_recent(&cell_pubkey, None).await;
        let mut peers = Vec::new();

        if let Some(item) = maybe_item {
            let records = deserialize_peer_records(item.value()).unwrap_or_default();
            let now = chrono::Utc::now().timestamp();
            let our_pubkey = self.identity.public_key_bytes();

            for record in records {
                if record.pubkey == our_pubkey {
                    continue;
                }
                if record.announced_at + record.ttl_seconds < now {
                    continue;
                }
                if !record.verify() {
                    tracing::warn!("Discarded forged or invalid signature peer record from {}", record.username);
                    continue;
                }

                peers.push(Peer {
                    pubkey: record.pubkey,
                    tunnel_url: record.tunnel_url,
                    last_seen: now,
                    failed_pings: 0,
                    is_manual: 0,
                });
            }
        }

        Ok(peers)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::load_or_create;

    #[test]
    fn test_peer_record_signing_and_verification() {
        let identity = load_or_create(true).unwrap();
        let pubkey = identity.public_key_bytes();
        let url = "https://xyz.trycloudflare.com".to_string();
        let username = identity.username.clone();
        let now = chrono::Utc::now().timestamp();

        let mut record = PeerRecord {
            pubkey,
            tunnel_url: url.clone(),
            username: username.clone(),
            announced_at: now,
            ttl_seconds: 2700,
            sig: [0u8; 64],
        };

        record.sign(&identity.signing_key);
        assert!(record.verify(), "Record signature should verify successfully");

        // Tamper with record
        record.tunnel_url = "https://hacked.com".to_string();
        assert!(!record.verify(), "Tampered record should fail verification");
    }

    #[test]
    fn test_peer_records_serialization() {
        let identity = load_or_create(true).unwrap();
        let pubkey = identity.public_key_bytes();
        let url = "https://xyz.trycloudflare.com".to_string();
        let username = identity.username.clone();
        let now = chrono::Utc::now().timestamp();

        let mut record = PeerRecord {
            pubkey,
            tunnel_url: url.clone(),
            username: username.clone(),
            announced_at: now,
            ttl_seconds: 2700,
            sig: [0u8; 64],
        };
        record.sign(&identity.signing_key);

        let list = vec![record.clone()];
        let serialized = serialize_peer_records(&list).unwrap();
        let deserialized = deserialize_peer_records(&serialized).unwrap();

        assert_eq!(list, deserialized);
    }
}
