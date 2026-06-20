use std::fs;
use std::path::PathBuf;
use anyhow::Result;
use libsql::{params, params_from_iter, Connection, Value};
use uuid::Uuid;

pub mod serde_pubkey {
    use serde::{Deserialize, Deserializer, Serializer};
    pub fn serialize<S>(key: &[u8; 32], serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_bytes(key)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<[u8; 32], D::Error>
    where
        D: Deserializer<'de>,
    {
        let bytes = Vec::<u8>::deserialize(deserializer)?;
        let array: [u8; 32] = bytes.try_into().map_err(|_| serde::de::Error::custom("invalid public key length"))?;
        Ok(array)
    }
}

pub mod serde_sig {
    use serde::{Deserialize, Deserializer, Serializer};
    pub fn serialize<S>(sig: &[u8; 64], serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_bytes(sig)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<[u8; 64], D::Error>
    where
        D: Deserializer<'de>,
    {
        let bytes = Vec::<u8>::deserialize(deserializer)?;
        let array: [u8; 64] = bytes.try_into().map_err(|_| serde::de::Error::custom("invalid signature length"))?;
        Ok(array)
    }
}

pub const VIBES: &[&str] = &["✨", "🌊", "🔥", "💀", "🌿", "👁️", "🫧"];

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Post {
    pub id: Uuid,
    #[serde(with = "serde_pubkey")]
    pub author_pubkey: [u8; 32],
    pub author_username: String,
    pub content: String,
    pub lat_coarse: f64,
    pub lon_coarse: f64,
    pub h3_cell: String,
    pub created_at: i64,
    pub expires_at: i64,
    #[serde(with = "serde_sig")]
    pub sig: [u8; 64],
}

impl Post {
    pub fn signature_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(self.id.as_bytes());
        bytes.extend_from_slice(&self.author_pubkey);
        bytes.extend_from_slice(self.content.as_bytes());
        bytes.extend_from_slice(&self.lat_coarse.to_be_bytes());
        bytes.extend_from_slice(&self.lon_coarse.to_be_bytes());
        bytes.extend_from_slice(self.h3_cell.as_bytes());
        bytes.extend_from_slice(&self.created_at.to_be_bytes());
        bytes.extend_from_slice(&self.expires_at.to_be_bytes());
        bytes
    }

    pub fn sign(&mut self, signing_key: &ed25519_dalek::SigningKey) {
        use ed25519_dalek::Signer;
        let bytes = self.signature_bytes();
        let signature = signing_key.sign(&bytes);
        self.sig = signature.to_bytes();
    }

    pub fn verify(&self) -> bool {
        use ed25519_dalek::Verifier;
        let bytes = self.signature_bytes();
        if let Ok(verifying_key) = ed25519_dalek::VerifyingKey::from_bytes(&self.author_pubkey) {
            if let Ok(signature) = ed25519_dalek::Signature::from_slice(&self.sig) {
                return verifying_key.verify(&bytes, &signature).is_ok();
            }
        }
        false
    }

    pub fn validate_and_clean(&mut self) -> bool {
        if !self.verify() {
            return false;
        }
        let now = chrono::Utc::now().timestamp();
        if self.created_at > now + 300 {
            return false;
        }
        if self.expires_at <= now {
            return false;
        }
        let re = regex::Regex::new(r"https?://[^\s]+").unwrap();
        self.content = re.replace_all(&self.content, "").to_string();
        if self.content.chars().count() > 280 {
            return false;
        }
        true
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Peer {
    #[serde(with = "serde_pubkey")]
    pub pubkey: [u8; 32],
    pub tunnel_url: String,
    pub last_seen: i64,
    pub failed_pings: i32,
    pub is_manual: i32,
}

#[derive(Clone)]
pub struct Store {
    conn: Connection,
}

fn get_murmur_dir() -> Option<PathBuf> {
    std::env::var("HOME")
        .ok()
        .map(PathBuf::from)
        .or_else(|| std::env::var("USERPROFILE").ok().map(PathBuf::from))
        .map(|h| h.join(".murmur"))
}

impl Store {
    pub async fn init(is_ephemeral: bool) -> Result<Self> {
        let db_path = if is_ephemeral {
            ":memory:".to_string()
        } else {
            let dir = get_murmur_dir().ok_or_else(|| anyhow::anyhow!("Could not get home dir"))?;
            if !dir.exists() {
                fs::create_dir_all(&dir)?;
            }
            dir.join("murmur.db").to_string_lossy().to_string()
        };

        let db = libsql::Builder::new_local(&db_path).build().await?;
        let conn = db.connect()?;

        // Configure WAL and locks
        conn.query("PRAGMA journal_mode = WAL;", ()).await?;
        conn.query("PRAGMA synchronous = NORMAL;", ()).await?;
        conn.query("PRAGMA busy_timeout = 5000;", ()).await?;

        // Initialize schema
        conn.execute(
            "CREATE TABLE IF NOT EXISTS posts (
                id          TEXT PRIMARY KEY,
                author_pk   BLOB NOT NULL,
                username    TEXT NOT NULL,
                content     TEXT NOT NULL,
                lat_coarse  REAL NOT NULL,
                lon_coarse  REAL NOT NULL,
                h3_cell     TEXT NOT NULL,
                created_at  INTEGER NOT NULL,
                expires_at  INTEGER NOT NULL,
                sig         BLOB NOT NULL
            );",
            (),
        )
        .await?;

        conn.execute(
            "CREATE TABLE IF NOT EXISTS peers (
                pubkey        BLOB PRIMARY KEY,
                tunnel_url    TEXT NOT NULL,
                last_seen     INTEGER NOT NULL,
                failed_pings  INTEGER DEFAULT 0,
                is_manual     INTEGER DEFAULT 0
            );",
            (),
        )
        .await?;

        conn.execute(
            "CREATE TABLE IF NOT EXISTS vibes (
                post_id   TEXT NOT NULL,
                vibe_type TEXT NOT NULL,
                PRIMARY KEY (post_id, vibe_type)
            );",
            (),
        )
        .await?;

