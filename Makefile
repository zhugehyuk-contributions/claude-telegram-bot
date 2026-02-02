.PHONY: up up-force preflight install build lint fmt test stop start restart logs errors status install-service uninstall-service reinstall-service \
	up-rust build-rust test-rust stop-rust start-rust restart-rust logs-rust errors-rust status-rust \
	install-service-rust uninstall-service-rust reinstall-service-rust

# Detect OS
UNAME_S := $(shell uname -s)
IS_WSL := $(shell if [ -f /proc/version ] && grep -qi microsoft /proc/version; then echo 1; else echo 0; fi)

# Service configuration - reads SERVICE_NAME from .env or uses directory name
-include .env
SERVICE_NAME ?= $(notdir $(shell pwd))
MACOS_PLIST = ~/Library/LaunchAgents/com.$(SERVICE_NAME).plist
SYSTEMD_SERVICE = ~/.config/systemd/user/$(SERVICE_NAME).service
PIDFILE = /tmp/$(SERVICE_NAME).pid
LOGFILE = /tmp/$(SERVICE_NAME).log
ERRFILE = /tmp/$(SERVICE_NAME).err
BUN_PATH = $(shell which bun)

# Rust build/run configuration
RUST_DIR = rust
RUST_BIN_NAME ?= ctb
RUST_BIN = $(RUST_DIR)/target/release/$(RUST_BIN_NAME)
RUST_MCP_BIN = $(RUST_DIR)/target/release/ctb-ask-user-mcp

# Keep TS and Rust services separate by default.
RUST_SERVICE_NAME ?= $(SERVICE_NAME)-rs
RUST_MACOS_PLIST = ~/Library/LaunchAgents/com.$(RUST_SERVICE_NAME).plist
RUST_SYSTEMD_SERVICE = ~/.config/systemd/user/$(RUST_SERVICE_NAME).service
RUST_PIDFILE = /tmp/$(RUST_SERVICE_NAME).pid
RUST_LOGFILE = /tmp/$(RUST_SERVICE_NAME).log
RUST_ERRFILE = /tmp/$(RUST_SERVICE_NAME).err

# WSL systemd requires DBUS session bus
SYSTEMCTL := $(if $(filter 1,$(IS_WSL)),DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/$$(id -u)/bus systemctl --user,systemctl --user)

# Preflight checks - must pass before deployment
preflight:
	@echo "🔍 Running preflight checks..."
	@bun run typecheck || (echo "❌ Typecheck failed. Run: bun run typecheck" && exit 1)
	@bun run lint:check || (echo "❌ Lint errors found. Run: bun run lint:check" && exit 1)
	@echo "✅ Preflight passed"

# Full deployment pipeline with preflight (reinstalls service on WSL)
up: install build preflight
	@echo "🔄 Deploying..."
	@if [ "$(UNAME_S)" = "Darwin" ]; then \
		if [ -f $(MACOS_PLIST) ]; then \
			$(MAKE) restart; \
			echo "✅ Deployment complete - macOS service restarted"; \
		else \
			echo "⚠️  macOS: Run 'make install-service' first"; \
		fi \
	elif [ "$(IS_WSL)" = "1" ]; then \
		echo "   Updating service file..."; \
		$(SYSTEMCTL) unmask $(SERVICE_NAME) 2>/dev/null || true; \
		mkdir -p ~/.config/systemd/user; \
		printf '[Unit]\nDescription=$(SERVICE_NAME)\nAfter=network.target\n\n[Service]\nType=simple\nWorkingDirectory=%s\nExecStart=%s run start\nRestart=always\nRestartSec=10\nEnvironment=PATH=%s:/usr/local/bin:/usr/bin:/bin\nStandardOutput=append:$(LOGFILE)\nStandardError=append:$(ERRFILE)\n\n[Install]\nWantedBy=default.target\n' "$(shell pwd)" "$(BUN_PATH)" "$(dir $(BUN_PATH))" > $(SYSTEMD_SERVICE); \
		$(SYSTEMCTL) daemon-reload; \
		$(SYSTEMCTL) enable $(SERVICE_NAME) 2>/dev/null || true; \
		echo "   Restarting service (will kill current process)..."; \
		$(SYSTEMCTL) restart $(SERVICE_NAME); \
		echo "✅ Deployment complete"; \
	else \
		echo "⚠️  Unsupported platform"; \
	fi

