use std::io;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
    Terminal,
};
use uuid::Uuid;

use crate::identity::Identity;
use crate::store::{Store, Peer, Post};
use crate::dht::{DhtNode, derive_node_id};
use crate::geo::{Coords, step_to_resolution};
use crate::sync::SyncState;

pub struct TuiApp {
    store: Store,
    dht: Arc<DhtNode>,
    identity: Identity,
    sync_state: Arc<Mutex<SyncState>>,
    input: String,
    scroll: usize,
    messages: Vec<String>,
    ttl_hours: i64,
    posts_cache: Vec<(Post, Vec<String>)>,
    peers_cache: Vec<Peer>,
    last_db_refresh: Instant,
}

impl TuiApp {
    pub fn new(
        store: Store,
        dht: Arc<DhtNode>,
        identity: Identity,
        sync_state: Arc<Mutex<SyncState>>,
    ) -> Self {
        Self {
            store,
            dht,
            identity,
            sync_state,
            input: String::new(),
            scroll: 0,
            messages: vec!["Welcome to murmur. Type :help for commands.".to_string()],
            ttl_hours: 24,
            posts_cache: Vec::new(),
            peers_cache: Vec::new(),
            last_db_refresh: Instant::now() - Duration::from_secs(10),
        }
    }

    pub async fn run(&mut self) -> anyhow::Result<()> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend)?;

        self.refresh_caches().await;

        let mut last_tick = Instant::now();
        loop {
            // Draw TUI
            terminal.draw(|f| self.draw(f))?;

            // Polling interval
            let timeout = Duration::from_millis(50);
            if event::poll(timeout)? {
                if let Event::Key(key) = event::read()? {
                    {
                        let mut s = self.sync_state.lock().unwrap();
                        s.last_user_activity = Instant::now();
                    }
                    match key.code {
                        KeyCode::Char(c) => {
                            self.input.push(c);
                        }
                        KeyCode::Backspace => {
                            self.input.pop();
                        }
                        KeyCode::Esc => {
                            self.input.clear();
                        }
                        KeyCode::Up => {
                            if self.scroll > 0 {
                                self.scroll -= 1;
                            }
                        }
                        KeyCode::Down => {
                            if !self.posts_cache.is_empty() && self.scroll < self.posts_cache.len() - 1 {
                                self.scroll += 1;
                            }
                        }
                        KeyCode::Enter => {
                            let cmd = self.input.clone();
                            self.input.clear();
                            if !cmd.trim().is_empty() {
                                if cmd.starts_with(':') {
                                    self.handle_command(&cmd).await;
                                } else {
                                    // Implicit post creation if not starting with ':'
                                    self.handle_command(&format!(":post {}", cmd)).await;
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }

            // Periodically refresh data from DB
            if last_tick.elapsed() >= Duration::from_millis(500) {
                self.refresh_caches().await;
                last_tick = Instant::now();
            }

            // Check if we need to exit
            if self.messages.iter().any(|m| m.contains("Exiting murmur...")) {
                break;
            }
        }

        // Clean up terminal
        disable_raw_mode()?;
        execute!(
            terminal.backend_mut(),
            LeaveAlternateScreen,
            DisableMouseCapture
        )?;
        terminal.show_cursor()?;

        Ok(())
    }

    async fn refresh_caches(&mut self) {
        // Query active resolution & cells
        let (coords, step) = {
            let s = self.sync_state.lock().unwrap();
            (s.coords, s.step)
        };
        let res = step_to_resolution(step);
        let cells = crate::geo::get_cells_for_coords(coords.lat, coords.lon, res);
        let cell_strs: Vec<String> = cells.iter().map(|c| c.to_string()).collect();

        // Fetch posts for nearby cells
        if let Ok(posts) = self.store.get_posts_for_cells(&cell_strs).await {
            let mut resolved = Vec::new();
            for post in posts {
                let vibes = self.store.get_vibes_for_post(&post.id.to_string()).await.unwrap_or_default();
                resolved.push((post, vibes));
            }
            self.posts_cache = resolved;
        }

        // Fetch active peers
        if let Ok(peers) = self.store.get_active_peers().await {
            self.peers_cache = peers;
        }
    }

    fn add_message(&mut self, msg: String) {
        self.messages.push(msg);
        if self.messages.len() > 100 {
            self.messages.remove(0);
        }
    }

    async fn handle_command(&mut self, cmd: &str) {
        let parts: Vec<&str> = cmd.split_whitespace().collect();
        if parts.is_empty() {
            return;
        }

        let command = parts[0];
        match command {
            ":help" => {
                self.add_message("Available commands:".to_string());
                self.add_message("  :post <text>           - Broadcast an ambient thought".to_string());
                self.add_message("  :vibe <id> <emoji>     - Send vibe emoji to a post author".to_string());
                self.add_message("  :radius [<step 1-5>]   - Toggle or set grid search radius".to_string());
                self.add_message("  :peer add <url>        - Manually add a peer by URL".to_string());
                self.add_message("  :ttl <hours>           - Set TTL for your new posts".to_string());
                self.add_message("  :location set <lat>,<lon> - Manually override location".to_string());
                self.add_message("  :dht status            - Show DHT diagnostic information".to_string());
                self.add_message("  :exit or :quit         - Quit murmur".to_string());
            }
            ":exit" | ":quit" | ":q" => {
                self.add_message("Exiting murmur...".to_string());
            }
            ":post" => {
                if parts.len() < 2 {
                    self.add_message("Usage: :post <text>".to_string());
                    return;
                }
                let text = parts[1..].join(" ");
                
                let (coords, step) = {
                    let s = self.sync_state.lock().unwrap();
                    (s.coords, s.step)
                };
                let res = step_to_resolution(step);
                let h3_cell = if let Ok(latlng) = h3o::LatLng::new(coords.lat, coords.lon) {
                    latlng.to_cell(res).to_string()
                } else {
                    self.add_message("Error: Invalid coordinates".to_string());
                    return;
                };

                let (lat_coarse, lon_coarse) = crate::geo::coarsen(coords.lat, coords.lon);
                let now = chrono::Utc::now().timestamp();
                let mut post = Post {
                    id: Uuid::new_v4(),
                    author_pubkey: self.identity.public_key_bytes(),
                    author_username: self.identity.username.clone(),
                    content: text,
                    lat_coarse,
                    lon_coarse,
                    h3_cell,
                    created_at: now,
                    expires_at: now + self.ttl_hours * 3600,
                    sig: [0u8; 64],
                };

                // Validate and clean
                post.sign(&self.identity.signing_key);
                if post.validate_and_clean() {
                    match self.store.insert_post(&post).await {
                        Ok(_) => {
                            self.add_message("Post created successfully!".to_string());
                            // Force background announce & sync run
                            {
                                let mut s = self.sync_state.lock().unwrap();
                                s.force_sync = true;
                            }
                            self.refresh_caches().await;
                        }
                        Err(e) => {
                            self.add_message(format!("Database error: {:?}", e));
                        }
                    }
                } else {
                    self.add_message("Error: Post content invalid or too long (>280 chars)".to_string());
                }
            }
            ":vibe" => {
                if parts.len() < 3 {
                    self.add_message("Usage: :vibe <post_id_prefix> <emoji>".to_string());
                    return;
                }
                let prefix = parts[1].to_lowercase();
                let emoji = parts[2];

                let allowed_vibes = ["✨", "🌊", "🔥", "💀", "🌿", "👁️", "👁", "🫧"];
                if !allowed_vibes.contains(&emoji) {
                    self.add_message("Error: Vibe must be one of: ✨, 🌊, 🔥, 💀, 🌿, 👁️, 🫧".to_string());
                    return;
                }

                // Match prefix to cached posts with collision checking
                let matches: Vec<Post> = self.posts_cache.iter()
                    .map(|(p, _)| p)
                    .filter(|p| p.id.to_string().to_lowercase().starts_with(&prefix))
                    .cloned()
                    .collect();

                if matches.is_empty() {
                    self.add_message(format!("Error: No post found matching prefix '{}'", prefix));
                    return;
                }
                if matches.len() > 1 {
                    self.add_message(format!("Error: Ambiguous prefix '{}'. {} posts matched.", prefix, matches.len()));
                    return;
                }
                let post = matches[0].clone();

                // Record locally
                if let Err(e) = self.store.add_vibe(&post.id.to_string(), emoji).await {
                    self.add_message(format!("Failed to save vibe locally: {:?}", e));
                    return;
                }
                self.add_message(format!("Vibe '{}' saved locally for post {}", emoji, post.id));

                // Send to remote peer if not ourselves
                if post.author_pubkey != self.identity.public_key_bytes() {
                    let peers = self.peers_cache.clone();
                    let target_peer = peers.into_iter().find(|p| p.pubkey == post.author_pubkey);
                    if let Some(peer) = target_peer {
                        let client = reqwest::Client::builder()
                            .timeout(Duration::from_secs(3))
                            .build()
                            .unwrap();
                        let target_url = format!("{}/vibe", peer.tunnel_url);
                        
                        let sent_at = chrono::Utc::now().timestamp();
                        let msg = crate::store::vibe_signature_bytes(&post.id.to_string(), emoji, sent_at);
                        use ed25519_dalek::Signer;
                        let sig = self.identity.signing_key.sign(&msg).to_bytes();

                        #[derive(serde::Serialize)]
                        struct VibeReq {
                            post_id: String,
                            vibe_type: String,
                            sent_at: i64,
                            #[serde(with = "crate::store::serde_pubkey")]
                            from_pubkey: [u8; 32],
                            #[serde(with = "crate::store::serde_sig")]
                            sig: [u8; 64],
                        }
                        
                        let req_body = VibeReq {
                            post_id: post.id.to_string(),
                            vibe_type: emoji.to_string(),
                            sent_at,
                            from_pubkey: self.identity.public_key_bytes(),
                            sig,
                        };

                        let store_clone = self.store.clone();
                        let pk = peer.pubkey;

                        tokio::spawn(async move {
                            match client.post(&target_url).json(&req_body).send().await {
                                Ok(resp) => {
                                    if resp.status().is_success() {
                                        let _ = store_clone.reset_failed_pings(&pk).await;
                                    } else {
                                        let _ = store_clone.increment_failed_pings(&pk).await;
                                    }
                                }
                                Err(_) => {
                                    let _ = store_clone.increment_failed_pings(&pk).await;
                                }
                            }
                        });
                    } else {
                        self.add_message("Peer not currently active or address unknown. Vibe stored locally only.".to_string());
                    }
                }
                self.refresh_caches().await;
            }
            ":radius" => {
                let mut s = self.sync_state.lock().unwrap();
                if parts.len() >= 2 {
                    if let Ok(step) = parts[1].parse::<u32>() {
                        if (1..=5).contains(&step) {
                            s.step = step;
                            s.force_sync = true;
                            drop(s);
                            self.add_message(format!("Radius step set to: {} (~{}m search area)", step, match step {
                                1 => 100,
                                2 => 300,
                                3 => 1000,
                                4 => 3000,
                                _ => 10000,
                            }));
                        } else {
                            drop(s);
                            self.add_message("Error: Radius step must be 1 to 5".to_string());
                        }
                    } else {
                        drop(s);
                        self.add_message("Usage: :radius [<step 1-5>]".to_string());
                    }
                } else {
                    s.step = (s.step % 5) + 1;
                    s.force_sync = true;
                    let step = s.step;
                    drop(s);
                    self.add_message(format!("Toggled radius step to: {} (~{}m search area)", step, match step {
                        1 => 100,
                        2 => 300,
                        3 => 1000,
                        4 => 3000,
                        _ => 10000,
                    }));
                }
            }
            ":peer" => {
                if parts.len() >= 3 && parts[1] == "add" {
                    let mut url = parts[2].to_string();
                    if !url.starts_with("http://") && !url.starts_with("https://") {
                        url = format!("http://{}", url);
                    }
                    
                    self.add_message(format!("Pinging peer at {}...", url));
                    let client = reqwest::Client::builder()
                        .timeout(Duration::from_secs(5))
                        .build()
                        .unwrap();
                    let store_clone = self.store.clone();
                    let sync_clone = self.sync_state.clone();
                    
                    #[derive(serde::Deserialize)]
                    struct PingResp {
                        username: String,
                        public_key: String,
                    }

                    tokio::spawn(async move {
                        let ping_url = format!("{}/ping", url);
                        match client.get(&ping_url).send().await {
                            Ok(resp) => {
                                if let Ok(data) = resp.json::<PingResp>().await {
                                    if let Ok(pubkey_vec) = hex::decode(&data.public_key) {
                                        if let Ok(pubkey) = pubkey_vec.try_into() {
                                            let peer = Peer {
                                                pubkey,
                                                tunnel_url: url.clone(),
                                                last_seen: chrono::Utc::now().timestamp(),
                                                failed_pings: 0,
                                                is_manual: 1,
                                            };
                                            if store_clone.upsert_peer(&peer).await.is_ok() {
                                                let mut s = sync_clone.lock().unwrap();
                                                s.force_sync = true;
                                            }
                                        }
                                    }
                                }
                            }
                            Err(_) => {}
                        }
                    });
                } else {
                    self.add_message("Usage: :peer add <url>".to_string());
                }
            }
            ":ttl" => {
                if parts.len() < 2 {
                    self.add_message(format!("Current post TTL: {} hours", self.ttl_hours));
                    return;
                }
                if let Ok(hours) = parts[1].parse::<i64>() {
                    if hours > 0 {
                        self.ttl_hours = hours;
                        self.add_message(format!("Set post TTL to: {} hours", hours));
                    } else {
                        self.add_message("Error: TTL hours must be > 0".to_string());
                    }
                } else {
                    self.add_message("Usage: :ttl <hours>".to_string());
                }
            }
            ":location" => {
                if parts.len() >= 3 && parts[1] == "set" {
                    let raw_coords = parts[2];
                    let subparts: Vec<&str> = if raw_coords.contains(',') {
                        raw_coords.split(',').collect()
                    } else {
                        parts[2..].to_vec()
                    };

                    if subparts.len() >= 2 {
                        let lat_opt = subparts[0].parse::<f64>();
                        let lon_opt = subparts[1].parse::<f64>();
                        if let (Ok(lat), Ok(lon)) = (lat_opt, lon_opt) {
                            let mut s = self.sync_state.lock().unwrap();
                            s.coords = Coords { lat, lon };
                            s.force_sync = true;
                            drop(s);
                            self.add_message(format!("Coordinates manually overridden to: {:.4}, {:.4}", lat, lon));
                        } else {
                            self.add_message("Error: Failed to parse coordinate decimals".to_string());
                        }
                    } else {
                        self.add_message("Usage: :location set <lat>,<lon>".to_string());
                    }
                } else {
                    let coords_val = {
                        let s = self.sync_state.lock().unwrap();
                        s.coords
                    };
                    self.add_message(format!("Current location: {:.4}, {:.4}", coords_val.lat, coords_val.lon));
                }
            }
            ":dht" => {
                let our_pubkey = self.identity.public_key_bytes();
                let our_node_id = derive_node_id(&our_pubkey);
                let node_id_hex: String = our_node_id.iter().map(|b| format!("{:02x}", b)).collect();
                self.add_message(format!("DHT UDP Bind Port: {}", self.dht.udp_port()));
                self.add_message(format!("Kademlia Node ID (SHA3 derived): {}", node_id_hex));
                self.add_message(format!("Active network peers count: {}", self.peers_cache.len()));
            }
            _ => {
                self.add_message(format!("Unknown command: '{}'. Type :help for info.", command));
            }
        }
    }

    fn draw(&self, f: &mut ratatui::Frame) {
        let size = f.size();

        // 3-way layout: Main panels (left / right), bottom CLI input
        let main_layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(5),
                Constraint::Length(3),
            ])
            .split(size);

        let content_layout = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage(70),
                Constraint::Percentage(30),
            ])
            .split(main_layout[0]);

        // Left Pane: Ambient Social Feed
        let feed_block = Block::default()
            .borders(Borders::ALL)
            .title(Span::styled(" 🌌 AMBIENT SOCIAL FEED (↑/↓ to Scroll) ", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)))
            .border_style(Style::default().fg(Color::DarkGray));

        // Format posts list
        let mut feed_text = Vec::new();
        if self.posts_cache.is_empty() {
            feed_text.push(Line::from(vec![
                Span::styled("No ambient signals detected in this area.", Style::default().fg(Color::DarkGray).add_modifier(Modifier::ITALIC))
            ]));
            feed_text.push(Line::from(vec![
                Span::styled("Share your own vibe using the input bar below!", Style::default().fg(Color::DarkGray))
            ]));
        } else {
            // Render cached posts starting from scroll offset
            for (idx, (post, vibes)) in self.posts_cache.iter().enumerate() {
                if idx < self.scroll {
                    continue;
                }
                
                let time_str = chrono::DateTime::<chrono::Utc>::from_timestamp(post.created_at, 0)
                    .map(|dt| dt.format("%H:%M:%S").to_string())
                    .unwrap_or_else(|| "unknown".to_string());
                
                let is_our_post = post.author_pubkey == self.identity.public_key_bytes();
                let username_color = if is_our_post { Color::Magenta } else { Color::Green };
                
                // Header
                let mut header = vec![
                    Span::styled(format!("● @{}", post.author_username), Style::default().fg(username_color).add_modifier(Modifier::BOLD)),
                    Span::styled(format!(" [{}]", &post.id.to_string()[..8]), Style::default().fg(Color::DarkGray)),
                    Span::styled(format!(" @ H3:{}", post.h3_cell), Style::default().fg(Color::Yellow)),
                    Span::styled(format!(" ({} UTC)", time_str), Style::default().fg(Color::DarkGray)),
                ];
                
                if is_our_post {
                    header.push(Span::styled(" (You)", Style::default().fg(Color::Magenta).add_modifier(Modifier::ITALIC)));
                }
                
                feed_text.push(Line::from(header));
                
                // Content
                feed_text.push(Line::from(vec![
                    Span::raw(format!("  {}", post.content))
                ]));
                
                // Vibes
                if !vibes.is_empty() {
                    let mut vibe_spans = vec![Span::styled("  Vibes: ", Style::default().fg(Color::DarkGray))];
                    for v in vibes {
                        vibe_spans.push(Span::styled(format!("{} ", v), Style::default().fg(Color::LightYellow)));
                    }
                    feed_text.push(Line::from(vibe_spans));
                }
                
                // Separator
                feed_text.push(Line::from(vec![
                    Span::styled("  ───────────────────────────────────", Style::default().fg(Color::Rgb(30, 30, 30)))
                ]));
            }
        }

        let feed_paragraph = Paragraph::new(feed_text)
            .block(feed_block)
            .wrap(Wrap { trim: true });
        f.render_widget(feed_paragraph, content_layout[0]);

        // Right Pane: Network & Status
        let right_layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Percentage(50),
                Constraint::Percentage(50),
            ])
            .split(content_layout[1]);

        // Top-Right Panel: System Status
        let status_block = Block::default()
            .borders(Borders::ALL)
            .title(Span::styled(" STATUS ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)))
            .border_style(Style::default().fg(Color::DarkGray));

        let (coords, step) = {
            let s = self.sync_state.lock().unwrap();
            (s.coords, s.step)
        };
        let h3_res = step_to_resolution(step);
        let center_cell = h3o::LatLng::new(coords.lat, coords.lon)
            .ok()
            .map(|ll| ll.to_cell(h3_res).to_string())
            .unwrap_or_else(|| "Invalid".to_string());

        let status_lines = vec![
            Line::from(vec![
                Span::styled("Identity: ", Style::default().fg(Color::DarkGray)),
                Span::styled(format!("@{}", self.identity.username), Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD)),
            ]),
            Line::from(vec![
                Span::styled("Public Key: ", Style::default().fg(Color::DarkGray)),
                Span::styled(format!("{}...", &hex::encode(self.identity.public_key_bytes())[..12]), Style::default().fg(Color::LightBlue)),
            ]),
            Line::from(vec![
                Span::styled("Location: ", Style::default().fg(Color::DarkGray)),
                Span::styled(format!("{:.4}, {:.4}", coords.lat, coords.lon), Style::default().fg(Color::White)),
            ]),
            Line::from(vec![
                Span::styled("H3 Cell: ", Style::default().fg(Color::DarkGray)),
                Span::styled(center_cell, Style::default().fg(Color::Yellow)),
            ]),
            Line::from(vec![
                Span::styled("Radius Step: ", Style::default().fg(Color::DarkGray)),
                Span::styled(format!("{} (Res {})", step, h3_res), Style::default().fg(Color::Cyan)),
            ]),
            Line::from(vec![
                Span::styled("DHT UDP Port: ", Style::default().fg(Color::DarkGray)),
                Span::styled(self.dht.udp_port().to_string(), Style::default().fg(Color::LightGreen)),
            ]),
            Line::from(vec![
                Span::styled("Tunnel URL: ", Style::default().fg(Color::DarkGray)),
                Span::styled(self.dht_node_tunnel_url(), Style::default().fg(Color::White).add_modifier(Modifier::UNDERLINED)),
            ]),
        ];

        let status_paragraph = Paragraph::new(status_lines).block(status_block);
        f.render_widget(status_paragraph, right_layout[0]);

        // Bottom-Right Panel: Peers / Signals List
        let peers_block = Block::default()
            .borders(Borders::ALL)
            .title(Span::styled(format!(" NEIGHBORS ({}) ", self.peers_cache.len()), Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)))
            .border_style(Style::default().fg(Color::DarkGray));

        let mut peer_lines = Vec::new();
        if self.peers_cache.is_empty() {
            peer_lines.push(Line::from(vec![
                Span::styled("Searching DHT...", Style::default().fg(Color::DarkGray).add_modifier(Modifier::ITALIC))
            ]));
        } else {
            for peer in &self.peers_cache {
                let short_pk = &hex::encode(peer.pubkey)[..8];
                let color = if peer.failed_pings > 0 { Color::Red } else { Color::Green };
                let last_seen_dt = chrono::DateTime::<chrono::Utc>::from_timestamp(peer.last_seen, 0)
                    .map(|dt| dt.format("%H:%M").to_string())
                    .unwrap_or_else(|| "?".to_string());
                
                peer_lines.push(Line::from(vec![
                    Span::styled("● ", Style::default().fg(color)),
                    Span::styled(format!("pk:{} ", short_pk), Style::default().fg(Color::White)),
                    Span::styled(format!("@{} ", last_seen_dt), Style::default().fg(Color::DarkGray)),
                    Span::styled(format!("(fail: {})", peer.failed_pings), Style::default().fg(Color::DarkGray)),
                ]));
                peer_lines.push(Line::from(vec![
                    Span::styled(format!("  {}", peer.tunnel_url), Style::default().fg(Color::Cyan).add_modifier(Modifier::DIM))
                ]));
            }
        }

        let peers_paragraph = Paragraph::new(peer_lines)
            .block(peers_block)
            .wrap(Wrap { trim: true });
        f.render_widget(peers_paragraph, right_layout[1]);

        // Bottom Bar: Terminal Command Router Input
        let input_block = Block::default()
            .borders(Borders::ALL)
            .title(Span::styled(" ENTER COMMAND OR POST ", Style::default().fg(Color::White).add_modifier(Modifier::BOLD)))
            .border_style(Style::default().fg(Color::Cyan));

        let current_input = self.input.clone();
        
        // Show command bar and some diagnostic message history in the last line of input_block
        let last_message = self.messages.last().cloned().unwrap_or_default();
        let display_content = vec![
            Line::from(vec![
                Span::styled("> ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
                Span::styled(current_input, Style::default()),
            ]),
            Line::from(vec![
                Span::styled("System log: ", Style::default().fg(Color::DarkGray)),
                Span::styled(last_message, Style::default().fg(Color::Gray)),
            ]),
        ];

        let input_paragraph = Paragraph::new(display_content).block(input_block);
        f.render_widget(input_paragraph, main_layout[1]);
    }

    fn dht_node_tunnel_url(&self) -> String {
        let s = self.sync_state.lock().unwrap();
        s.tunnel_url.clone().unwrap_or_else(|| "generating...".to_string())
    }
}
