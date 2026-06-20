# murmur

## Functional & Technical Specification

**Version:** 1.0 (Finalized)
**Status:** Implementation Ready
**Stack:** Rust · Turso DB (libSQL embedded) · Kademlia DHT · H3 Hexagons · Cloudflare Tunnel · ratatui TUI

### 1. Philosophy

`murmur` is not a social network. It is ambient social presence — the digital equivalent of overhearing conversations in a public space.

Core principles:

* You did not choose your name.
* Your words will disappear.
* No one can follow you.
* No one can find you by searching.
* You can only hear people near you.
* The crowd determines how far your voice carries.
* There is no engagement to optimize.

`murmur` resists every dark pattern of modern social media not through policy, but through architecture. The features simply do not exist.

---

### 2. Non-Goals (Explicit)

The following will never be implemented. They are architectural constraints, not roadmap deferrals.

| Feature | Reason Excluded |
| --- | --- |
| **Profiles / bios** | Identity performance |
| **Username choice** | Social signaling |
| **Follower/following** | Social graph = power dynamics |
| **Link sharing / reposts** | Virality vector |
| **Global feed** | Loses local texture |
| **Search** | Targeted harassment vector |
| **Notifications** | Compulsion loops |
| **Read receipts** | Anxiety mechanics |
| **Ads / monetization** | Misaligned incentives |
| **Groups / channels** | Tribalism mechanics |
| **Filters / algorithms** | Feed manipulation |
| **Permanent history** | Surveillance |
| **Verified accounts** | Status hierarchy |
| **Reactions with public counts** | Vanity metrics |

---

### 3. What murmur Is

A single Rust binary that every user runs locally. It:

1. Generates a cryptographic identity (Ed25519 keypair) on first launch.
2. Derives a random, non-chosen username from that identity.
3. Starts a local HTTP server (Axum) to serve and receive posts.
4. Exposes that server publicly via Cloudflare Tunnel.
5. Announces itself into a Kademlia DHT network keyed by H3 geographic cell.
6. Discovers nearby peers from the same DHT cell set.
7. Polls peer feeds periodically and caches them in local Turso DB (embedded libSQL).
8. Renders everything as a TUI with a command bar.
9. Expires all content after a configurable TTL (default: 24 hours).

No cloud account. No registration. No database server. No deployment. The binary is the entire application.

---

### 4. Identity System

#### 4.1 Keypair Generation

On first launch, `murmur` generates an Ed25519 keypair using `ed25519-dalek`:

* `private_key`: 32 bytes (never leaves device)
* `public_key`: 32 bytes (your identity fingerprint)
* `node_id`: SHA3-256(public_key) → 32 bytes (DHT node address)

Stored in `~/.murmur/identity.key` (`chmod 600`). Never transmitted. The public key is included with every post for signature verification.

#### 4.2 Username Derivation

Usernames are deterministically derived from the public key:

```rust
fn derive_username(pubkey: &[u8; 32]) -> String {
    let seed = u64::from_le_bytes(pubkey[0..8].try_into().unwrap());
    let mut rng = SmallRng::seed_from_u64(seed);

    let adj  = ADJECTIVES[rng.next_u32() as usize % ADJECTIVES.len()];
    let noun = NOUNS[rng.next_u32() as usize % NOUNS.len()];
    let num  = rng.next_u32() % 9000 + 1000;

    format!("{}-{}-{}", adj, noun, num)
}

```

*Examples:* `wandering-pike-4471`, `silent-crow-8823`. Collision probability is negligible. To get a new name, generate a new keypair (which wipes your identity entirely).

#### 4.3 Ephemeral Mode

Launch flag `--ephemeral` generates a fresh keypair in-memory each session. New name, new node ID, zero continuity. Full ghost mode.

---

### 5. Geographic System (H3)

#### 5.1 Location Acquisition

Supported in priority order:

1. **System GPS** (Hardware permitted).
2. **IP Geolocation** (Offline MaxMind GeoLite2 bundled via `include_bytes!`).
3. **Manual Input** (`:location set Kathmandu` or `:location set 27.7172,85.3240`).

#### 5.2 Coordinate Coarsening