# Emergency deployment without preflight (use with caution)
up-force: install build
	@echo "⚠️  Skipping preflight checks (emergency mode)..."
	@echo "🔄 Deploying..."
	@if [ "$(UNAME_S)" = "Darwin" ]; then \
		if [ -f $(MACOS_PLIST) ]; then \
			$(MAKE) restart; \
			echo "✅ Deployment complete - macOS service restarted"; \
		else \
			echo "⚠️  macOS: Run 'make install-service' first"; \
		fi \
	elif [ "$(IS_WSL)" = "1" ]; then \
		echo "   Updating service file..."; \
		$(SYSTEMCTL) unmask $(SERVICE_NAME) 2>/dev/null || true; \
		mkdir -p ~/.config/systemd/user; \
		printf '[Unit]\nDescription=$(SERVICE_NAME)\nAfter=network.target\n\n[Service]\nType=simple\nWorkingDirectory=%s\nExecStart=%s run start\nRestart=always\nRestartSec=10\nEnvironment=PATH=%s:/usr/local/bin:/usr/bin:/bin\nStandardOutput=append:$(LOGFILE)\nStandardError=append:$(ERRFILE)\n\n[Install]\nWantedBy=default.target\n' "$(shell pwd)" "$(BUN_PATH)" "$(dir $(BUN_PATH))" > $(SYSTEMD_SERVICE); \
		$(SYSTEMCTL) daemon-reload; \
		$(SYSTEMCTL) enable $(SERVICE_NAME) 2>/dev/null || true; \
		echo "   Restarting service (will kill current process)..."; \
		$(SYSTEMCTL) restart $(SERVICE_NAME); \
		echo "✅ Deployment complete"; \
	else \
		echo "⚠️  Unsupported platform"; \
	fi

# Install dependencies
install:
	@echo "📦 Installing dependencies..."
	bun install

# Build/typecheck
build:
	@echo "🔨 Type checking..."
	bun run typecheck

# Lint code
lint:
	@echo "🔍 Linting code..."
	@if [ -f node_modules/.bin/eslint ]; then \
		bun run lint; \
	else \
		echo "⚠️  ESLint not installed, skipping..."; \
	fi

# Format code
fmt:
	@echo "🎨 Formatting code..."
	@if [ -f node_modules/.bin/prettier ]; then \
		bun run fmt; \
	else \
		echo "⚠️  Prettier not installed, skipping..."; \
	fi

# Run tests
test:
	@echo "🧪 Running tests..."
	@if [ -d src/__tests__ ] || [ -f src/**/*.test.ts ]; then \
		bun test; \
	else \
		echo "⚠️  No tests found, skipping..."; \
	fi

# Stop service or process
stop:
	@echo "🛑 Stopping..."
	@if [ "$(UNAME_S)" = "Darwin" ] && [ -f $(MACOS_PLIST) ]; then \
		launchctl unload $(MACOS_PLIST) 2>/dev/null || true; \
		echo "   macOS service stopped"; \
	elif [ "$(IS_WSL)" = "1" ] && $(SYSTEMCTL) is-active $(SERVICE_NAME) >/dev/null 2>&1; then \
		$(SYSTEMCTL) stop $(SERVICE_NAME); \
		echo "   systemd service stopped"; \
	elif [ -f $(PIDFILE) ]; then \
		kill $$(cat $(PIDFILE)) 2>/dev/null && echo "   Process stopped" || echo "   Process already stopped"; \
		rm -f $(PIDFILE); \
	else \
		echo "   Nothing running"; \
	fi

# Start service or process
start:
	@echo "🚀 Starting..."
	@if [ "$(UNAME_S)" = "Darwin" ] && [ -f $(MACOS_PLIST) ]; then \
		launchctl load $(MACOS_PLIST); sleep 1; \
		launchctl list | grep com.claude-telegram-ts && echo "   macOS service running" || echo "   ⚠️  Failed to start"; \
	elif [ "$(IS_WSL)" = "1" ] && $(SYSTEMCTL) is-enabled $(SERVICE_NAME) >/dev/null 2>&1; then \
		$(SYSTEMCTL) start $(SERVICE_NAME); sleep 1; \
		$(SYSTEMCTL) is-active $(SERVICE_NAME) && echo "   systemd service running" || echo "   ⚠️  Failed to start"; \
	else \
		nohup bun run src/index.ts >$(LOGFILE) 2>&1 & \
		echo $$! > $(PIDFILE); \
		sleep 1; \
		if kill -0 $$(cat $(PIDFILE)) 2>/dev/null; then \
			echo "   Bot running (PID: $$(cat $(PIDFILE)))"; \
		else \
			echo "   ⚠️  Failed to start"; \
			rm -f $(PIDFILE); \
		fi \
	fi

