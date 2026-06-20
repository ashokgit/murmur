#!/bin/bash
set -e

# Colors for output
GREEN='\033[0;32m'
BLUE='\033[0;34m'
YELLOW='\033[1;33m'
RED='\033[0;31m'
NC='\033[0m' # No Color

echo -e "${BLUE}=== murmur Installer ===${NC}"

# 1. Check prerequisites
echo -e "\n${BLUE}[1/4] Checking prerequisites...${NC}"

if ! command -v cargo &> /dev/null; then
    echo -e "${RED}Error: Rust/Cargo is not installed. Please install it first from https://rustup.rs${NC}"
    exit 1
else
    echo -e "✓ Rust/Cargo found: $(cargo --version)"
fi

if ! command -v cloudflared &> /dev/null; then
    echo -e "${YELLOW}Warning: 'cloudflared' was not found in your PATH.${NC}"
    echo -e "         cloudflared is required to expose your social feed externally."
    echo -e "         You can install it later using: brew install cloudflare/cloudflare/cloudflared"
else
    echo -e "✓ cloudflared found: $(cloudflared --version | head -n 1)"
fi

# 2. Build release binary
echo -e "\n${BLUE}[2/4] Building release binary...${NC}"
cargo build --release

# 3. Create config directory
echo -e "\n${BLUE}[3/4] Initializing configuration directory...${NC}"
mkdir -p "$HOME/.murmur"
chmod 700 "$HOME/.murmur"
echo -e "✓ Configuration directory created at ~/.murmur"

# 4. Install binary to user PATH
echo -e "\n${BLUE}[4/4] Installing binary...${NC}"
INSTALL_DIR="/usr/local/bin"

if [ -w "$INSTALL_DIR" ]; then
    cp target/release/murmur "$INSTALL_DIR/murmur"
    echo -e "${GREEN}✓ Success! Installed murmur to $INSTALL_DIR/murmur${NC}"
else
    # Try ~/.local/bin as a fallback
    LOCAL_BIN="$HOME/.local/bin"
    mkdir -p "$LOCAL_BIN"
    cp target/release/murmur "$LOCAL_BIN/murmur"
    echo -e "${GREEN}✓ Success! Installed murmur to $LOCAL_BIN/murmur${NC}"
    
    # Check if local bin is in PATH
    if [[ ":$PATH:" != *":$LOCAL_BIN:"* ]]; then
        echo -e "${YELLOW}Note: Make sure $LOCAL_BIN is in your PATH. Add this line to your ~/.zshrc or ~/.bash_profile:${NC}"
        echo -e "      export PATH=\"\$PATH:$LOCAL_BIN\""
    fi
fi

echo -e "\n${GREEN}=== murmur installation complete! ===${NC}"
echo -e "Type 'murmur' or 'murmur --ephemeral' to start."
