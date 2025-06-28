#!/bin/bash

# NHI Setup Script
# This script builds NHI and ensures CRIU is available

set -e

echo "🚀 Setting up NHI (Node Host Infrastructure)"
echo "=============================================="

# Check if we're in the right directory
if [ ! -f "Cargo.toml" ]; then
    echo "❌ Error: Please run this script from the NHI project root directory"
    exit 1
fi

# Check if Rust is installed
if ! command -v cargo &> /dev/null; then
    echo "❌ Error: Rust is not installed. Please install Rust first:"
    echo "   curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"
    exit 1
fi

# Check if CRIU binary exists
if [ ! -f "criu/bin/criu" ]; then
    echo "❌ Error: CRIU binary not found at criu/bin/criu"
    echo "   This should be included in the repository."
    exit 1
fi

# Make CRIU executable
chmod +x criu/bin/criu

# Check CRIU version
echo "📋 Checking CRIU version..."
./criu/bin/criu --version

# Build NHI
echo "🔨 Building NHI..."
cargo build --release

# Check if build was successful
if [ ! -f "target/release/nhi" ]; then
    echo "❌ Error: Build failed"
    exit 1
fi

echo "✅ Setup complete!"
echo ""
echo "🎯 Quick Start:"
echo "   sudo ./target/release/nhi"
echo ""
echo "📖 For more information, see README.md"