# Restart service (includes graceful shutdown delay)
restart: stop
	@sleep 2 && $(MAKE) start

# Install service (one-time setup)
install-service:
	@echo "📝 Installing service..."
	@if [ "$(UNAME_S)" = "Darwin" ]; then \
		echo "macOS: Please manually configure launchagent/com.claude-telegram-ts.plist.template"; \
		echo "       Then copy it to ~/Library/LaunchAgents/com.claude-telegram-ts.plist"; \
	elif [ "$(IS_WSL)" = "1" ]; then \
		mkdir -p ~/.config/systemd/user; \
		printf '[Unit]\nDescription=$(SERVICE_NAME)\nAfter=network.target\n\n[Service]\nType=simple\nWorkingDirectory=%s\nExecStart=%s run start\nRestart=always\nRestartSec=10\nEnvironment=PATH=%s:/usr/local/bin:/usr/bin:/bin\nStandardOutput=append:$(LOGFILE)\nStandardError=append:$(ERRFILE)\n\n[Install]\nWantedBy=default.target\n' "$(shell pwd)" "$(BUN_PATH)" "$(dir $(BUN_PATH))" > $(SYSTEMD_SERVICE); \
		$(SYSTEMCTL) daemon-reload; \
		$(SYSTEMCTL) enable $(SERVICE_NAME); \
		echo "✅ WSL systemd service installed ($(SERVICE_NAME))"; \
		echo "   Start with: make start"; \
	else \
		echo "⚠️  Unsupported platform"; \
	fi

# Reinstall service (uninstall + install + start)
reinstall-service: uninstall-service install-service start
	@echo "✅ Service reinstalled and started"

# Uninstall service (complete removal)
uninstall-service:
	@echo "🗑️  Uninstalling service..."
	@if [ "$(UNAME_S)" = "Darwin" ]; then \
		if [ -f $(MACOS_PLIST) ]; then \
			launchctl unload $(MACOS_PLIST) 2>/dev/null || true; \
			rm -f $(MACOS_PLIST); \
			echo "✅ macOS service removed"; \
		else \
			echo "   Service not installed"; \
		fi \
	elif [ "$(IS_WSL)" = "1" ]; then \
		$(SYSTEMCTL) stop $(SERVICE_NAME) 2>/dev/null || true; \
		$(SYSTEMCTL) disable $(SERVICE_NAME) 2>/dev/null || true; \
		$(SYSTEMCTL) unmask $(SERVICE_NAME) 2>/dev/null || true; \
		rm -f $(SYSTEMD_SERVICE); \
		$(SYSTEMCTL) daemon-reload; \
		echo "✅ WSL systemd service removed"; \
	else \
		echo "⚠️  Unsupported platform"; \
	fi

# View logs
logs:
	@echo "📋 Service logs:"
	@tail -f $(LOGFILE)

# View error logs
errors:
	@echo "❌ Error logs:"
	@tail -f $(ERRFILE)

# Service/process status
status:
	@echo "📊 Status:"
	@if [ "$(UNAME_S)" = "Darwin" ] && [ -f $(MACOS_PLIST) ]; then \
		launchctl list | grep com.claude-telegram-ts || echo "   macOS service not running"; \
	elif [ "$(IS_WSL)" = "1" ] && $(SYSTEMCTL) is-enabled $(SERVICE_NAME) >/dev/null 2>&1; then \
		$(SYSTEMCTL) status $(SERVICE_NAME) --no-pager || echo "   systemd service not running"; \
	elif [ -f $(PIDFILE) ] && kill -0 $$(cat $(PIDFILE)) 2>/dev/null; then \
		PID=$$(cat $(PIDFILE)); \
		echo "   Bot running (PID: $$PID, dev mode)"; \
		ps -p $$PID -o pid,etime,rss,args --no-headers 2>/dev/null || true; \
	else \
		rm -f $(PIDFILE) 2>/dev/null; \
		echo "   Not running"; \
	fi

# ==============================================================================
# Rust Port Targets
# ==============================================================================

# Build Rust binaries (release)
build-rust:
	@echo "🦀 Building Rust (release)..."
	@cd $(RUST_DIR) && cargo build -p ctb -p ctb-ask-user-mcp --release
	@if [ -f $(RUST_BIN) ]; then echo "   ✅ Built $(RUST_BIN)"; else echo "   ❌ Missing $(RUST_BIN)"; exit 1; fi
	@if [ -f $(RUST_MCP_BIN) ]; then echo "   ✅ Built $(RUST_MCP_BIN)"; else echo "   ❌ Missing $(RUST_MCP_BIN)"; exit 1; fi