Exact coordinates are coarsened to a ~1km grid *before* any H3 encoding to prevent exact location tracking.

```rust
fn coarsen(lat: f64, lon: f64) -> (f64, f64) {
    let grid = 0.009; // ~1km in degrees
    ((lat / grid).round() * grid, (lon / grid).round() * grid)
}

```

#### 5.3 H3 Hexagonal Grid System

`murmur` uses Uber's H3 grid (`h3o` crate). Hexagons solve the boundary proximity problem inherent to geohashes. All 6 neighbors are equidistant from the center, eliminating "invisible walls" dividing streets.

#### 5.4 Cell Querying with `grid_disk`

The `h3o` crate maps $k$-ring expansion predictably.

```rust
use h3o::{CellIndex, LatLng, Resolution};

fn cells_to_query(lat: f64, lon: f64, resolution: Resolution, k: u32) -> Vec<CellIndex> {
    let center = LatLng::new(lat, lon).expect("valid").to_cell(resolution);
    center.grid_disk::<Vec<_>>(k) // k=1 returns 7 cells (center + 6 neighbors)
}

```

---

### 6. Adaptive Radius Engine

The feed radius is continuously computed to maintain a target window of **30–80 visible posts** in the active feed.

#### 6.1 H3 Radius Ladder

Always 7 cells queried (center + 6 neighbors at `k=1`).

| Step | H3 Res | $k$-ring | Approx Area | Context |
| --- | --- | --- | --- | --- |
| **1 (floor)** | 10 | 1 | ~1.2 km² | Dense urban block |
| **2** | 9 | 1 | ~8 km² | Neighborhood |
| **3** | 8 | 1 | ~51 km² | Urban district |
| **4** | 7 | 1 | ~360 km² | City zone |
| **5 (ceiling)** | 6 | 1 | ~2500 km² | Metro/Region |

#### 6.2 Adjustment Algorithm

The engine evaluates the visible post count over a smoothed 3-tick history. If outside the goldilocks zone (30-80), it steps up or down the ladder, respecting a 5-minute cooldown to let the network breathe.

#### 6.3 What This Feels Like

* **Dense urban (Thamel at peak hours):** Radius shrinks to Step 1. You hear your street corner.
* **Suburban Nagarjun:** Radius settles at Step 2–3. You hear your district and immediate valleys.
* **Remote trail:** Radius expands to Step 5. Regional voices, rare and sparse.

---

### 7. DHT Network & Resilience

#### 7.1 Bootstrap Strategy

ISP interference is mitigated via a layered fallback stack.

1. **Tier 1 (DNS):** `router.bittorrent.com:6881`
2. **Tier 2 (IP Fallback):** Hardcoded IPs (e.g., `82.221.103.244:6881`) to bypass DNS blocks.
3. **Tier 3 (Alt Ports):** Ports `6969` and `80` to bypass strict UDP filtering.
4. **Tier 4 (Custom):** User-configured VPS nodes.

All probes fire in parallel; the first successful response initializes the DHT.

#### 7.2 Sparse Mode

If all bootstrap attempts fail, `murmur` enters **Sparse Mode**.

* DHT discovery is disabled.
* Manual peers (`:peer add`) are polled normally.
* Silent retry occurs every 30 minutes.
* TUI indicates: `DHT ⚠ sparse mode`.

#### 7.3 Manual Peers & Exchange

Independent of the DHT, allowing pure P2P mapping between known contacts. Stored in `config.toml` and persisted across restarts.

#### 7.4 Protocol Data

Peer records are announced to the DHT using `SHA1(h3_cell_index_as_bytes)` as the routing key. All records are CBOR-encoded and Ed25519 signed.

---

### 8. Feed Protocol

#### 8.1 Post Structure

```rust
struct Post {
    id: Uuid,
    author_pubkey: [u8; 32],
    author_username: String,
    content: String,           // Max 280 chars. URLs stripped. Plain text only.
    lat_coarse: f64,
    lon_coarse: f64,
    h3_cell: String,           // H3 Res 9 index stored as hex string
    created_at: i64,
    expires_at: i64,           // TTL default 86400s
    sig: [u8; 64],             // Ed25519 signature
}

```

#### 8.2 Vibes

