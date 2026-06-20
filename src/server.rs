use std::sync::Arc;
use std::time::Instant;
use axum::{
    extract::{State, Json},
    routing::{get, post},
    Router,
};
use serde::{Deserialize, Serialize};
use tower_governor::{governor::GovernorConfigBuilder, GovernorLayer};

use crate::identity::Identity;
use crate::store::{Post, Store};

#[derive(Clone)]
pub struct ServerState {
    pub identity: Identity,
    pub store: Store,
    pub udp_port: u16,
    pub start_time: Instant,
}

#[derive(Serialize, Deserialize)]
struct VibeRequest {
    post_id: String,
    vibe_type: String,
    sent_at: i64,
    #[serde(with = "crate::store::serde_pubkey")]
    from_pubkey: [u8; 32],
    #[serde(with = "crate::store::serde_sig")]
    sig: [u8; 64],
}

#[derive(Serialize, Deserialize)]
struct PingResponse {
    username: String,
    public_key: String,
    uptime_seconds: u64,
    udp_port: u16,
}

async fn get_feed(State(state): State<Arc<ServerState>>) -> Result<Json<Vec<Post>>, String> {
    let posts = state.store.get_posts_by_author(&state.identity.public_key_bytes())
        .await
        .map_err(|e| format!("Database error: {:?}", e))?;
    Ok(Json(posts))
}

async fn post_vibe(
    State(state): State<Arc<ServerState>>,
    Json(payload): Json<VibeRequest>,
) -> Result<String, String> {
    let active = state.store.is_post_active(&payload.post_id)
        .await
        .map_err(|e| format!("Database error: {:?}", e))?;
    if !active {
        return Err("Post not found or expired".to_string());
    }

    let allowed_vibes = ["✨", "🌊", "🔥", "💀", "🌿", "👁️", "👁", "🫧"];
    if !allowed_vibes.contains(&payload.vibe_type.as_str()) {
        return Err("Invalid vibe type".to_string());
    }

    let now = chrono::Utc::now().timestamp();
    if (payload.sent_at - now).abs() > 300 {
        return Err("Vibe request timed out".to_string());
    }

    let msg = crate::store::vibe_signature_bytes(&payload.post_id, &payload.vibe_type, payload.sent_at);
    use ed25519_dalek::{Verifier, Signature, VerifyingKey};
    let verifying_key = VerifyingKey::from_bytes(&payload.from_pubkey)
        .map_err(|e| format!("Invalid public key: {:?}", e))?;
    let sig = Signature::from_slice(&payload.sig)
        .map_err(|e| format!("Invalid signature: {:?}", e))?;
    verifying_key.verify(&msg, &sig)
        .map_err(|e| format!("Signature verification failed: {:?}", e))?;

    state.store.add_vibe(&payload.post_id, &payload.vibe_type)
        .await
        .map_err(|e| format!("Database error: {:?}", e))?;
    Ok("Vibe recorded".to_string())
}

async fn get_ping(State(state): State<Arc<ServerState>>) -> Json<PingResponse> {
    let pubkey_hex: String = state.identity.public_key_bytes()
        .iter()
        .map(|b| format!("{:02x}", b))
        .collect();
    
    Json(PingResponse {
        username: state.identity.username.clone(),
        public_key: pubkey_hex,
        uptime_seconds: state.start_time.elapsed().as_secs(),
        udp_port: state.udp_port,
    })
}