        Ok(Store { conn })
    }

    pub async fn insert_post(&self, post: &Post) -> Result<()> {
        self.conn
            .execute(
                "INSERT OR REPLACE INTO posts (
                    id, author_pk, username, content, lat_coarse, lon_coarse, h3_cell, created_at, expires_at, sig
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                params![
                    post.id.to_string(),
                    post.author_pubkey.to_vec(),
                    post.author_username.clone(),
                    post.content.clone(),
                    post.lat_coarse,
                    post.lon_coarse,
                    post.h3_cell.clone(),
                    post.created_at,
                    post.expires_at,
                    post.sig.to_vec()
                ],
            )
            .await?;
        Ok(())
    }

    pub async fn insert_posts_batch(&self, posts: &[Post]) -> Result<()> {
        self.conn.execute("BEGIN TRANSACTION;", ()).await?;
        for post in posts {
            if let Err(e) = self.insert_post(post).await {
                let _ = self.conn.execute("ROLLBACK;", ()).await;
                return Err(e);
            }
        }
        self.conn.execute("COMMIT;", ()).await?;
        Ok(())
    }

    /// Retrieves posts belonging to the specified H3 cells.
    /// Note: SQLite has a default limit of 999 host parameters (variables) per query.
    /// Since the maximum resolution search yields a maximum of 7 cells (for resolution 8 disk 1),
    /// we are well within the safety margin.
    pub async fn get_posts_for_cells(&self, cells: &[String]) -> Result<Vec<Post>> {
        if cells.is_empty() {
            return Ok(vec![]);
        }

        let placeholders = vec!["?"; cells.len()].join(", ");
        let query_str = format!(
            "SELECT id, author_pk, username, content, lat_coarse, lon_coarse, h3_cell, created_at, expires_at, sig 
             FROM posts 
             WHERE h3_cell IN ({}) AND expires_at > ?
             ORDER BY created_at DESC",
            placeholders
        );

        let now = chrono::Utc::now().timestamp();
        
        let mut sql_params = Vec::new();
        for cell in cells {
            sql_params.push(Value::Text(cell.clone()));
        }
        sql_params.push(Value::Integer(now));

        let mut stmt = self.conn.prepare(&query_str).await?;
        let mut rows = stmt.query(params_from_iter(sql_params)).await?;

        let mut posts = Vec::new();
        while let Some(row) = rows.next().await? {
            let id_str: String = row.get(0)?;
            let id = Uuid::parse_str(&id_str)?;
            
            let author_pk_vec: Vec<u8> = row.get(1)?;
            let author_pubkey: [u8; 32] = author_pk_vec.try_into()
                .map_err(|_| anyhow::anyhow!("Invalid author pubkey size"))?;
                
            let author_username: String = row.get(2)?;
            let content: String = row.get(3)?;
            let lat_coarse: f64 = row.get(4)?;
            let lon_coarse: f64 = row.get(5)?;
            let h3_cell: String = row.get(6)?;
            let created_at: i64 = row.get(7)?;
            let expires_at: i64 = row.get(8)?;
            
            let sig_vec: Vec<u8> = row.get(9)?;
            let sig: [u8; 64] = sig_vec.try_into()
                .map_err(|_| anyhow::anyhow!("Invalid signature size"))?;

            posts.push(Post {
                id,
                author_pubkey,
                author_username,
                content,
                lat_coarse,
                lon_coarse,
                h3_cell,
                created_at,
                expires_at,
                sig,
            });
        }

        Ok(posts)
    }