Reactions (✨ 🌊 🔥 💀 🌿 👁️ 🫧) are private. No counts are displayed. The author sees only a dim `~vibes~` aggregate glow.

#### 8.3 Rate Limiting (`tower-governor`)

Axum API endpoints (`/feed`, `/vibe`, `/ping`) are protected by a token bucket rate limiter to survive "thundering herds" when radius expands.

* **Burst:** 15 requests allowed immediately.
* **Refill:** 1 token per second (sustained 60/min).

#### 8.4 Star-Topology Feed Architecture (No Relay/Gossip)

`murmur` uses a direct federated star-topology architecture for feed retrieval:
* **Direct Fetching:** When a peer is discovered, your client fetches their posts directly from their local server endpoint (via their Cloudflare tunnel URL).
* **Owner-Only Feeds:** The `/feed` endpoint on a peer's server only returns the posts authored by that specific peer. It does not relay or gossip posts from other peers.
* **No Propagation:** Your local node does not serve, index, store, or forward other users' posts to third-party nodes. Communication is strictly peer-to-peer between the subscriber and the publisher.

---

### 9. Local Storage (Turso DB / libSQL Embedded)

`murmur` uses the `libsql` Rust crate to run a high-performance embedded SQLite-compatible engine. No remote synchronization to Turso Cloud is used; the architecture remains strictly local-first.

#### 9.1 Concurrency & Write-Ahead Logging (WAL)

Because the sync engine handles batch inserts from multiple concurrent peer fetches via `rayon`, SQLite's default write serialization will trigger `SQLITE_BUSY` lock errors. `murmur` explicitly configures the libSQL connection pool:

```rust
// src/store/schema.rs
conn.execute("PRAGMA journal_mode = WAL;", ()).await?;
conn.execute("PRAGMA synchronous = NORMAL;", ()).await?;
conn.execute("PRAGMA busy_timeout = 5000;", ()).await?; // Queue locks instead of failing

```

#### 9.2 The H3 Data Type Guard

H3 cell indexes are 64-bit unsigned integers (`u64`). SQLite only supports *signed* 64-bit integers (`i64`). To prevent undocumented bit-alignment corruption or overflow panics, all H3 cell indices are safely cast and stored as 16-character hexadecimal strings (`TEXT`).

#### 9.3 Schema Excerpt

```sql
CREATE TABLE posts (
    id          TEXT PRIMARY KEY,
    author_pk   BLOB NOT NULL,
    h3_cell     TEXT NOT NULL,      -- Safely stored as hex string
    created_at  INTEGER NOT NULL,
    expires_at  INTEGER NOT NULL,
    sig         BLOB NOT NULL
);

CREATE TABLE peers (
    pubkey        BLOB PRIMARY KEY,
    tunnel_url    TEXT NOT NULL,
    last_seen     INTEGER NOT NULL,
    failed_pings  INTEGER DEFAULT 0, -- Used for zombie polling backoff
    is_manual     INTEGER DEFAULT 0
);

```

Expired posts are cleared via a background task running every 15 minutes.

---

### 10. Sync Engine

#### 10.1 The Polling Loop (Jitter & Adaptive Heartbeat)

To prevent synchronized network hammering, randomized jitter is applied to all sleeps. Furthermore, if the radius and peer count are stable, the polling interval backs off to save battery.

* **Active state:** 5 minutes `±30s` jitter.
* **Settled state:** 15 minutes `±30s` jitter.

#### 10.2 Time-Jump Wake Detection

Instead of relying on brittle cross-platform OS power APIs, `murmur` tracks async yield durations. If an awaited `sleep(300s)` actually took `330s+`, the OS was suspended. The system immediately flushes stale peers and forces a DHT lookup upon wake.

#### 10.3 Rayon Crypto Offloading

Batch validating 50 Ed25519 signatures blocks the Tokio async reactor. The engine bridges to a dedicated system thread pool:

```rust
// src/sync/fetcher.rs
let valid_posts: Vec<Post> = tokio::task::spawn_blocking(move || {
    raw_posts.into_par_iter() // Rayon parallel execution
        .filter(|p| validate_post(p).is_ok())
        .collect()
}).await?;
store.insert_posts_batch(&valid_posts).await?;

```

