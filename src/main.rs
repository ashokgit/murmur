mod identity;
mod geo;
mod store;
mod dht;
mod tunnel;
mod server;
mod sync;
mod tui;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Write logs to a file in ~/.murmur to prevent permission errors and TUI stdout corruption
    let murmur_dir = std::env::var("HOME")
        .ok()
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var("USERPROFILE").ok().map(std::path::PathBuf::from))
        .map(|h| h.join(".murmur"));

    let log_file = if let Some(ref dir) = murmur_dir {
        std::fs::create_dir_all(dir).ok();
        std::fs::File::create(dir.join("murmur.log")).unwrap_or_else(|_| {
            std::fs::File::create("murmur.log").unwrap()
        })
    } else {
        std::fs::File::create("murmur.log").unwrap()
    };

    tracing_subscriber::fmt()
        .with_writer(log_file)
        .init();

    tracing::info!("murmur social client starting");

    let args: Vec<String> = std::env::args().collect();
    let mut is_ephemeral = false;
    let mut lat_override = None;
    let mut lon_override = None;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--ephemeral" | "-e" => {
                is_ephemeral = true;
            }
            "--lat" => {
                if i + 1 < args.len() {
                    lat_override = args[i + 1].parse::<f64>().ok();
                    i += 1;
                }
            }
            "--lon" => {
                if i + 1 < args.len() {
                    lon_override = args[i + 1].parse::<f64>().ok();
                    i += 1;
                }
            }
            "--help" | "-h" => {
                println!("murmur - Ambient Social Presence client");
                println!("Usage: murmur [OPTIONS]");
                println!("Options:");
                println!("  -e, --ephemeral        Run in ephemeral mode (in-memory keys/DB)");
                println!("  --lat <value>          Latitude override");
                println!("  --lon <value>          Longitude override");
                println!("  -h, --help             Show this help message");
                return Ok(());
            }
            _ => {}
        }
        i += 1;
    }

    // 1. Identity loading
    let identity = identity::load_or_create(is_ephemeral)?;
    tracing::info!("Loaded identity: @{}", identity.username);

    // 2. Database store
    let store = store::Store::init(is_ephemeral).await?;
    tracing::info!("Database initialized");

    // 3. Resolve starting coordinates
    let mut coords = geo::Coords { lat: 27.7172, lon: 85.3240 }; // Default Kathmandu
    if let (Some(lat), Some(lon)) = (lat_override, lon_override) {
        coords = geo::Coords { lat, lon };
        tracing::info!("Coordinates manually set: {:.4}, {:.4}", lat, lon);
    } else if let Some(resolved) = geo::resolve_ip_location().await {
        coords = resolved;
        tracing::info!("Resolved coordinates via IP location: {:.4}, {:.4}", coords.lat, coords.lon);
    } else {
        tracing::info!("Using fallback default coordinates (Kathmandu): {:.4}, {:.4}", coords.lat, coords.lon);
    }

    // 4. Start Cloudflare Tunnel
    let tunnel = Arc::new(tunnel::Tunnel::start()?);
    let url_state = Arc::new(Mutex::new(None));
    tracing::info!("Cloudflare Tunnel process started");

    // 5. Instantiate DHT Node
    let dht_node = Arc::new(dht::DhtNode::new(identity.clone(), store.clone(), url_state.clone()).await?);
    tracing::info!("DHT node bound to UDP port {}", dht_node.udp_port());

    // 6. Setup shared SyncState
    let sync_state = Arc::new(Mutex::new(sync::SyncState {
        coords,
        step: 3, // default step 3 (~1km radius resolution)
        last_user_activity: Instant::now(),
        force_sync: false,
        tunnel_url: None,
    }));

    // 7. Spawn Axum HTTP server
    let server_state = Arc::new(server::ServerState {
        identity: identity.clone(),
        store: store.clone(),
        udp_port: dht_node.udp_port(),
        start_time: Instant::now(),
    });
    tokio::spawn(async move {
        if let Err(e) = server::start_server(server_state).await {
            tracing::error!("Axum HTTP server error: {:?}", e);
        }
    });

    // 8. Spawn Tunnel URL listener task to update shared state
    let tunnel_clone = tunnel.clone();
    let url_state_clone = url_state.clone();
    let sync_state_clone_for_tunnel = sync_state.clone();
    tokio::spawn(async move {
        let mut last_url = None;
        let start_time = Instant::now();
        loop {
            tokio::time::sleep(Duration::from_secs(1)).await;

            let is_alive = tunnel_clone.is_alive();
            let url_opt = tunnel_clone.get_url();

            if is_alive {
                if let Some(url) = url_opt {
                    if Some(url.clone()) != last_url {
                        last_url = Some(url.clone());
                        
                        // Update DHT Node URL state
                        {
                             let mut lock = url_state_clone.lock().unwrap();
                             *lock = Some(url.clone());
                        }

                        // Update shared SyncState
                        {
                             let mut lock = sync_state_clone_for_tunnel.lock().unwrap();
                             lock.tunnel_url = Some(url.clone());
                             lock.force_sync = true;
                        }
                        tracing::info!("Tunnel URL propagated to components: {}", url);
                    }
                } else if start_time.elapsed().as_secs() >= 20 && last_url.is_none() {
                    // Timeout after 20 seconds with no URL
                    tracing::error!("Tunnel timeout: failed to yield a URL within 20 seconds.");
                    let down_str = "❌ down".to_string();
                    last_url = Some(down_str.clone());

                    {
                         let mut lock = url_state_clone.lock().unwrap();
                         *lock = Some(down_str.clone());
                    }

                    {
                         let mut lock = sync_state_clone_for_tunnel.lock().unwrap();
                         lock.tunnel_url = Some(down_str.clone());
                         lock.force_sync = true;
                    }
                }
            } else {
                // Tunnel process is dead
                if last_url.as_deref() != Some("❌ down") {
                    tracing::error!("Tunnel process is not alive. Setting status to down.");
                    let down_str = "❌ down".to_string();
                    last_url = Some(down_str.clone());

                    {
                         let mut lock = url_state_clone.lock().unwrap();
                         *lock = Some(down_str.clone());
                    }

                    {
                         let mut lock = sync_state_clone_for_tunnel.lock().unwrap();
                         lock.tunnel_url = Some(down_str.clone());
                         lock.force_sync = true;
                    }
                }
            }
        }
    });

    // 9. Spawn Sync Runner Loop
    let sync_store = store.clone();
    let sync_dht = dht_node.clone();
    let sync_state_clone_for_loop = sync_state.clone();
    tokio::spawn(async move {
        sync::run_sync_loop(sync_store, sync_dht, sync_state_clone_for_loop).await;
    });

    // 10. Start TUI on the main thread
    let mut app = tui::TuiApp::new(
        store,
        dht_node,
        identity,
        sync_state,
    );
    app.run().await?;

    tracing::info!("murmur social client shutdown complete");
    Ok(())
}
