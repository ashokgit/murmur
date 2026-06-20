use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use reqwest::Client;
use rayon::prelude::*;
use crate::store::{Store, Post};
use crate::dht::DhtNode;
use crate::geo::{Coords, step_to_resolution, get_cells_for_coords};

pub struct SyncState {
    pub coords: Coords,
    pub step: u32,
    pub last_user_activity: Instant,
    pub force_sync: bool,
    pub tunnel_url: Option<String>,
}

fn get_backoff_duration(failed_pings: i32) -> i64 {
    match failed_pings {
        0 => 0,
        1 => 300,        // 5 min
        2 => 600,        // 10 min
        3 => 1200,       // 20 min
        4 => 2400,       // 40 min
        5 => 4800,       // 80 min
        _ => 9000,       // 2.5 hours max
    }
}

pub async fn run_sync_loop(
    store: Store,
    dht: Arc<DhtNode>,
    state: Arc<Mutex<SyncState>>,
) {
    let client = Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .expect("Failed to build HTTP client");

    let mut last_sync = Instant::now() - Duration::from_secs(1000); // trigger sync immediately on startup
    let mut last_tick = Instant::now();
    let mut post_count_history: Vec<usize> = Vec::new();
    let mut last_radius_adjustment = Instant::now() - Duration::from_secs(300); // 5 min cooldown

    loop {
        tokio::time::sleep(Duration::from_secs(1)).await;
        
        let now_tick = Instant::now();
        let tick_elapsed = now_tick.duration_since(last_tick);
        last_tick = now_tick;

        let mut trigger_now = false;

        // 1. Time-Jump Wake Detection
        if tick_elapsed > Duration::from_secs(15) {
            tracing::info!("OS wake detected! Elapsed: {:?}", tick_elapsed);
            trigger_now = true;
        }

        // 2. Read state and check for force_sync or config changes
        let (coords, step, last_activity, force) = {
            let mut s = state.lock().unwrap();
            let force = s.force_sync;
            if force {
                s.force_sync = false;
            }
            (s.coords, s.step, s.last_user_activity, force)
        };

        if force {
            trigger_now = true;
        }

        // Calculate if we need to sync based on active/settled duration
        let is_active = last_activity.elapsed() < Duration::from_secs(15 * 60);
        let base_interval = if is_active {
            // Active: 5 mins (300s) with ±30s jitter
            let jitter = (rand::random::<i64>() % 30) - 15;
            300 + jitter
        } else {
            // Settled: 15 mins (900s) with ±30s jitter
            let jitter = (rand::random::<i64>() % 30) - 15;
            900 + jitter
        };

        let elapsed_since_sync = now_tick.duration_since(last_sync);
        if trigger_now || elapsed_since_sync >= Duration::from_secs(base_interval as u64) {
            last_sync = now_tick;
            tracing::info!("Triggering sync run. Active: {}, Coords: {:?}", is_active, coords);
            
            // Clean expired posts
            if let Err(e) = store.delete_expired_posts().await {
                tracing::error!("Failed to clean expired posts: {:?}", e);
            }

            // DHT Announce and Discovery
            let resolution = step_to_resolution(step);
            let cells = get_cells_for_coords(coords.lat, coords.lon, resolution);
            
            for cell in &cells {
                let cell_str = cell.to_string();
                
                // Announce our presence in the background so it doesn't block the sync loop
                let dht_clone = dht.clone();
                let cell_str_clone = cell_str.clone();
                tokio::spawn(async move {
                    if let Err(e) = dht_clone.announce_h3_cell(&cell_str_clone).await {
                        tracing::error!("DHT announce failed for cell {}: {:?}", cell_str_clone, e);
                    }
                });

                // Discover other peers in the cell
                match dht.discover_peers(&cell_str).await {
                    Ok(discovered_peers) => {
                        for peer in discovered_peers {
                            if let Err(e) = store.upsert_peer(&peer).await {
                                tracing::error!("Failed to upsert discovered peer: {:?}", e);
                            }
                        }
                    }
                    Err(e) => {
                        tracing::error!("DHT peer discovery failed for cell {}: {:?}", cell_str, e);
                    }
                }
            }

            // Query active peers from DB
            match store.get_active_peers().await {
                Ok(peers) => {
                    let now_ts = chrono::Utc::now().timestamp();
                    let mut active_peers = Vec::new();

                    for peer in peers {
                        let backoff = get_backoff_duration(peer.failed_pings);
                        if peer.failed_pings > 0 && peer.last_seen + backoff > now_ts {
                            // Skip peer due to backoff
                            continue;
                        }
                        active_peers.push(peer);
                    }

                    // Poll each active peer
                    for peer in active_peers {
                        let client_clone = client.clone();
                        let store_clone = store.clone();
                        let feed_url = format!("{}/feed", peer.tunnel_url);

                        tokio::spawn(async move {
                            match client_clone.get(&feed_url).send().await {
                                Ok(resp) => {
                                    if resp.status().is_success() {
                                        if let Ok(posts) = resp.json::<Vec<Post>>().await {
                                            let valid_posts: Vec<Post> = tokio::task::spawn_blocking(move || {
                                                posts.into_par_iter()
                                                    .filter_map(|mut p| {
                                                        if p.validate_and_clean() {
                                                            Some(p)
                                                        } else {
                                                            None
                                                        }
                                                    })
                                                    .collect()
                                            }).await.unwrap_or_default();

                                            if !valid_posts.is_empty() {
                                                if let Err(e) = store_clone.insert_posts_batch(&valid_posts).await {
                                                    tracing::error!("Failed to save synced posts: {:?}", e);
                                                }
                                            }
                                            let _ = store_clone.reset_failed_pings(&peer.pubkey).await;
                                        } else {
                                            let _ = store_clone.increment_failed_pings(&peer.pubkey).await;
                                        }
                                    } else {
                                        let _ = store_clone.increment_failed_pings(&peer.pubkey).await;
                                    }
                                }
                                Err(_) => {
                                    let _ = store_clone.increment_failed_pings(&peer.pubkey).await;
                                }
                            }
                        });
                    }
                }
                Err(e) => {
                    tracing::error!("Failed to get active peers: {:?}", e);
                }
            }

            // Count current visible posts to evaluate adaptive radius
            let current_resolution = step_to_resolution(step);
            let current_cells = get_cells_for_coords(coords.lat, coords.lon, current_resolution);
            let current_cell_strs: Vec<String> = current_cells.iter().map(|c| c.to_string()).collect();
            let mut post_count = 0;
            if let Ok(posts) = store.get_posts_for_cells(&current_cell_strs).await {
                post_count = posts.len();
            }

            post_count_history.push(post_count);
            if post_count_history.len() > 3 {
                post_count_history.remove(0);
            }

            if post_count_history.len() == 3 {
                let avg = post_count_history.iter().sum::<usize>() / 3;
                if last_radius_adjustment.elapsed() >= Duration::from_secs(300) {
                    let mut s = state.lock().unwrap();
                    let current_step = s.step;
                    let mut new_step = current_step;

                    if avg < 30 && current_step < 5 {
                        new_step = current_step + 1;
                        tracing::info!("Adaptive Radius: Average post count {} is below 30. Expanding step {} -> {}", avg, current_step, new_step);
                    } else if avg > 80 && current_step > 1 {
                        new_step = current_step - 1;
                        tracing::info!("Adaptive Radius: Average post count {} is above 80. Shrinking step {} -> {}", avg, current_step, new_step);
                    }

                    if new_step != current_step {
                        s.step = new_step;
                        s.force_sync = true;
                        post_count_history.clear();
                        last_radius_adjustment = Instant::now();
                    }
                }
            }
        }
    }
}