    pub async fn get_posts_by_author(&self, author_pubkey: &[u8; 32]) -> Result<Vec<Post>> {
        let mut stmt = self.conn
            .prepare("SELECT id, author_pk, username, content, lat_coarse, lon_coarse, h3_cell, created_at, expires_at, sig 
                      FROM posts 
                      WHERE author_pk = ? AND expires_at > ?
                      ORDER BY created_at DESC")
            .await?;
        let now = chrono::Utc::now().timestamp();
        let mut rows = stmt.query(params![author_pubkey.to_vec(), now]).await?;

        let mut posts = Vec::new();
        while let Some(row) = rows.next().await? {
            let id_str: String = row.get(0)?;
            let id = Uuid::parse_str(&id_str)?;
            
            let author_pk_vec: Vec<u8> = row.get(1)?;
            let author_pubkey: [u8; 32] = author_pk_vec.try_into()
                .map_err(|_| anyhow::anyhow!("Invalid author pubkey size"))?;
                
            let author_username: String = row.get(2)?;
            let content: String = row.get(3)?;
            let lat_coarse: f64 = row.get(4)?;
            let lon_coarse: f64 = row.get(5)?;
            let h3_cell: String = row.get(6)?;
            let created_at: i64 = row.get(7)?;
            let expires_at: i64 = row.get(8)?;
            
            let sig_vec: Vec<u8> = row.get(9)?;
            let sig: [u8; 64] = sig_vec.try_into()
                .map_err(|_| anyhow::anyhow!("Invalid signature size"))?;

            posts.push(Post {
                id,
                author_pubkey,
                author_username,
                content,
                lat_coarse,
                lon_coarse,
                h3_cell,
                created_at,
                expires_at,
                sig,
            });
        }
        Ok(posts)
    }

    pub async fn delete_expired_posts(&self) -> Result<()> {
        let now = chrono::Utc::now().timestamp();
        self.conn
            .execute("DELETE FROM posts WHERE expires_at <= ?", params![now])
            .await?;
        Ok(())
    }

    pub async fn upsert_peer(&self, peer: &Peer) -> Result<()> {
        self.conn
            .execute(
                "INSERT OR REPLACE INTO peers (pubkey, tunnel_url, last_seen, failed_pings, is_manual) 
                 VALUES (?, ?, ?, ?, ?)",
                params![
                    peer.pubkey.to_vec(),
                    peer.tunnel_url.clone(),
                    peer.last_seen,
                    peer.failed_pings,
                    peer.is_manual
                ],
            )
            .await?;
        Ok(())
    }

    pub async fn get_active_peers(&self) -> Result<Vec<Peer>> {
        let mut stmt = self.conn.prepare("SELECT pubkey, tunnel_url, last_seen, failed_pings, is_manual FROM peers").await?;
        let mut rows = stmt.query(()).await?;

        let mut peers = Vec::new();
        while let Some(row) = rows.next().await? {
            let pk_vec: Vec<u8> = row.get(0)?;
            let pubkey: [u8; 32] = pk_vec.try_into()
                .map_err(|_| anyhow::anyhow!("Invalid pubkey size"))?;
            let tunnel_url: String = row.get(1)?;
            let last_seen: i64 = row.get(2)?;
            let failed_pings: i32 = row.get(3)?;
            let is_manual: i32 = row.get(4)?;

            peers.push(Peer {
                pubkey,
                tunnel_url,
                last_seen,
                failed_pings,
                is_manual,
            });
        }
        Ok(peers)
    }

    pub async fn increment_failed_pings(&self, pubkey: &[u8]) -> Result<()> {
        self.conn
            .execute(
                "UPDATE peers SET failed_pings = failed_pings + 1, last_seen = ? WHERE pubkey = ?",
                params![chrono::Utc::now().timestamp(), pubkey.to_vec()],
            )
            .await?;
        Ok(())
    }

    pub async fn reset_failed_pings(&self, pubkey: &[u8]) -> Result<()> {
        self.conn
            .execute(
                "UPDATE peers SET failed_pings = 0, last_seen = ? WHERE pubkey = ?",
                params![chrono::Utc::now().timestamp(), pubkey.to_vec()],
            )
            .await?;
        Ok(())
    }

    pub async fn remove_peer(&self, pubkey: &[u8]) -> Result<()> {
        self.conn
            .execute("DELETE FROM peers WHERE pubkey = ?", params![pubkey.to_vec()])
            .await?;
        Ok(())
    }

    pub async fn add_vibe(&self, post_id: &str, vibe_type: &str) -> Result<()> {
        self.conn
            .execute(
                "INSERT OR IGNORE INTO vibes (post_id, vibe_type) VALUES (?, ?)",
                params![post_id, vibe_type],
            )
            .await?;
        Ok(())
    }

    pub async fn get_vibes_for_post(&self, post_id: &str) -> Result<Vec<String>> {
        let mut stmt = self.conn
            .prepare("SELECT vibe_type FROM vibes WHERE post_id = ?")
            .await?;
        let mut rows = stmt.query(params![post_id]).await?;

        let mut vibes = Vec::new();
        while let Some(row) = rows.next().await? {
            let vibe: String = row.get(0)?;
            vibes.push(vibe);
        }
        Ok(vibes)
    }

    pub async fn is_post_active(&self, post_id: &str) -> Result<bool> {
        let mut stmt = self.conn.prepare("SELECT 1 FROM posts WHERE id = ? AND expires_at > ?").await?;
        let now = chrono::Utc::now().timestamp();
        let mut rows = stmt.query(params![post_id, now]).await?;
        Ok(rows.next().await?.is_some())
    }

    pub async fn get_latest_post_location(&self, author_pk: &[u8; 32]) -> Result<Option<(f64, f64)>> {
        let mut stmt = self.conn.prepare("SELECT lat_coarse, lon_coarse FROM posts WHERE author_pk = ? ORDER BY created_at DESC LIMIT 1").await?;
        let mut rows = stmt.query(params![author_pk.to_vec()]).await?;
        if let Some(row) = rows.next().await? {
            let lat: f64 = row.get(0)?;
            let lon: f64 = row.get(1)?;
            Ok(Some((lat, lon)))
        } else {
            Ok(None)
        }
    }
}

pub fn vibe_signature_bytes(post_id: &str, vibe_type: &str, sent_at: i64) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(post_id.as_bytes());
    bytes.extend_from_slice(vibe_type.as_bytes());
    bytes.extend_from_slice(&sent_at.to_be_bytes());
    bytes
}


