use std::fs;
use std::path::PathBuf;
use ed25519_dalek::SigningKey;
use rand::{rngs::SmallRng, RngCore, SeedableRng};

const ADJECTIVES: &[&str] = &[
    "wandering", "silent", "hollow", "pale", "iron", "drifting", "restless", "copper",
    "forgotten", "golden", "shadowy", "frosty", "burning", "sleeping", "hidden", "broken",
    "crimson", "azure", "misty", "wild", "still", "dusty", "crying", "dancing"
];

const NOUNS: &[&str] = &[
    "pike", "crow", "dusk", "moth", "veil", "ash", "cloud", "sparrow", "stone", "river",
    "forest", "wind", "wolf", "hawk", "owl", "leaf", "thorn", "root", "ridge", "flame",
    "wave", "echo", "star", "path"
];

#[derive(Clone)]
pub struct Identity {
    pub signing_key: SigningKey,
    pub username: String,
    pub is_ephemeral: bool,
}

impl Identity {
    pub fn public_key_bytes(&self) -> [u8; 32] {
        self.signing_key.verifying_key().to_bytes()
    }
}

pub fn derive_username(pubkey: &[u8; 32]) -> String {
    let seed = u64::from_le_bytes(pubkey[0..8].try_into().unwrap());
    let mut rng = SmallRng::seed_from_u64(seed);

    let adj_idx = (rng.next_u32() as usize) % ADJECTIVES.len();
    let noun_idx = (rng.next_u32() as usize) % NOUNS.len();
    let num = (rng.next_u32() % 9000) + 1000;

    format!("{}-{}-{}", ADJECTIVES[adj_idx], NOUNS[noun_idx], num)
}

fn get_home_dir() -> Option<PathBuf> {
    std::env::var("HOME")
        .ok()
        .map(PathBuf::from)
        .or_else(|| std::env::var("USERPROFILE").ok().map(PathBuf::from))
}

pub fn load_or_create(is_ephemeral: bool) -> anyhow::Result<Identity> {
    if is_ephemeral {
        let mut rng = rand::thread_rng();
        let signing_key = SigningKey::generate(&mut rng);
        let pubkey = signing_key.verifying_key().to_bytes();
        let username = derive_username(&pubkey);
        return Ok(Identity {
            signing_key,
            username,
            is_ephemeral: true,
        });
    }

    let home = get_home_dir().ok_or_else(|| anyhow::anyhow!("Could not find home directory"))?;
    let murmur_dir = home.join(".murmur");
    let key_file = murmur_dir.join("identity.key");

    if !murmur_dir.exists() {
        fs::create_dir_all(&murmur_dir)?;
    }

    let signing_key = if key_file.exists() {
        let bytes = fs::read(&key_file)?;
        if bytes.len() != 32 {
            anyhow::bail!("Invalid identity key file length");
        }
        let array: [u8; 32] = bytes.try_into().map_err(|_| anyhow::anyhow!("Invalid bytes"))?;
        SigningKey::from_bytes(&array)
    } else {
        let mut rng = rand::thread_rng();
        let signing_key = SigningKey::generate(&mut rng);
        let bytes = signing_key.to_bytes();
        
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            let mut options = fs::OpenOptions::new();
            options.write(true).create(true).mode(0o600);
            let mut file = options.open(&key_file)?;
            use std::io::Write;
            file.write_all(&bytes)?;
        }
        #[cfg(not(unix))]
        {
            fs::write(&key_file, &bytes)?;
        }
        signing_key
    };

    let pubkey = signing_key.verifying_key().to_bytes();
    let username = derive_username(&pubkey);

    Ok(Identity {
        signing_key,
        username,
        is_ephemeral: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_username_derivation() {
        let pubkey = [0u8; 32];
        let name1 = derive_username(&pubkey);
        let name2 = derive_username(&pubkey);
        assert_eq!(name1, name2, "Derivation should be deterministic");
        
        let mut another_pubkey = [0u8; 32];
        another_pubkey[0] = 1;
        let name3 = derive_username(&another_pubkey);
        assert_ne!(name1, name3, "Different public keys should yield different names");
    }
}
