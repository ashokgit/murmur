#!/bin/bash
set -e

# Colors for output
GREEN='\033[0;32m'
BLUE='\033[0;34m'
YELLOW='\033[1;33m'
RED='\033[0;31m'
NC='\033[0m' # No Color

echo -e "${BLUE}=== murmur Code Packer ===${NC}"

# Ensure we are in the project root by checking for Cargo.toml
if [ ! -f "Cargo.toml" ]; then
    echo -e "${RED}Error: Cargo.toml not found. Please run this script from the project root.${NC}"
    exit 1
fi

OUTPUT_FILE="packed.txt"

# Clear the output file
> "$OUTPUT_FILE"

echo -e "\n${BLUE}[1/2] Scanning and packing files into $OUTPUT_FILE...${NC}"

# Find and pack code files
# Exclude build targets, git metadata, binary files (png, dmg, gz), logs, DS_Store, and packed.txt itself
find . -type f \
    -not -path "*/target/*" \
    -not -path "*/.git/*" \
    -not -path "*/untitled folder/*" \
    -not -name "packed.txt" \
    -not -name "*.png" \
    -not -name "*.dmg" \
    -not -name "*.tar.gz" \
    -not -name "*.log" \
    -not -name ".DS_Store" \
    | sort | while read -r file; do
        # Clean relative path (remove './')
        rel_path="${file#./}"
        
        # Skip Cargo.lock to keep the package concise (it is auto-generated dependency lock)
        if [[ "$rel_path" == *"Cargo.lock"* ]]; then
            echo -e "  - Skipping $rel_path (generated lock file)"
            continue
        fi
        
        echo -e "  + Packing $rel_path"
        
        # Write file header and contents to packed.txt
        echo "================================================================================" >> "$OUTPUT_FILE"
        echo "FILE: $rel_path" >> "$OUTPUT_FILE"
        echo "================================================================================" >> "$OUTPUT_FILE"
        cat "$file" >> "$OUTPUT_FILE"
        echo -e "\n" >> "$OUTPUT_FILE"
    done

if [ -f "$OUTPUT_FILE" ]; then
    SIZE=$(du -sh "$OUTPUT_FILE" | cut -f1)
    LINE_COUNT=$(wc -l "$OUTPUT_FILE" | awk '{print $1}')
    echo -e "\n${GREEN}✓ Success! Created ${YELLOW}$OUTPUT_FILE${NC} ($SIZE, $LINE_COUNT lines)${NC}"
else
    echo -e "${RED}Error: Failed to create $OUTPUT_FILE${NC}"
    exit 1
fi

echo -e "\n${GREEN}=== murmur Packaging Complete! ===${NC}"
