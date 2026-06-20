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

const COLOR_BG: Color = Color::Rgb(0, 0, 0);
const COLOR_PRIMARY: Color = Color::Rgb(255, 255, 255);
const COLOR_MUTED: Color = Color::Rgb(120, 120, 120);
const COLOR_ACCENT_YOU: Color = Color::Rgb(255, 255, 255);
const COLOR_ACCENT_OTHERS: Color = Color::Rgb(200, 200, 200);
const COLOR_ACCENT_SYSTEM: Color = Color::Rgb(160, 160, 160);
const COLOR_BORDER_IDLE: Color = Color::Rgb(48, 48, 48);
const COLOR_BORDER_ACTIVE: Color = Color::Rgb(255, 255, 255);
const COLOR_ERROR: Color = Color::Rgb(255, 255, 255);
const COLOR_VIBE_GLOW: Color = Color::Rgb(255, 255, 255);

use crate::store::VIBES;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppMode {
    Typing,
    Navigating,
    AboutOverlay,
    HelpOverlay,
    DebugOverlay,
    VibePicker,
    WelcomeOverlay,
}

pub fn haversine_distance(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let r = 6371000.0; // Earth radius in meters
    let phi1 = lat1.to_radians();
    let phi2 = lat2.to_radians();
    let delta_phi = (lat2 - lat1).to_radians();
    let delta_lambda = (lon2 - lon1).to_radians();

    let a = (delta_phi / 2.0).sin().powi(2)
        + phi1.cos() * phi2.cos() * (delta_lambda / 2.0).sin().powi(2);
    let c = 2.0 * a.sqrt().atan2((1.0 - a).sqrt());

    r * c
}

pub fn relative_time(timestamp: i64) -> String {
    let now = chrono::Utc::now().timestamp();
    let diff = now - timestamp;
    if diff < 60 {
        return "just now".to_string();
    }
    let mins = diff / 60;
    if mins < 60 {
        return format!("{}m ago", mins);
    }
    let hours = mins / 60;
    if hours < 24 {
        return format!("{}h ago", hours);
    }
    let days = hours / 24;
    if days == 1 {
        return "yesterday".to_string();
    }
    format!("{}d ago", days)
}

pub fn fuzzy_distance(meters: f64) -> String {
    if meters < 500.0 {
        "nearby".to_string()
    } else if meters < 1500.0 {
        "~1km".to_string()
    } else if meters < 3500.0 {
        "~3km".to_string()
    } else {
        "far".to_string()
    }
}

fn load_seen_welcome() -> bool {
    let home = std::env::var("HOME").ok().or_else(|| std::env::var("USERPROFILE").ok());
    if let Some(h) = home {
        let config_path = std::path::PathBuf::from(h).join(".murmur").join("config.toml");
        if let Ok(content) = std::fs::read_to_string(config_path) {
            return content.contains("seen_welcome = true");
        }
    }
    false
}

fn save_seen_welcome() {
    let home = std::env::var("HOME").ok().or_else(|| std::env::var("USERPROFILE").ok());
    if let Some(h) = home {
        let murmur_dir = std::path::PathBuf::from(h).join(".murmur");
        let config_path = murmur_dir.join("config.toml");
        let _ = std::fs::create_dir_all(&murmur_dir);
        let _ = std::fs::write(config_path, "[onboarding]\nseen_welcome = true\n");
    }
}

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
    peers_cache: Vec<(Peer, Option<f64>)>,
    mode: AppMode,
    focused_post_idx: usize,
    app_start_time: Instant,
    vibe_picker_focus: usize,
    dismissed_nudge: bool,
}

