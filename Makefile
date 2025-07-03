# NHI Project Makefile

# Compiler settings
CC = gcc
CFLAGS = -Wall -Wextra -O2
TARGET_DIR = target
DAEMON_WRAPPER = $(TARGET_DIR)/daemon_wrapper

# Default target
all: $(DAEMON_WRAPPER)

# Create target directory
$(TARGET_DIR):
	mkdir -p $(TARGET_DIR)

# Build daemon wrapper
$(DAEMON_WRAPPER): src/daemon_wrapper.c | $(TARGET_DIR)
	$(CC) $(CFLAGS) -o $@ $<
	@echo "Built daemon wrapper: $@"

# Build Rust project
rust-build:
	cargo build --release

# Build everything
build-all: $(DAEMON_WRAPPER) rust-build

# Clean build artifacts
clean:
	rm -rf $(TARGET_DIR)
	cargo clean

# Install daemon wrapper to system path (optional)
install: $(DAEMON_WRAPPER)
	sudo cp $(DAEMON_WRAPPER) /usr/local/bin/
	@echo "Installed daemon_wrapper to /usr/local/bin/"

# Test daemon wrapper
test-daemon: $(DAEMON_WRAPPER)
	@echo "Testing daemon wrapper..."
	./$(DAEMON_WRAPPER) /tmp/test_daemon.log /bin/sleep 5 &
	@sleep 1
	@echo "Checking if daemon process is running..."
	@ps aux | grep -v grep | grep sleep || echo "No sleep process found"
	@echo "Output file content:"
	@cat /tmp/test_daemon.log 2>/dev/null || echo "No output file found"

.PHONY: all rust-build build-all clean install test-daemon
