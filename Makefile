.PHONY: up up-force preflight install build lint fmt test stop start restart logs errors status install-service uninstall-service reinstall-service

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