pub async fn start_server(state: Arc<ServerState>) -> anyhow::Result<()> {
    // Config rate limiter: burst 15, refill 1/sec
    let governor_config = Arc::new(
        GovernorConfigBuilder::default()
            .per_second(1)
            .burst_size(15)
            .finish()
            .unwrap(),
    );

    // NOTE: Axum applies middleware layers to all routes defined BEFORE the layer in the builder chain.
    // Therefore, the GovernorLayer rate-limiter covers only "/feed" and "/vibe", leaving "/ping" ungated.
    let app = Router::new()
        .route("/feed", get(get_feed))
        .route("/vibe", post(post_vibe))
        .layer(GovernorLayer { config: governor_config })
        .route("/ping", get(get_ping))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:3731").await?;
    tracing::info!("Axum server listening on http://localhost:3731");
    axum::serve(listener, app.into_make_service_with_connect_info::<std::net::SocketAddr>()).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::load_or_create;
    use crate::store::Store;
    use uuid::Uuid;

    #[tokio::test]
    async fn test_server_routes_integration() {
        // 1. Setup ephemeral identity and store
        let identity = load_or_create(true).expect("failed to load ephemeral identity");
        let store = Store::init(true).await.expect("failed to init ephemeral store");

        // 2. Insert a post into the store so we have something in /feed
        let mut test_post = Post {
            id: Uuid::new_v4(),
            author_pubkey: identity.public_key_bytes(),
            author_username: identity.username.clone(),
            content: "Hello from integration test!".to_string(),
            lat_coarse: 27.7172,
            lon_coarse: 85.3240,
            h3_cell: "837001fffffffff".to_string(),
            created_at: chrono::Utc::now().timestamp(),
            expires_at: chrono::Utc::now().timestamp() + 3600,
            sig: [0u8; 64],
        };
        test_post.sign(&identity.signing_key);
        store.insert_post(&test_post).await.expect("failed to insert post");

        // 3. Build ServerState
        let state = Arc::new(ServerState {
            identity: identity.clone(),
            store: store.clone(),
            udp_port: 9999,
            start_time: Instant::now(),
        });

        // 4. Configure app
        let governor_config = Arc::new(
            GovernorConfigBuilder::default()
                .per_second(10) // high limit for testing
                .burst_size(100)
                .finish()
                .unwrap(),
        );
        // NOTE: Axum applies middleware layers to all routes defined BEFORE the layer in the builder chain.
        // Therefore, the GovernorLayer rate-limiter covers only "/feed" and "/vibe", leaving "/ping" ungated.
        let app = Router::new()
            .route("/feed", get(get_feed))
            .route("/vibe", axum::routing::post(post_vibe))
            .layer(GovernorLayer { config: governor_config })
            .route("/ping", get(get_ping))
            .with_state(state);

        // 5. Bind listener to dynamic port (port 0)
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        // 6. Spawn the Axum server
        tokio::spawn(async move {
            axum::serve(listener, app.into_make_service_with_connect_info::<std::net::SocketAddr>()).await.unwrap();
        });

        // Give the server a small moment to bind
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let client = reqwest::Client::new();

        // 7. Test /ping endpoint
        let ping_url = format!("http://127.0.0.1:{}/ping", port);
        let ping_resp = client.get(&ping_url).send().await.unwrap();
        assert!(ping_resp.status().is_success());
        let ping_data: PingResponse = ping_resp.json().await.unwrap();
        assert_eq!(ping_data.username, identity.username);
        assert_eq!(ping_data.udp_port, 9999);

        // 8. Test /feed endpoint
        let feed_url = format!("http://127.0.0.1:{}/feed", port);
        let feed_resp = client.get(&feed_url).send().await.unwrap();
        assert!(feed_resp.status().is_success());
        let feed_posts: Vec<Post> = feed_resp.json().await.unwrap();
        assert_eq!(feed_posts.len(), 1);
        assert_eq!(feed_posts[0].content, "Hello from integration test!");
        assert_eq!(feed_posts[0].id, test_post.id);

        // 9. Test /vibe endpoint
        let vibe_url = format!("http://127.0.0.1:{}/vibe", port);
        let sent_at = chrono::Utc::now().timestamp();
        let msg = crate::store::vibe_signature_bytes(&test_post.id.to_string(), "🔥", sent_at);
        use ed25519_dalek::Signer;
        let sig = identity.signing_key.sign(&msg).to_bytes();
        let vibe_req = VibeRequest {
            post_id: test_post.id.to_string(),
            vibe_type: "🔥".to_string(),
            sent_at,
            from_pubkey: identity.public_key_bytes(),
            sig,
        };
        let vibe_resp = client.post(&vibe_url).json(&vibe_req).send().await.unwrap();
        assert!(vibe_resp.status().is_success());
        let vibe_body = vibe_resp.text().await.unwrap();
        assert_eq!(vibe_body, "Vibe recorded");

        // Verify vibe was stored in database
        let vibes = store.get_vibes_for_post(&test_post.id.to_string()).await.unwrap();
        assert_eq!(vibes.len(), 1);
        assert_eq!(vibes[0], "🔥");
    }

    #[tokio::test]
    async fn test_vibe_security_and_validation() {
        // Setup ephemeral identity and store
        let identity = load_or_create(true).expect("failed to load ephemeral identity");
        let store = Store::init(true).await.expect("failed to init ephemeral store");

        // Insert an active post
        let mut active_post = Post {
            id: Uuid::new_v4(),
            author_pubkey: identity.public_key_bytes(),
            author_username: identity.username.clone(),
            content: "Active post".to_string(),
            lat_coarse: 0.0,
            lon_coarse: 0.0,
            h3_cell: "837001fffffffff".to_string(),
            created_at: chrono::Utc::now().timestamp(),
            expires_at: chrono::Utc::now().timestamp() + 3600,
            sig: [0u8; 64],
        };
        active_post.sign(&identity.signing_key);
        store.insert_post(&active_post).await.expect("failed to insert active post");

        // Insert an expired post
        let mut expired_post = Post {
            id: Uuid::new_v4(),
            author_pubkey: identity.public_key_bytes(),
            author_username: identity.username.clone(),
            content: "Expired post".to_string(),
            lat_coarse: 0.0,
            lon_coarse: 0.0,
            h3_cell: "837001fffffffff".to_string(),
            created_at: chrono::Utc::now().timestamp() - 7200,
            expires_at: chrono::Utc::now().timestamp() - 3600,
            sig: [0u8; 64],
        };
        expired_post.sign(&identity.signing_key);
        store.insert_post(&expired_post).await.expect("failed to insert expired post");

        // Build ServerState
        let state = Arc::new(ServerState {
            identity: identity.clone(),
            store: store.clone(),
            udp_port: 8888,
            start_time: Instant::now(),
        });

        // App
        let app = Router::new()
            .route("/vibe", axum::routing::post(post_vibe))
            .with_state(state);

        // Bind listener
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        tokio::spawn(async move {
            axum::serve(listener, app.into_make_service_with_connect_info::<std::net::SocketAddr>()).await.unwrap();
        });

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let client = reqwest::Client::new();
        let vibe_url = format!("http://127.0.0.1:{}/vibe", port);

        // Case 1: Post not found
        {
            let sent_at = chrono::Utc::now().timestamp();
            let msg = crate::store::vibe_signature_bytes("non-existent-id", "🔥", sent_at);
            use ed25519_dalek::Signer;
            let sig = identity.signing_key.sign(&msg).to_bytes();
            let vibe_req = VibeRequest {
                post_id: "non-existent-id".to_string(),
                vibe_type: "🔥".to_string(),
                sent_at,
                from_pubkey: identity.public_key_bytes(),
                sig,
            };
            let resp = client.post(&vibe_url).json(&vibe_req).send().await.unwrap();
            let status = resp.status();
            let body = resp.text().await.unwrap();
            assert_eq!(status, reqwest::StatusCode::OK);
            assert!(body.contains("Post not found or expired"));
        }

        // Case 2: Post expired
        {
            let sent_at = chrono::Utc::now().timestamp();
            let msg = crate::store::vibe_signature_bytes(&expired_post.id.to_string(), "🔥", sent_at);
            use ed25519_dalek::Signer;
            let sig = identity.signing_key.sign(&msg).to_bytes();
            let vibe_req = VibeRequest {
                post_id: expired_post.id.to_string(),
                vibe_type: "🔥".to_string(),
                sent_at,
                from_pubkey: identity.public_key_bytes(),
                sig,
            };
            let resp = client.post(&vibe_url).json(&vibe_req).send().await.unwrap();
            let status = resp.status();
            let body = resp.text().await.unwrap();
            assert_eq!(status, reqwest::StatusCode::OK);
            assert!(body.contains("Post not found or expired"));
        }

        // Case 3: Invalid vibe type (emoji)
        {
            let sent_at = chrono::Utc::now().timestamp();
            let msg = crate::store::vibe_signature_bytes(&active_post.id.to_string(), "💩", sent_at);
            use ed25519_dalek::Signer;
            let sig = identity.signing_key.sign(&msg).to_bytes();
            let vibe_req = VibeRequest {
                post_id: active_post.id.to_string(),
                vibe_type: "💩".to_string(),
                sent_at,
                from_pubkey: identity.public_key_bytes(),
                sig,
            };
            let resp = client.post(&vibe_url).json(&vibe_req).send().await.unwrap();
            let status = resp.status();
            let body = resp.text().await.unwrap();
            assert_eq!(status, reqwest::StatusCode::OK);
            assert!(body.contains("Invalid vibe type"));
        }

        // Case 4: Timestamp drift too high (6 minutes into future)
        {
            let sent_at = chrono::Utc::now().timestamp() + 360;
            let msg = crate::store::vibe_signature_bytes(&active_post.id.to_string(), "🔥", sent_at);
            use ed25519_dalek::Signer;
            let sig = identity.signing_key.sign(&msg).to_bytes();
            let vibe_req = VibeRequest {
                post_id: active_post.id.to_string(),
                vibe_type: "🔥".to_string(),
                sent_at,
                from_pubkey: identity.public_key_bytes(),
                sig,
            };
            let resp = client.post(&vibe_url).json(&vibe_req).send().await.unwrap();
            let status = resp.status();
            let body = resp.text().await.unwrap();
            assert_eq!(status, reqwest::StatusCode::OK);
            assert!(body.contains("Vibe request timed out"));
        }

        // Case 5: Bad signature
        {
            let sent_at = chrono::Utc::now().timestamp();
            let vibe_req = VibeRequest {
                post_id: active_post.id.to_string(),
                vibe_type: "🔥".to_string(),
                sent_at,
                from_pubkey: identity.public_key_bytes(),
                sig: [0u8; 64],
            };
            let resp = client.post(&vibe_url).json(&vibe_req).send().await.unwrap();
            let status = resp.status();
            let body = resp.text().await.unwrap();
            assert_eq!(status, reqwest::StatusCode::OK);
            assert!(body.contains("Signature verification failed"));
        }
    }
}
