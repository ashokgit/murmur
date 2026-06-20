# murmur 🤫

> Ambient, local-first social presence for the terminal.

`murmur` is a decentralized, location-bound, local-first social presence client. It lets you discover other users nearby and share short status updates (up to 280 characters) without relying on a central database, signup screens, or tracking servers. 

Everything runs directly on your computer: cryptographic keys, a local-first SQLite WAL database, P2P network discovery over the Mainline DHT, and a beautiful Terminal User Interface (TUI) powered by `ratatui`.

---

## Features

- **Local-First & Ephemeral:** Your posts and private key are stored securely on your machine (`~/.murmur/`). Ephemeral mode (`--ephemeral`) allows running entirely in-memory.
- **Privacy-Coarsened Location:** Leverages the Uber H3 spatial index to group users within a ~1km, ~3km, or ~10km radius grid. Your exact GPS coordinates never leave your machine.
- **Zero Configuration TUI:** An interactive terminal interface designed with glassmorphic aesthetics, featuring a real-time feed, nearby peer listing, system console logs, and active status indicators.
- **Auto Tunneling (P2P Over NAT):** Automatically spawns a Cloudflare Quick Tunnel to expose your local feed endpoint externally, bypass firewalls/NATs, and share your feed with peers.
- **Mainline DHT Discovery:** Bootstraps peer discovery using the BitTorrent Mainline DHT, matching peers located inside matching geographic H3 grids.
- **Rayon-Parallel Validation:** Pulls peers' feeds asynchronously, validating cryptographic signatures using a Rayon thread pool for high performance.
- **Interactive Vibe Reactions:** React to updates from other peers with local aggregate emoji vibes.

---

## Installation

### Method 1: macOS Disk Image (.dmg) (Recommended)

1. Download **`Murmur.dmg`** from the latest release.
2. Double-click the `.dmg` file to mount it.
3. Drag **`Murmur.app`** into your **`Applications`** folder.
4. Run `Murmur.app` from your Applications list.

*Note: Since it is self-signed, macOS might show a warning. Right-click the app and choose "Open", or go to `System Settings > Privacy & Security` and click "Open Anyway".*

### Method 2: Building from Source (All Platforms)

#### Prerequisites
1. **Rust/Cargo:** Install Rust via [rustup](https://rustup.rs):
   ```bash
   curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
   ```
2. **cloudflared:** Required to expose your feed to the network:
   - **macOS:** `brew install cloudflared`
   - **Linux:** Install via your package manager or directly from [Cloudflare](https://github.com/cloudflare/cloudflared).

#### Build & Run
Clone the repository and run:
```bash
git clone https://github.com/username/murmur.git
cd murmur
cargo run --release
```

---

## How to Use

When you launch `murmur`, it will:
1. Load or generate a secure Ed25519 identity keypair (`~/.murmur/identity.key`).
2. Resolve your general location via a IP-API lookup (which is coarsened immediately to a H3 grid cell).
3. Start the local server (`127.0.0.1:3731`), launch a Cloudflare Tunnel, and begin peer discovery over the DHT.

### Keyboard Shortcuts
- `Tab` / `Shift+Tab` – Switch focus between the Feed, Peers, and Command Bar.
- `Up` / `Down` – Scroll through the active panel (Feeds or Peers).
- `Esc` – Clear input or unfocus the command bar.
- `Ctrl+C` – Safely shutdown all background tasks and exit.

### Command Console Bar
Press `:` to focus the command input router at the bottom of the screen. The following commands are supported:

| Command | Description | Example |
| :--- | :--- | :--- |
| `:post <text>` | Publish a new status update (max 280 chars, URLs auto-stripped) | `:post sipping coffee in Kathmandu ☕` |
| `:vibe <post_id> <emoji>` | React to a post with an emoji vibe | `:vibe <uuid> 🔥` |
| `:radius` | Cycle discovery resolution step (~1km, ~3km, ~10km radius) | `:radius` |
| `:peer add <url>` | Manually connect to a specific peer's tunnel URL | `:peer add https://xyz.trycloudflare.com` |
| `:ttl <hours>` | Set lifetime (TTL) for your next posts | `:ttl 12` |
| `:dht status` | Print current DHT routing table and network status | `:dht status` |

---

## Architectural Highlights

```
              ┌──────────────────────────────────────────────┐
              │                 murmur TUI                   │
              │  (Left: Feeds, Right: Peers, Bottom: Cmd)   │
              └──────────────────────┬───────────────────────┘
                                     │
         ┌───────────────────────────┼───────────────────────────┐
         ▼                           ▼                           ▼
┌─────────────────┐         ┌─────────────────┐         ┌─────────────────┐
│  Axum HTTP API  │         │  Mainline DHT   │         │ libSQL Database │
│  (Port 3731)    │         │ (Peer Discovery)│         │   (WAL Mode)    │
└────────┬────────┘         └────────┬────────┘         └────────┬────────┘
         │                           │                           │
         ▼                           ▼                           ▼
┌─────────────────┐         ┌─────────────────┐         ┌─────────────────┐
│ Cloudflare NAT  │         │ H3 Geo-Indexing │         │ ed25519-dalek   │
│ (Quick Tunnel)  │         │ (~1km-10km Grid)│         │ (Identity Keys) │
└─────────────────┘         └─────────────────┘         └─────────────────┘
```

- **Authentication:** All posts are signed using Ed25519. Peers verify the signatures of incoming posts using Rayon before saving them to their databases.
- **DHT Discovery:** Discovery relies on publishing the active H3 cell index (InfoHash) to the DHT, letting other nodes in the same cell discover each other's UDP endpoints.
- **WAL Engine:** The application uses SQLite in Write-Ahead Log (WAL) mode to permit concurrent read queries from the TUI and background writes from the sync engine.

### Star-Topology Feed Architecture (No Relay/Gossip)

`murmur` uses a direct federated star-topology architecture for feed retrieval:
- **Direct Fetching:** When you discover a peer in your H3 cells, your client fetches their posts directly from their local server endpoint (via their Cloudflare tunnel URL).
- **Owner-Only Feeds:** The `/feed` endpoint on a peer's server only returns the posts authored by that specific peer. It does not relay or gossip posts from other peers.
- **No Propagation:** Your local node does not serve, index, store, or forward other users' posts to third-party nodes. Communication is strictly peer-to-peer between the subscriber and the publisher.

---

## License

MIT License. See [LICENSE](LICENSE) for details.
