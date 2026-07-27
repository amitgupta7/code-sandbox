SHELL := /bin/bash

# Apple ships GNU make 3.81, which doesn't reliably export a reassigned PATH to
# recipe shells. So instead of relying on `export PATH`, prepend cargo's bin dir
# inline. If cargo came from Homebrew instead of rustup, ~/.cargo/bin simply
# won't contain it and the rest of $PATH (which has Homebrew) is used.
CARGO := PATH="$$HOME/.cargo/bin:$$PATH" cargo
HAVE_CARGO := command -v cargo >/dev/null 2>&1 || [ -x "$$HOME/.cargo/bin/cargo" ]

CARGO_FEATURES := --features typescript
BIN := code-sandbox

.DEFAULT_GOAL := help

.PHONY: help init tools runtimes build release run test fmt clippy clean

help: ## Show available targets
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) \
		| awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[36m%-12s\033[0m %s\n", $$1, $$2}'

init: tools runtimes build ## Full dev-env setup on macOS (tools + runtimes + build)
	@echo ""
	@echo "==> Ready. Start the server with:  make run"

tools: ## Install host toolchain on macOS (Homebrew, Rust, cmake)
	@echo "==> Checking host toolchain..."
	@if [ "$$(uname -s)" != "Darwin" ]; then \
		echo "   'make tools' targets macOS; on Linux install rust + cmake via your package manager."; \
	fi
	@command -v brew >/dev/null 2>&1 || { \
		echo "!! Homebrew not found. Install it from https://brew.sh, then re-run 'make init'."; exit 1; }
	@$(HAVE_CARGO) || { \
		echo "==> Installing Rust via rustup..."; \
		curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable --profile minimal; }
	@command -v cmake >/dev/null 2>&1 || { echo "==> Installing cmake..."; brew install cmake; }
	@$(CARGO) --version && cmake --version | head -1

runtimes: ## Fetch/build the wasm runtime modules (python.wasm, qjs.wasm)
	@./scripts/setup-runtimes.sh

py-deps: ## Install pure-Python packages into the mounted dir. Usage: make py-deps PKG="humanize jinja2"
	@test -n "$(PKG)" || { echo "usage: make py-deps PKG=\"pkg1 pkg2\""; exit 1; }
	python3 -m pip install --target runtimes/py-site-packages --only-binary :all: $(PKG)
	@echo "==> Installed into runtimes/py-site-packages: $(PKG)"

build: ## Debug build (with TypeScript support)
	$(CARGO) build $(CARGO_FEATURES)

release: ## Optimized release build (with TypeScript support)
	$(CARGO) build --release $(CARGO_FEATURES)

run: build ## Build and run the server (BIND overridable, default 127.0.0.1:8080)
	$(CARGO) run $(CARGO_FEATURES)

console: build ## Run with the browser playground enabled at /console (dev only)
	@echo "==> Playground: http://127.0.0.1:8080/console"
	CONSOLE=1 $(CARGO) run $(CARGO_FEATURES)

stop: ## Stop a running server (matches the exact binary name, not this recipe)
	@pkill -x $(BIN) && echo "stopped $(BIN)" || echo "no running $(BIN)"

test: ## Run the test suite
	$(CARGO) test $(CARGO_FEATURES)

fmt: ## Format the code
	$(CARGO) fmt

clippy: ## Lint with clippy
	$(CARGO) clippy $(CARGO_FEATURES) -- -D warnings

clean: ## Remove build artifacts (keeps downloaded runtimes)
	$(CARGO) clean