impl TuiApp {
    pub fn new(
        store: Store,
        dht: Arc<DhtNode>,
        identity: Identity,
        sync_state: Arc<Mutex<SyncState>>,
        is_new_identity: bool,
    ) -> Self {
        let seen_welcome = load_seen_welcome();
        let mode = if is_new_identity && !seen_welcome {
            AppMode::WelcomeOverlay
        } else {
            AppMode::Navigating
        };

        Self {
            store,
            dht,
            identity,
            sync_state,
            input: String::new(),
            scroll: 0,
            messages: vec!["welcome to murmur · try : help".to_string()],
            ttl_hours: 24,
            posts_cache: Vec::new(),
            peers_cache: Vec::new(),
            mode,
            focused_post_idx: 0,
            app_start_time: Instant::now(),
            vibe_picker_focus: 0,
            dismissed_nudge: false,
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

                    match self.mode {
                        AppMode::WelcomeOverlay => {
                            // Any key press dismisses onboarding overlay, returning to Navigating mode
                            save_seen_welcome();
                            self.mode = AppMode::Navigating;
                        }
                        AppMode::AboutOverlay | AppMode::HelpOverlay | AppMode::DebugOverlay => {
                            match key.code {
                                KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('Q') => {
                                    self.mode = AppMode::Navigating;
                                }
                                _ => {}
                            }
                        }
                        AppMode::VibePicker => {
                            match key.code {
                                KeyCode::Esc => {
                                    self.mode = AppMode::Navigating;
                                }
                                KeyCode::Left | KeyCode::Char('h') => {
                                    if self.vibe_picker_focus == 0 {
                                        self.vibe_picker_focus = 6;
                                    } else {
                                        self.vibe_picker_focus -= 1;
                                    }
                                }
                                KeyCode::Right | KeyCode::Char('l') => {
                                    self.vibe_picker_focus = (self.vibe_picker_focus + 1) % 7;
                                }
                                KeyCode::Enter => {
                                    self.send_vibe_picker_index(self.vibe_picker_focus).await;
                                    self.mode = AppMode::Navigating;
                                }
                                KeyCode::Char(c) if c.is_digit(10) => {
                                    if let Some(digit) = c.to_digit(10) {
                                        if (1..=7).contains(&digit) {
                                            self.send_vibe_picker_index(digit as usize - 1).await;
                                            self.mode = AppMode::Navigating;
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                        AppMode::Typing => {
                            match key.code {
                                KeyCode::Char(c) => {
                                    self.input.push(c);
                                }
                                KeyCode::Backspace => {
                                    self.input.pop();
                                }
                                KeyCode::Esc => {
                                    self.input.clear();
                                    self.mode = AppMode::Navigating;
                                }
                                KeyCode::Up => {
                                    if !self.posts_cache.is_empty() && self.focused_post_idx > 0 {
                                        self.focused_post_idx -= 1;
                                        if self.focused_post_idx < self.scroll {
                                            self.scroll = self.focused_post_idx;
                                        }
                                    }
                                }
                                KeyCode::Down => {
                                    if !self.posts_cache.is_empty() && self.focused_post_idx < self.posts_cache.len() - 1 {
                                        self.focused_post_idx += 1;
                                        if self.focused_post_idx >= self.scroll + 4 {
                                            self.scroll = self.focused_post_idx - 3;
                                        }
                                    }
                                }
                                KeyCode::Enter => {
                                    let cmd = self.input.clone();
                                    self.input.clear();
                                    if !cmd.trim().is_empty() {
                                        if cmd.starts_with(':') {
                                            self.handle_command(&cmd).await;
                                        } else if cmd.starts_with('/') {
                                            self.add_message("commands start with : not / — try : help".to_string());
                                        } else {
                                            self.handle_command(&format!(":post {}", cmd)).await;
                                        }
                                    }
                                    if self.mode == AppMode::Typing {
                                        self.mode = AppMode::Navigating;
                                    }
                                }
                                _ => {}
                            }
                        }
                        AppMode::Navigating => {
                            match key.code {
                                KeyCode::Char('i') | KeyCode::Char('I') | KeyCode::Enter => {
                                    self.mode = AppMode::Typing;
                                }
                                KeyCode::Char(':') => {
                                    self.mode = AppMode::Typing;
                                    self.input.push(':');
                                }
                                KeyCode::Up | KeyCode::Char('k') | KeyCode::Char('K') => {
                                    if !self.posts_cache.is_empty() && self.focused_post_idx > 0 {
                                        self.focused_post_idx -= 1;
                                        if self.focused_post_idx < self.scroll {
                                            self.scroll = self.focused_post_idx;
                                        }
                                    }
                                }
                                KeyCode::Down | KeyCode::Char('j') | KeyCode::Char('J') => {
                                    if !self.posts_cache.is_empty() && self.focused_post_idx < self.posts_cache.len() - 1 {
                                        self.focused_post_idx += 1;
                                        if self.focused_post_idx >= self.scroll + 4 {
                                            self.scroll = self.focused_post_idx - 3;
                                        }
                                    }
                                }
                                KeyCode::Char('v') | KeyCode::Char('V') => {
                                    if !self.posts_cache.is_empty() {
                                        self.mode = AppMode::VibePicker;
                                        self.vibe_picker_focus = 0;
                                    } else {
                                        self.add_message("nothing to react to yet".to_string());
                                    }
                                }
                                KeyCode::Esc => {
                                    self.dismissed_nudge = true;
                                }
                                _ => {}
                            }
                        }
                    }
                }
            }

            // Periodically refresh data from DB
            if last_tick.elapsed() >= Duration::from_millis(500) {
                self.refresh_caches().await;
                last_tick = Instant::now();
            }

            // Check if we need to exit
            if self.messages.iter().any(|m| m.contains("fading out...")) {
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
            let mut resolved_peers = Vec::new();
            for peer in peers {
                let dist = if let Ok(Some((lat, lon))) = self.store.get_latest_post_location(&peer.pubkey).await {
                    Some(haversine_distance(coords.lat, coords.lon, lat, lon))
                } else {
                    None
                };
                resolved_peers.push((peer, dist));
            }
            self.peers_cache = resolved_peers;
        }
    }

    async fn send_vibe_for_post(&mut self, post: Post, emoji: &str) {
        // Record locally
        if let Err(e) = self.store.add_vibe(&post.id.to_string(), emoji).await {
            self.add_message("something went wrong on my end — try again in a moment".to_string());
            tracing::error!("Failed to save vibe locally: {:?}", e);
            return;
        }
        self.add_message(format!("you sent a {}", emoji));

        // Send to remote peer if not ourselves
        if post.author_pubkey != self.identity.public_key_bytes() {
            let peers = self.peers_cache.clone();
            let target_peer = peers.into_iter().map(|(p, _)| p).find(|p| p.pubkey == post.author_pubkey);
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
                self.add_message("they're not reachable right now — your vibe is saved for when they are".to_string());
            }
        }
        self.refresh_caches().await;
    }

    async fn send_vibe_picker_index(&mut self, index: usize) {
        if index >= VIBES.len() {
            return;
        }
        let emoji = VIBES[index];
        if !self.posts_cache.is_empty() && self.focused_post_idx < self.posts_cache.len() {
            let post = self.posts_cache[self.focused_post_idx].0.clone();
            self.send_vibe_for_post(post, emoji).await;
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
            ":about" => {
                self.mode = AppMode::AboutOverlay;
            }
            ":help" => {
                self.mode = AppMode::HelpOverlay;
            }
            ":debug" => {
                self.mode = AppMode::DebugOverlay;
            }
            ":exit" | ":quit" | ":q" => {
                self.add_message("fading out...".to_string());
            }
            ":post" => {
                if parts.len() < 2 {
                    self.add_message("try: : post your thought here".to_string());
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
                    self.add_message("something went wrong on my end — try again in a moment".to_string());
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
                            self.add_message("sent into the quiet.".to_string());
                            // Force background announce & sync run
                            {
                                let mut s = self.sync_state.lock().unwrap();
                                s.force_sync = true;
                            }
                            self.refresh_caches().await;
                        }
                        Err(e) => {
                            tracing::error!("Database error: {:?}", e);
                            self.add_message("something went wrong on my end — try again in a moment".to_string());
                        }
                    }
                } else {
                    self.add_message("that's a bit long — keep it under 280 characters".to_string());
                }
            }
            ":vibe" | ":v" => {
                if parts.len() < 3 {
                    self.add_message("to react: press Esc to exit input mode, select a post, and press v (or type :vibe <id> <emoji>)".to_string());
                    return;
                }
                let prefix = parts[1].to_lowercase();
                let emoji = parts[2];

                if !VIBES.contains(&emoji) && emoji != "👁" {
                    self.add_message("pick one of these: ✨ 🌊 🔥 💀 🌿 👁️ 🫧".to_string());
                    return;
                }

                // Match prefix to cached posts with collision checking
                let matches: Vec<Post> = self.posts_cache.iter()
                    .map(|(p, _)| p)
                    .filter(|p| p.id.to_string().to_lowercase().starts_with(&prefix))
                    .cloned()
                    .collect();

                if matches.is_empty() {
                    self.add_message("couldn't find that post — it may have faded already".to_string());
                    return;
                }
                if matches.len() > 1 {
                    self.add_message("that matches a few posts — try a longer id".to_string());
                    return;
                }
                let post = matches[0].clone();

                self.send_vibe_for_post(post, emoji).await;
            }
            ":radius" => {
                let mut s = self.sync_state.lock().unwrap();
                if parts.len() >= 2 {
                    if let Ok(step) = parts[1].parse::<u32>() {
                        if (1..=5).contains(&step) {
                            s.step = step;
                            s.force_sync = true;
                            drop(s);
                            self.add_message(format!("listening radius set to ~{}m", match step {
                                1 => 100,
                                2 => 300,
                                3 => 1000,
                                4 => 3000,
                                _ => 10000,
                            }));
                        } else {
                            drop(s);
                            self.add_message("radius step must be 1 to 5".to_string());
                        }
                    } else {
                        drop(s);
                        self.add_message("try: : radius <step 1-5>".to_string());
                    }
                } else {
                    s.step = (s.step % 5) + 1;
                    s.force_sync = true;
                    let step = s.step;
                    drop(s);
                    self.add_message(format!("listening radius set to ~{}m", match step {
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
                    
                    self.add_message("connecting to peer...".to_string());
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
                    self.add_message("try: : peer add <url>".to_string());
                }
            }
            ":ttl" => {
                if parts.len() < 2 {
                    self.add_message(format!("your words currently last {} hours", self.ttl_hours));
                    return;
                }
                if let Ok(hours) = parts[1].parse::<i64>() {
                    if hours > 0 {
                        self.ttl_hours = hours;
                        self.add_message(format!("your words will now last {} hours", hours));
                    } else {
                        self.add_message("fade time must be at least 1 hour".to_string());
                    }
                } else {
                    self.add_message("try: : ttl <hours>".to_string());
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
                            self.add_message(format!("location set to {:.4}, {:.4}", lat, lon));
                        } else {
                            self.add_message("couldn't parse those coordinates".to_string());
                        }
                    } else {
                        self.add_message("try: : location set <lat>,<lon>".to_string());
                    }
                } else {
                    let coords_val = {
                        let s = self.sync_state.lock().unwrap();
                        s.coords
                    };
                    self.add_message(format!("current location: {:.4}, {:.4}", coords_val.lat, coords_val.lon));
                }
            }
            _ => {
                self.add_message("not sure what that means — try : help".to_string());
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

        // Resolve active borders
        let feed_border_color = if self.mode == AppMode::Navigating {
            COLOR_BORDER_ACTIVE
        } else {
            COLOR_BORDER_IDLE
        };
        let input_border_color = if self.mode == AppMode::Typing {
            COLOR_BORDER_ACTIVE
        } else {
            COLOR_BORDER_IDLE
        };

        // Left Pane: Ambient Social Feed
        let feed_block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(feed_border_color))
            .style(Style::default().bg(COLOR_BG).fg(COLOR_PRIMARY));

        let (coords, step) = {
            let s = self.sync_state.lock().unwrap();
            (s.coords, s.step)
        };
        let h3_res = step_to_resolution(step);
        let center_cell = h3o::LatLng::new(coords.lat, coords.lon)
            .ok()
            .map(|ll| ll.to_cell(h3_res).to_string())
            .unwrap_or_else(|| "Invalid".to_string());

        let step_radius_str = match step {
            1 => "~100m",
            2 => "~300m",
            3 => "~1km",
            4 => "~3km",
            _ => "~10km",
        };

        // Format posts list
        let mut feed_text = Vec::new();
        if self.posts_cache.is_empty() {
            if self.app_start_time.elapsed().as_secs() >= 30 && !self.dismissed_nudge {
                feed_text.push(Line::from(vec![
                    Span::styled("it's quiet here right now.", Style::default().fg(COLOR_MUTED))
                ]));
                feed_text.push(Line::from(vec![
                    Span::styled("that's normal — murmur is still listening.", Style::default().fg(COLOR_MUTED))
                ]));
                feed_text.push(Line::from(vec![]));
                feed_text.push(Line::from(vec![
                    Span::styled("try saying something, or wait a while.", Style::default().fg(COLOR_MUTED))
                ]));
            } else {
                feed_text.push(Line::from(vec![
                    Span::styled("listening for nearby voices...", Style::default().fg(COLOR_MUTED).add_modifier(Modifier::ITALIC))
                ]));
            }
        } else {
            // Render cached posts starting from scroll offset
            for (idx, (post, vibes)) in self.posts_cache.iter().enumerate() {
                if idx < self.scroll {
                    continue;
                }
                
                let is_our_post = post.author_pubkey == self.identity.public_key_bytes();
                let is_focused = idx == self.focused_post_idx && (self.mode == AppMode::Navigating || self.mode == AppMode::Typing || self.mode == AppMode::VibePicker);
                
                let bar_color = if is_focused {
                    COLOR_BORDER_ACTIVE
                } else if is_our_post {
                    COLOR_ACCENT_YOU
                } else {
                    COLOR_BG
                };
                let prefix_span = Span::styled("│ ", Style::default().fg(bar_color));
                let username_color = if is_our_post { COLOR_ACCENT_YOU } else { COLOR_ACCENT_OTHERS };
                
                // Header
                let mut header = vec![
                    prefix_span.clone(),
                    Span::styled(format!("@{}", post.author_username), Style::default().fg(username_color).add_modifier(Modifier::BOLD)),
                    Span::styled(" · ", Style::default().fg(COLOR_MUTED)),
                    Span::styled(relative_time(post.created_at), Style::default().fg(COLOR_MUTED)),
                ];
                
                if !is_our_post {
                    let dist_m = haversine_distance(coords.lat, coords.lon, post.lat_coarse, post.lon_coarse);
                    let dist_str = fuzzy_distance(dist_m);
                    header.push(Span::styled(" · ", Style::default().fg(COLOR_MUTED)));
                    header.push(Span::styled(dist_str, Style::default().fg(COLOR_MUTED)));
                }
                
                feed_text.push(Line::from(header));
                
                // Content
                feed_text.push(Line::from(vec![
                    prefix_span.clone(),
                    Span::raw(format!("{}", post.content))
                ]));
                
                // Vibes & Picker
                if is_focused && self.mode == AppMode::VibePicker {
                    let mut picker_spans = vec![
                        prefix_span.clone(),
                        Span::styled("react: ", Style::default().fg(COLOR_MUTED)),
                    ];
                    for (i, emoji) in VIBES.iter().enumerate() {
                        let is_opt_focused = i == self.vibe_picker_focus;
                        let style = if is_opt_focused {
                            Style::default().fg(COLOR_BG).bg(COLOR_BORDER_ACTIVE).add_modifier(Modifier::BOLD)
                        } else {
                            Style::default().fg(COLOR_PRIMARY)
                        };
                        picker_spans.push(Span::styled(format!(" {}:{} ", i + 1, emoji), style));
                    }
                    picker_spans.push(Span::styled("  [esc to cancel]", Style::default().fg(COLOR_MUTED).add_modifier(Modifier::DIM)));
                    feed_text.push(Line::from(picker_spans));
                } else if !vibes.is_empty() || (is_focused && (self.mode == AppMode::Navigating || self.mode == AppMode::Typing)) {
                    let mut vibe_spans = vec![prefix_span.clone()];
                    for v in vibes {
                        vibe_spans.push(Span::styled(format!("{} ", v), Style::default().fg(COLOR_VIBE_GLOW)));
                    }
                    if is_focused && (self.mode == AppMode::Navigating || self.mode == AppMode::Typing) {
                        let hint = if self.mode == AppMode::Typing {
                            "   [esc then v to react]"
                        } else {
                            "   [v to react]"
                        };
                        vibe_spans.push(Span::styled(hint, Style::default().fg(COLOR_MUTED).add_modifier(Modifier::DIM)));
                    }
                    feed_text.push(Line::from(vibe_spans));
                }
                
                // Separator / Breathing room
                feed_text.push(Line::from(vec![
                    prefix_span.clone(),
                    Span::styled("───", Style::default().fg(COLOR_BORDER_IDLE)),
                ]));
                feed_text.push(Line::from(vec![
                    prefix_span.clone(),
                ]));
            }
        }

        let feed_paragraph = Paragraph::new(feed_text)
            .block(feed_block)
            .wrap(Wrap { trim: true });
        f.render_widget(feed_paragraph, content_layout[0]);

        // Right Pane: Network & Status
        let right_block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(COLOR_BORDER_IDLE))
            .style(Style::default().bg(COLOR_BG).fg(COLOR_PRIMARY));

        let mut right_lines = Vec::new();
        // Section 1 — identity
        right_lines.push(Line::from(vec![
            Span::styled(format!("@{}", self.identity.username), Style::default().fg(COLOR_ACCENT_YOU).add_modifier(Modifier::BOLD))
        ]));
        right_lines.push(Line::from(vec![
            Span::styled(format!("{} · {}h posts", step_radius_str, self.ttl_hours), Style::default().fg(COLOR_MUTED))
        ]));

        right_lines.push(Line::from(vec![]));
        right_lines.push(Line::from(vec![
            Span::styled("────────────────────", Style::default().fg(COLOR_BORDER_IDLE))
        ]));
        right_lines.push(Line::from(vec![]));

        // Section 2 — presence
        right_lines.push(Line::from(vec![
            Span::styled("presence", Style::default().fg(COLOR_MUTED).add_modifier(Modifier::DIM))
        ]));
        let tunnel_url = self.dht_node_tunnel_url();
        let (dot, status_str, color) = if tunnel_url == "generating..." {
            ("● ", "connecting...", COLOR_ACCENT_SYSTEM)
        } else if tunnel_url == "❌ down" {
            ("○ ", "reconnecting...", COLOR_ERROR)
        } else if self.dht.to_bootstrap().is_empty() {
            ("○ ", "quiet network", COLOR_MUTED)
        } else {
            ("● ", "listening", COLOR_ACCENT_OTHERS)
        };
        let status_display = if status_str == "listening" {
            format!("listening · {} reach", step_radius_str)
        } else {
            status_str.to_string()
        };
        right_lines.push(Line::from(vec![
            Span::styled(dot, Style::default().fg(color)),
            Span::styled(status_display, Style::default().fg(COLOR_PRIMARY)),
        ]));

        right_lines.push(Line::from(vec![]));
        right_lines.push(Line::from(vec![
            Span::styled("────────────────────", Style::default().fg(COLOR_BORDER_IDLE))
        ]));
        right_lines.push(Line::from(vec![]));

        // Section 3 — nearby
        right_lines.push(Line::from(vec![
            Span::styled("nearby", Style::default().fg(COLOR_MUTED).add_modifier(Modifier::DIM))
        ]));
        if self.peers_cache.is_empty() {
            right_lines.push(Line::from(vec![
                Span::styled("no one yet —", Style::default().fg(COLOR_MUTED))
            ]));
            right_lines.push(Line::from(vec![
                Span::styled("the quiet is normal", Style::default().fg(COLOR_MUTED))
            ]));
        } else {
            for (peer, dist_opt) in &self.peers_cache {
                // Note: We rely on deterministic username derivation from the public key here.
                // The Peer struct/schema does not store the announced username from PeerRecord.
                let peer_username = crate::identity::derive_username(&peer.pubkey);
                let dist_str = if let Some(m) = dist_opt {
                    format!(" · {}", fuzzy_distance(*m))
                } else {
                    String::new()
                };
                let peer_color = if peer.failed_pings > 0 { COLOR_ERROR } else { COLOR_ACCENT_OTHERS };
                right_lines.push(Line::from(vec![
                    Span::styled("● ", Style::default().fg(peer_color)),
                    Span::styled(format!("@{}", peer_username), Style::default().fg(COLOR_PRIMARY)),
                    Span::styled(dist_str, Style::default().fg(COLOR_MUTED)),
                ]));
            }
        }

        right_lines.push(Line::from(vec![]));
        right_lines.push(Line::from(vec![
            Span::styled("────────────────────", Style::default().fg(COLOR_BORDER_IDLE))
        ]));
        right_lines.push(Line::from(vec![]));

        // Quick Command reference
        right_lines.push(Line::from(vec![
            Span::styled(": for commands", Style::default().fg(COLOR_MUTED))
        ]));
        right_lines.push(Line::from(vec![
            Span::styled(": about · : help", Style::default().fg(COLOR_MUTED))
        ]));

        let right_paragraph = Paragraph::new(right_lines)
            .block(right_block)
            .wrap(Wrap { trim: true });
        f.render_widget(right_paragraph, content_layout[1]);

        // Bottom Bar: Terminal Command Router Input
        let input_block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(input_border_color))
            .style(Style::default().bg(COLOR_BG).fg(COLOR_PRIMARY));

        let current_input = self.input.clone();
        let display_content = if self.mode == AppMode::Navigating {
            vec![Line::from(vec![
                Span::styled("~ navigation · j/k to select · v to react · i to type", Style::default().fg(COLOR_MUTED).add_modifier(Modifier::ITALIC)),
            ])]
        } else if current_input.is_empty() {
            if let Some(last_msg) = self.messages.last() {
                vec![Line::from(vec![
                    Span::styled("~ ", Style::default().fg(COLOR_ACCENT_SYSTEM)),
                    Span::styled(last_msg.clone(), Style::default().fg(COLOR_MUTED)),
                ])]
            } else {
                vec![Line::from(vec![
                    Span::styled("~ say something, : for commands, or Esc to navigate", Style::default().fg(COLOR_MUTED).add_modifier(Modifier::ITALIC)),
                ])]
            }
        } else {
            vec![Line::from(vec![
                Span::styled("> ", Style::default().fg(COLOR_ACCENT_SYSTEM).add_modifier(Modifier::BOLD)),
                Span::styled(current_input, Style::default().fg(COLOR_PRIMARY)),
            ])]
        };

        let input_paragraph = Paragraph::new(display_content).block(input_block);
        f.render_widget(input_paragraph, main_layout[1]);

        // Render Overlays
        if self.mode != AppMode::Typing && self.mode != AppMode::Navigating {
            let area = centered_rect(65, 80, size);
            let overlay_block = Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(COLOR_BORDER_ACTIVE))
                .style(Style::default().bg(COLOR_BG));

            match self.mode {
                AppMode::WelcomeOverlay => {
                    let welcome_lines = vec![
                        Line::from(vec![]),
                        Line::from(vec![Span::styled(format!("you are @{}", self.identity.username), Style::default().fg(COLOR_ACCENT_YOU).add_modifier(Modifier::BOLD))]),
                        Line::from(vec![]),
                        Line::from(vec![Span::styled("that name wasn't chosen — it came from your", Style::default().fg(COLOR_PRIMARY))]),
                        Line::from(vec![Span::styled("key, like everyone else's does.", Style::default().fg(COLOR_PRIMARY))]),
                        Line::from(vec![]),
                        Line::from(vec![Span::styled("murmur shows you what people nearby are", Style::default().fg(COLOR_PRIMARY))]),
                        Line::from(vec![Span::styled("saying, right now. nothing more.", Style::default().fg(COLOR_PRIMARY))]),
                        Line::from(vec![]),
                        Line::from(vec![Span::styled("no profiles. no followers. no history.", Style::default().fg(COLOR_PRIMARY))]),
                        Line::from(vec![Span::styled("words fade after a day.", Style::default().fg(COLOR_PRIMARY))]),
                        Line::from(vec![]),
                        Line::from(vec![Span::styled("say something with : post, or just listen.", Style::default().fg(COLOR_PRIMARY))]),
                        Line::from(vec![]),
                        Line::from(vec![Span::styled("press any key to begin", Style::default().fg(COLOR_MUTED).add_modifier(Modifier::DIM))]),
                    ];
                    let welcome_paragraph = Paragraph::new(welcome_lines)
                        .block(overlay_block)
                        .alignment(ratatui::layout::Alignment::Center);
                    f.render_widget(welcome_paragraph, area);
                }
                AppMode::AboutOverlay => {
                    let about_lines = vec![
                        Line::from(vec![Span::styled("murmur", Style::default().fg(COLOR_ACCENT_SYSTEM).add_modifier(Modifier::BOLD))]),
                        Line::from(vec![]),
                        Line::from(vec![Span::styled("you did not choose your name.", Style::default().fg(COLOR_PRIMARY))]),
                        Line::from(vec![Span::styled("your words will disappear.", Style::default().fg(COLOR_PRIMARY))]),
                        Line::from(vec![Span::styled("no one can follow you.", Style::default().fg(COLOR_PRIMARY))]),
                        Line::from(vec![Span::styled("no one can find you by searching.", Style::default().fg(COLOR_PRIMARY))]),
                        Line::from(vec![Span::styled("you can only hear people near you.", Style::default().fg(COLOR_PRIMARY))]),
                        Line::from(vec![Span::styled("the crowd determines how far your voice carries.", Style::default().fg(COLOR_PRIMARY))]),
                        Line::from(vec![Span::styled("there is no engagement to optimize.", Style::default().fg(COLOR_PRIMARY))]),
                        Line::from(vec![]),
                        Line::from(vec![Span::styled("─────────────────────────────", Style::default().fg(COLOR_BORDER_IDLE))]),
                        Line::from(vec![]),
                        Line::from(vec![
                            Span::styled("your identity:    ", Style::default().fg(COLOR_MUTED)),
                            Span::styled(format!("@{}", self.identity.username), Style::default().fg(COLOR_ACCENT_YOU)),
                        ]),
                        Line::from(vec![
                            Span::styled("your reach:       ", Style::default().fg(COLOR_MUTED)),
                            Span::styled(format!("{} (settles automatically)", step_radius_str), Style::default().fg(COLOR_PRIMARY)),
                        ]),
                        Line::from(vec![
                            Span::styled("posts fade after: ", Style::default().fg(COLOR_MUTED)),
                            Span::styled(format!("{} hours", self.ttl_hours), Style::default().fg(COLOR_PRIMARY)),
                        ]),
                        Line::from(vec![]),
                        Line::from(vec![Span::styled("─────────────────────────────", Style::default().fg(COLOR_BORDER_IDLE))]),
                        Line::from(vec![]),
                        Line::from(vec![Span::styled(": help        see all commands", Style::default().fg(COLOR_PRIMARY))]),
                        Line::from(vec![Span::styled(": debug       show technical details", Style::default().fg(COLOR_PRIMARY))]),
                        Line::from(vec![Span::styled("esc           close this screen", Style::default().fg(COLOR_MUTED))]),
                    ];
                    let about_paragraph = Paragraph::new(about_lines)
                        .block(overlay_block)
                        .alignment(ratatui::layout::Alignment::Center);
                    f.render_widget(about_paragraph, area);
                }
                AppMode::HelpOverlay => {
                    let help_lines = vec![
                        Line::from(vec![Span::styled("commands", Style::default().fg(COLOR_ACCENT_SYSTEM).add_modifier(Modifier::BOLD))]),
                        Line::from(vec![]),
                        Line::from(vec![
                            Span::styled("  : post <text>          ", Style::default().fg(COLOR_ACCENT_YOU)),
                            Span::styled("say something", Style::default().fg(COLOR_PRIMARY)),
                        ]),
                        Line::from(vec![
                            Span::styled("  : vibe <id> <emoji>    ", Style::default().fg(COLOR_ACCENT_YOU)),
                            Span::styled("react to a post nearby", Style::default().fg(COLOR_PRIMARY)),
                        ]),
                        Line::from(vec![
                            Span::styled("  : radius               ", Style::default().fg(COLOR_ACCENT_YOU)),
                            Span::styled("see how far you're listening", Style::default().fg(COLOR_PRIMARY)),
                        ]),
                        Line::from(vec![
                            Span::styled("  : peer add <url>        ", Style::default().fg(COLOR_ACCENT_YOU)),
                            Span::styled("connect to someone directly", Style::default().fg(COLOR_PRIMARY)),
                        ]),
                        Line::from(vec![
                            Span::styled("  : ttl <hours>           ", Style::default().fg(COLOR_ACCENT_YOU)),
                            Span::styled("how long your words last", Style::default().fg(COLOR_PRIMARY)),
                        ]),
                        Line::from(vec![
                            Span::styled("  : location set <lat,lon>", Style::default().fg(COLOR_ACCENT_YOU)),
                            Span::styled("override where you are", Style::default().fg(COLOR_PRIMARY)),
                        ]),
                        Line::from(vec![
                            Span::styled("  : about                 ", Style::default().fg(COLOR_ACCENT_YOU)),
                            Span::styled("what murmur is", Style::default().fg(COLOR_PRIMARY)),
                        ]),
                        Line::from(vec![
                            Span::styled("  : debug                 ", Style::default().fg(COLOR_ACCENT_YOU)),
                            Span::styled("technical details, if you're curious", Style::default().fg(COLOR_PRIMARY)),
                        ]),
                        Line::from(vec![
                            Span::styled("  : quit                  ", Style::default().fg(COLOR_ACCENT_YOU)),
                            Span::styled("leave", Style::default().fg(COLOR_PRIMARY)),
                        ]),
                        Line::from(vec![]),
                        Line::from(vec![Span::styled("navigation & reactions", Style::default().fg(COLOR_ACCENT_SYSTEM).add_modifier(Modifier::BOLD))]),
                        Line::from(vec![]),
                        Line::from(vec![
                            Span::styled("  Esc                     ", Style::default().fg(COLOR_ACCENT_YOU)),
                            Span::styled("exit input mode to navigate feed", Style::default().fg(COLOR_PRIMARY)),
                        ]),
                        Line::from(vec![
                            Span::styled("  j / k or up/down        ", Style::default().fg(COLOR_ACCENT_YOU)),
                            Span::styled("select/focus a post", Style::default().fg(COLOR_PRIMARY)),
                        ]),
                        Line::from(vec![
                            Span::styled("  v                       ", Style::default().fg(COLOR_ACCENT_YOU)),
                            Span::styled("react to selected post (vibe picker)", Style::default().fg(COLOR_PRIMARY)),
                        ]),
                        Line::from(vec![
                            Span::styled("  i / Enter               ", Style::default().fg(COLOR_ACCENT_YOU)),
                            Span::styled("re-enter input mode to type/post", Style::default().fg(COLOR_PRIMARY)),
                        ]),
                        Line::from(vec![]),
                        Line::from(vec![Span::styled("esc to close", Style::default().fg(COLOR_MUTED))]),
                    ];
                    let help_paragraph = Paragraph::new(help_lines)
                        .block(overlay_block);
                    f.render_widget(help_paragraph, area);
                }
                AppMode::DebugOverlay => {
                    let pk_hex = hex::encode(self.identity.public_key_bytes());
                    let dht_id = hex::encode(derive_node_id(&self.identity.public_key_bytes()));
                    let debug_lines = vec![
                        Line::from(vec![Span::styled("debug", Style::default().fg(COLOR_ACCENT_SYSTEM).add_modifier(Modifier::BOLD))]),
                        Line::from(vec![]),
                        Line::from(vec![
                            Span::styled("  public key:       ", Style::default().fg(COLOR_MUTED)),
                            Span::styled(format!("{}...", &pk_hex[..12]), Style::default().fg(COLOR_PRIMARY)),
                        ]),
                        Line::from(vec![
                            Span::styled("  dht node id:       ", Style::default().fg(COLOR_MUTED)),
                            Span::styled(format!("{}...", &dht_id[..12]), Style::default().fg(COLOR_PRIMARY)),
                        ]),
                        Line::from(vec![
                            Span::styled("  dht udp port:      ", Style::default().fg(COLOR_MUTED)),
                            Span::styled(self.dht.udp_port().to_string(), Style::default().fg(COLOR_PRIMARY)),
                        ]),
                        Line::from(vec![
                            Span::styled("  tunnel url:        ", Style::default().fg(COLOR_MUTED)),
                            Span::styled(self.dht_node_tunnel_url(), Style::default().fg(COLOR_PRIMARY)),
                        ]),
                        Line::from(vec![
                            Span::styled("  h3 cell:           ", Style::default().fg(COLOR_MUTED)),
                            Span::styled(center_cell.clone(), Style::default().fg(COLOR_PRIMARY)),
                        ]),
                        Line::from(vec![
                            Span::styled("  radius step:       ", Style::default().fg(COLOR_MUTED)),
                            Span::styled(format!("{} (resolution {})", step, h3_res), Style::default().fg(COLOR_PRIMARY)),
                        ]),
                        Line::from(vec![
                            Span::styled("  active peers:      ", Style::default().fg(COLOR_MUTED)),
                            Span::styled(self.peers_cache.len().to_string(), Style::default().fg(COLOR_PRIMARY)),
                        ]),
                        Line::from(vec![
                            Span::styled("  sync interval:     ", Style::default().fg(COLOR_MUTED)),
                            Span::styled("5m (±30s jitter)", Style::default().fg(COLOR_PRIMARY)),
                        ]),
                        Line::from(vec![]),
                        Line::from(vec![Span::styled("esc to close", Style::default().fg(COLOR_MUTED))]),
                    ];
                    let debug_paragraph = Paragraph::new(debug_lines)
                        .block(overlay_block);
                    f.render_widget(debug_paragraph, area);
                }
                _ => {}
            }
        }
    }

    fn dht_node_tunnel_url(&self) -> String {
        let s = self.sync_state.lock().unwrap();
        s.tunnel_url.clone().unwrap_or_else(|| "generating...".to_string())
    }
}

fn centered_rect(percent_x: u16, percent_y: u16, r: ratatui::layout::Rect) -> ratatui::layout::Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}
