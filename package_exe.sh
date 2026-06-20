#!/bin/bash
set -e

# Colors for output
GREEN='\033[0;32m'
BLUE='\033[0;34m'
YELLOW='\033[1;33m'
RED='\033[0;31m'
NC='\033[0m' # No Color

echo -e "${BLUE}=== murmur Windows cross-compiler ===${NC}"

# 1. Check/Install target
echo -e "\n${BLUE}[1/4] Adding Rust Windows target...${NC}"
rustup target add x86_64-pc-windows-gnu

# 2. Check for mingw-w64 toolchain
echo -e "\n${BLUE}[2/4] Checking cross-compilation toolchain...${NC}"
if ! command -v x86_64-w64-mingw32-gcc &> /dev/null; then
    echo -e "${YELLOW}Warning: mingw-w64 was not found on your system.${NC}"
    if command -v brew &> /dev/null; then
        echo -e "Installing mingw-w64 using Homebrew. This may take a few minutes..."
        brew install mingw-w64
    else
        echo -e "${RED}Error: Homebrew is not installed. Please install Homebrew and run: brew install mingw-w64${NC}"
        exit 1
    fi
fi

# 3. Create .cargo/config.toml if it doesn't exist
echo -e "\n${BLUE}[3/4] Configuring Cargo linker...${NC}"
mkdir -p .cargo
cat << 'EOF' > .cargo/config.toml
[target.x86_64-pc-windows-gnu]
linker = "x86_64-w64-mingw32-gcc"
ar = "x86_64-w64-mingw32-ar"
EOF
echo -e "✓ Configured .cargo/config.toml"

# 4. Build Windows binary
echo -e "\n${BLUE}[4/4] Compiling murmur.exe in release mode...${NC}"
cargo build --release --target x86_64-pc-windows-gnu

echo -e "\n${GREEN}=== murmur Windows compilation complete! ===${NC}"
echo -e "Generated: ${YELLOW}target/x86_64-pc-windows-gnu/release/murmur.exe${NC}"