#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;
    use rand::thread_rng;

    #[tokio::test]
    async fn test_post_signing_and_verification() {
        let mut rng = thread_rng();
        let signing_key = SigningKey::generate(&mut rng);
        let pubkey = signing_key.verifying_key().to_bytes();

        let mut post = Post {
            id: Uuid::new_v4(),
            author_pubkey: pubkey,
            author_username: "test-user".to_string(),
            content: "hello world! http://example.com/test check this out".to_string(),
            lat_coarse: 0.0,
            lon_coarse: 0.0,
            h3_cell: "8a2f1b34cd57fff".to_string(),
            created_at: chrono::Utc::now().timestamp(),
            expires_at: chrono::Utc::now().timestamp() + 3600,
            sig: [0u8; 64],
        };

        post.sign(&signing_key);
        assert!(post.verify(), "Post signature should verify successfully");

        let validated = post.validate_and_clean();
        assert!(validated, "Post should be valid");
        assert_eq!(post.content, "hello world!  check this out", "URLs should be stripped from the content");
    }

    #[tokio::test]
    async fn test_database_ephemeral_ops() {
        let store = Store::init(true).await.expect("Failed to initialize database");

        let mut rng = thread_rng();
        let signing_key = SigningKey::generate(&mut rng);
        let pubkey = signing_key.verifying_key().to_bytes();

        let mut post = Post {
            id: Uuid::new_v4(),
            author_pubkey: pubkey,
            author_username: "tester".to_string(),
            content: "Testing DB store".to_string(),
            lat_coarse: 12.34,
            lon_coarse: 56.78,
            h3_cell: "8a2f1b34cd57fff".to_string(),
            created_at: chrono::Utc::now().timestamp(),
            expires_at: chrono::Utc::now().timestamp() + 3600,
            sig: [0u8; 64],
        };
        post.sign(&signing_key);

        store.insert_post(&post).await.expect("Failed to insert post");

        let posts = store.get_posts_for_cells(&["8a2f1b34cd57fff".to_string()]).await.expect("Failed to query posts");
        assert_eq!(posts.len(), 1);
        assert_eq!(posts[0].id, post.id);

        let posts_by_author = store.get_posts_by_author(&pubkey).await.expect("Failed to query posts by author");
        assert_eq!(posts_by_author.len(), 1);
    }
}