# Run Rust tests
test-rust:
	@echo "🧪 Testing Rust..."
	@cd $(RUST_DIR) && cargo test --workspace

# Full Rust deploy pipeline
up-rust: build-rust
	@echo "🔄 Deploying Rust..."
	@if [ "$(UNAME_S)" = "Darwin" ]; then \
		if [ -f $(RUST_MACOS_PLIST) ]; then \
			$(MAKE) restart-rust; \
			echo "✅ Rust deployment complete - macOS service restarted"; \
		else \
			echo "⚠️  macOS: Run 'make install-service-rust' first (or install the plist manually)"; \
		fi \
	elif [ "$(IS_WSL)" = "1" ]; then \
		echo "   Updating Rust systemd service file..."; \
		$(SYSTEMCTL) unmask $(RUST_SERVICE_NAME) 2>/dev/null || true; \
		mkdir -p ~/.config/systemd/user; \
		printf '[Unit]\nDescription=$(RUST_SERVICE_NAME)\nAfter=network.target\n\n[Service]\nType=simple\nWorkingDirectory=%s\nExecStart=%s\nRestart=always\nRestartSec=10\nEnvironment=PATH=/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin\nStandardOutput=append:$(RUST_LOGFILE)\nStandardError=append:$(RUST_ERRFILE)\n\n[Install]\nWantedBy=default.target\n' "$(shell pwd)" "$(shell pwd)/$(RUST_BIN)" > $(RUST_SYSTEMD_SERVICE); \
		$(SYSTEMCTL) daemon-reload; \
		$(SYSTEMCTL) enable $(RUST_SERVICE_NAME) 2>/dev/null || true; \
		echo "   Restarting Rust service..."; \
		$(SYSTEMCTL) restart $(RUST_SERVICE_NAME); \
		echo "✅ Rust deployment complete"; \
	else \
		echo "⚠️  Unsupported platform"; \
	fi

# Stop Rust service or process
stop-rust:
	@echo "🛑 Stopping Rust..."
	@if [ "$(UNAME_S)" = "Darwin" ] && [ -f $(RUST_MACOS_PLIST) ]; then \
		launchctl unload $(RUST_MACOS_PLIST) 2>/dev/null || true; \
		echo "   macOS Rust service stopped"; \
	elif [ "$(IS_WSL)" = "1" ] && $(SYSTEMCTL) is-active $(RUST_SERVICE_NAME) >/dev/null 2>&1; then \
		$(SYSTEMCTL) stop $(RUST_SERVICE_NAME); \
		echo "   systemd Rust service stopped"; \
	elif [ -f $(RUST_PIDFILE) ]; then \
		kill $$(cat $(RUST_PIDFILE)) 2>/dev/null && echo "   Rust process stopped" || echo "   Rust process already stopped"; \
		rm -f $(RUST_PIDFILE); \
	else \
		echo "   Nothing running"; \
	fi

# Start Rust service or process
start-rust:
	@echo "🚀 Starting Rust..."
	@if [ ! -f $(RUST_BIN) ]; then \
		echo "   ❌ Missing $(RUST_BIN). Run: make build-rust"; \
		exit 1; \
	fi
	@if [ "$(UNAME_S)" = "Darwin" ] && [ -f $(RUST_MACOS_PLIST) ]; then \
		launchctl load $(RUST_MACOS_PLIST); sleep 1; \
		launchctl list | grep com.$(RUST_SERVICE_NAME) && echo "   macOS Rust service running" || echo "   ⚠️  Failed to start"; \
	elif [ "$(IS_WSL)" = "1" ] && $(SYSTEMCTL) is-enabled $(RUST_SERVICE_NAME) >/dev/null 2>&1; then \
		$(SYSTEMCTL) start $(RUST_SERVICE_NAME); sleep 1; \
		$(SYSTEMCTL) is-active $(RUST_SERVICE_NAME) && echo "   systemd Rust service running" || echo "   ⚠️  Failed to start"; \
	else \
		nohup $(RUST_BIN) >$(RUST_LOGFILE) 2>$(RUST_ERRFILE) & \
		echo $$! > $(RUST_PIDFILE); \
		sleep 1; \
		if kill -0 $$(cat $(RUST_PIDFILE)) 2>/dev/null; then \
			echo "   Rust bot running (PID: $$(cat $(RUST_PIDFILE)))"; \
		else \
			echo "   ⚠️  Failed to start"; \
			rm -f $(RUST_PIDFILE); \
		fi \
	fi

