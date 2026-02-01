.PHONY: up install build lint fmt test stop start restart logs errors status install-service

# Detect OS
UNAME_S := $(shell uname -s)
IS_WSL := $(shell if [ -f /proc/version ] && grep -qi microsoft /proc/version; then echo 1; else echo 0; fi)

# Service configuration
SERVICE_NAME = claude-telegram-bot
MACOS_PLIST = ~/Library/LaunchAgents/com.claude-telegram-ts.plist
SYSTEMD_SERVICE = ~/.config/systemd/user/$(SERVICE_NAME).service

# make up: Full deployment pipeline
up: install build
	@echo "✅ Deployment complete (service management disabled - use 'bun run start')"

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

# Stop service
stop:
	@echo "🛑 Stopping service..."
	@if [ "$(UNAME_S)" = "Darwin" ]; then \
		if [ -f $(MACOS_PLIST) ]; then \
			launchctl unload $(MACOS_PLIST) 2>/dev/null || true; \
			echo "   macOS service stopped"; \
		else \
			echo "   Service not installed"; \
		fi \
	elif [ "$(IS_WSL)" = "1" ]; then \
		if systemctl --user is-enabled $(SERVICE_NAME) >/dev/null 2>&1; then \
			systemctl --user stop $(SERVICE_NAME); \
			echo "   WSL systemd service stopped"; \
		else \
			echo "   Service not installed"; \
		fi \
	else \
		echo "⚠️  Unsupported platform (use macOS or WSL)"; \
	fi

# Start service
start:
	@echo "🚀 Starting service..."
	@if [ "$(UNAME_S)" = "Darwin" ]; then \
		if [ -f $(MACOS_PLIST) ]; then \
			launchctl load $(MACOS_PLIST); \
			sleep 1; \
			launchctl list | grep com.claude-telegram-ts && echo "   macOS service running" || echo "   ⚠️  Service failed to start"; \
		else \
			echo "   ⚠️  Service not installed. Run 'make install-service' first"; \
		fi \
	elif [ "$(IS_WSL)" = "1" ]; then \
		if systemctl --user is-enabled $(SERVICE_NAME) >/dev/null 2>&1; then \
			systemctl --user start $(SERVICE_NAME); \
			sleep 1; \
			systemctl --user is-active $(SERVICE_NAME) && echo "   WSL systemd service running" || echo "   ⚠️  Service failed to start"; \
		else \
			echo "   ⚠️  Service not installed. Run 'make install-service' first"; \
		fi \
	else \
		echo "⚠️  Unsupported platform (use macOS or WSL)"; \
	fi

# Restart service
restart: stop start

# Install service (one-time setup)
install-service:
	@echo "📝 Installing service..."
	@if [ "$(UNAME_S)" = "Darwin" ]; then \
		echo "macOS: Please manually configure launchagent/com.claude-telegram-ts.plist.template"; \
		echo "       Then copy it to ~/Library/LaunchAgents/com.claude-telegram-ts.plist"; \
	elif [ "$(IS_WSL)" = "1" ]; then \
		mkdir -p ~/.config/systemd/user; \
		echo "[Unit]" > $(SYSTEMD_SERVICE); \
		echo "Description=Claude Telegram Bot" >> $(SYSTEMD_SERVICE); \
		echo "After=network.target" >> $(SYSTEMD_SERVICE); \
		echo "" >> $(SYSTEMD_SERVICE); \
		echo "[Service]" >> $(SYSTEMD_SERVICE); \
		echo "Type=simple" >> $(SYSTEMD_SERVICE); \
		echo "WorkingDirectory=$(shell pwd)" >> $(SYSTEMD_SERVICE); \
		echo "ExecStart=$(shell which bun) run src/index.ts" >> $(SYSTEMD_SERVICE); \
		echo "Restart=always" >> $(SYSTEMD_SERVICE); \
		echo "RestartSec=10" >> $(SYSTEMD_SERVICE); \
		echo "StandardOutput=append:/tmp/claude-telegram-bot.log" >> $(SYSTEMD_SERVICE); \
		echo "StandardError=append:/tmp/claude-telegram-bot.err" >> $(SYSTEMD_SERVICE); \
		echo "" >> $(SYSTEMD_SERVICE); \
		echo "[Install]" >> $(SYSTEMD_SERVICE); \
		echo "WantedBy=default.target" >> $(SYSTEMD_SERVICE); \
		systemctl --user daemon-reload; \
		systemctl --user enable $(SERVICE_NAME); \
		echo "✅ WSL systemd service installed"; \
		echo "   Start with: make start"; \
	else \
		echo "⚠️  Unsupported platform"; \
	fi

# View logs
logs:
	@echo "📋 Service logs:"
	@tail -f /tmp/claude-telegram-bot.log

# View error logs
errors:
	@echo "❌ Error logs:"
	@tail -f /tmp/claude-telegram-bot.err

# Service status
status:
	@echo "📊 Service status:"
	@if [ "$(UNAME_S)" = "Darwin" ]; then \
		launchctl list | grep com.claude-telegram-ts || echo "Service not running"; \
	elif [ "$(IS_WSL)" = "1" ]; then \
		systemctl --user status $(SERVICE_NAME) --no-pager || echo "Service not running"; \
	else \
		echo "⚠️  Unsupported platform"; \
	fi