#### 10.4 Zombie Polling Backoff

If a manual peer's laptop is closed for the weekend, `murmur` will not endlessly hammer a dead URL.

* Each `GET /ping` failure increments `failed_pings`.
* Poll delay scales exponentially: `base_interval * (2 ^ min(failed_pings, 5))`.
* Caps at roughly ~2.5 hours. Resets to `0` upon manual `:peer ping` or DHT rediscovery.

---

### 11. TUI Design

Built with `ratatui`. Keyboard-driven, zero mouse support.

```text
╔══════════════════════════════════════════════════════════════╗
║  murmur  │ wandering-pike-4471  │ 📍 ~8km² │ 31 nearby      ║
╠═══════════════════════════════════╦══════════════════════════╣
║  FEED                             ║  NEARBY                  ║
║                                   ║                          ║
║  silent-crow-8823                 ║  🟢 hollow-dusk-2201     ║
║  📍 ~0.8km  ·  14m ago           ║  🟢 pale-moth-3391       ║
║  the fog this morning             ║  🟡 iron-veil-7823       ║
║  was something else               ║     (syncing...)         ║
║                           ~vibes~ ║  🔴 drifting-ash-1104 [m]║
║  ─────────────────────────────    ║     (backoff: 30m)       ║
║  pale-moth-3391                   ║                          ║
║  📍 ~2.3km  ·  1h ago            ║  ──────────────────────  ║
║  anyone else hear that            ║  DHT: ● 2,847 nodes      ║
║  thunderstorm at 3am              ║  radius: step 2 (~8km²)  ║
║                                   ║  posts expire: 24h       ║
║  iron-veil-7823                   ║  uptime: 2h 14m          ║
║  📍 ~1.1km  ·  3h ago            ║  tunnel: ✅ active       ║
║  (post expired)                   ║                          ║
╠═══════════════════════════════════╩══════════════════════════╣
║ > _                                                          ║
╚══════════════════════════════════════════════════════════════╝

```

Command Bar syntax: `:post <text>`, `:vibe <id> <emoji>`, `:radius`, `:peer add <url>`, `:ttl <hours>`, `:dht status`.

---

### 12. Cloudflare Tunnel Integration

The binary spawns `cloudflared` as a child process, proxying the local port `3731`.

* **Quick Mode:** Allocates a random `trycloudflare.com` subdomain (prone to ISP blocking).
* **Named Mode:** Maps to a user-owned domain (highly recommended for stability).

---

### 13. Security Model

| Threat | Mitigation |
| --- | --- |
| **Fake post injection** | Ed25519 signatures on all data. |
| **Location tracking** | ~1km coarsening + H3 cell boundary abstraction. |
| **DHT poisoning** | Unsigned/invalid peer records discarded. |
| **Thundering herd (Spam)** | Token bucket rate limiting (Burst 15). |
| **Link spreading** | Regex URL stripping at write and ingest. |

*Note: murmur does not protect against ISP traffic analysis or provide plausible deniability. Cloudflare inherently sees HTTP metadata (but not content).*

---

### 14. Binary Distribution & Ecosystem

Compiles to a single static binary leveraging `x86_64-unknown-linux-musl`, `aarch64-apple-darwin`, and `x86_64-pc-windows-msvc`. Maximum payload ~15MB.

**Core Crates:**

* `tokio` (Async runtime)
* `axum`, `tower-governor` (HTTP & Rate Limiting)
* `libsql` (Turso DB embedded storage)
* `mainline` (BEP 5 Kademlia DHT)
* `h3o` (Uber H3 Hexagons)
* `rayon` (Crypto validation parallelism)
* `ed25519-dalek` (Identity)
* `ratatui` (TUI interface)

---

### 15. v1.1+ Roadmap Deferrals

* **Tor (`arti`) Transport:** Integrating Tor Onion Services to replace Cloudflare Tunnels for pure decentralized NAT traversal. Deferred until `tor-hsservice` crate stabilizes.
* **Bloom Filter Delta Sync:** Implementing Merkle root comparisons on `/sync-check` to save bandwidth at high peer densities.
* **Client-Side Blocklists:** Local UI filtering via `:block <pubkey>` to mitigate localized spam.