# Restart Rust service
restart-rust: stop-rust
	@sleep 2 && $(MAKE) start-rust

# Install Rust service (one-time setup)
install-service-rust:
	@echo "📝 Installing Rust service..."
	@if [ "$(UNAME_S)" = "Darwin" ]; then \
		echo "macOS: Use launchagent/com.claude-telegram-rs.plist.template as a starting point."; \
		echo "       Copy it to: $(RUST_MACOS_PLIST) (and edit paths/env as needed)"; \
	elif [ "$(IS_WSL)" = "1" ]; then \
		mkdir -p ~/.config/systemd/user; \
		printf '[Unit]\nDescription=$(RUST_SERVICE_NAME)\nAfter=network.target\n\n[Service]\nType=simple\nWorkingDirectory=%s\nExecStart=%s\nRestart=always\nRestartSec=10\nEnvironment=PATH=/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin\nStandardOutput=append:$(RUST_LOGFILE)\nStandardError=append:$(RUST_ERRFILE)\n\n[Install]\nWantedBy=default.target\n' "$(shell pwd)" "$(shell pwd)/$(RUST_BIN)" > $(RUST_SYSTEMD_SERVICE); \
		$(SYSTEMCTL) daemon-reload; \
		$(SYSTEMCTL) enable $(RUST_SERVICE_NAME); \
		echo "✅ WSL systemd Rust service installed ($(RUST_SERVICE_NAME))"; \
		echo "   Start with: make start-rust"; \
	else \
		echo "⚠️  Unsupported platform"; \
	fi

# Reinstall Rust service
reinstall-service-rust: uninstall-service-rust install-service-rust start-rust
	@echo "✅ Rust service reinstalled and started"

# Uninstall Rust service
uninstall-service-rust:
	@echo "🗑️  Uninstalling Rust service..."
	@if [ "$(UNAME_S)" = "Darwin" ]; then \
		if [ -f $(RUST_MACOS_PLIST) ]; then \
			launchctl unload $(RUST_MACOS_PLIST) 2>/dev/null || true; \
			rm -f $(RUST_MACOS_PLIST); \
			echo "✅ macOS Rust service removed"; \
		else \
			echo "   Rust service not installed"; \
		fi \
	elif [ "$(IS_WSL)" = "1" ]; then \
		$(SYSTEMCTL) stop $(RUST_SERVICE_NAME) 2>/dev/null || true; \
		$(SYSTEMCTL) disable $(RUST_SERVICE_NAME) 2>/dev/null || true; \
		$(SYSTEMCTL) unmask $(RUST_SERVICE_NAME) 2>/dev/null || true; \
		rm -f $(RUST_SYSTEMD_SERVICE); \
		$(SYSTEMCTL) daemon-reload; \
		echo "✅ WSL systemd Rust service removed"; \
	else \
		echo "⚠️  Unsupported platform"; \
	fi

# View Rust logs
logs-rust:
	@echo "📋 Rust logs:"
	@tail -f $(RUST_LOGFILE)

# View Rust error logs
errors-rust:
	@echo "❌ Rust error logs:"
	@tail -f $(RUST_ERRFILE)

# Rust status
status-rust:
	@echo "📊 Rust status:"
	@if [ "$(UNAME_S)" = "Darwin" ] && [ -f $(RUST_MACOS_PLIST) ]; then \
		launchctl list | grep com.$(RUST_SERVICE_NAME) || echo "   macOS Rust service not running"; \
	elif [ "$(IS_WSL)" = "1" ] && $(SYSTEMCTL) is-enabled $(RUST_SERVICE_NAME) >/dev/null 2>&1; then \
		$(SYSTEMCTL) status $(RUST_SERVICE_NAME) --no-pager || echo "   systemd Rust service not running"; \
	elif [ -f $(RUST_PIDFILE) ] && kill -0 $$(cat $(RUST_PIDFILE)) 2>/dev/null; then \
		PID=$$(cat $(RUST_PIDFILE)); \
		echo "   Rust bot running (PID: $$PID, dev mode)"; \
		ps -p $$PID -o pid,etime,rss,args --no-headers 2>/dev/null || true; \
	else \
		rm -f $(RUST_PIDFILE) 2>/dev/null; \
		echo "   Not running"; \
	fi
