# Usage: make help

COMPOSE = docker compose
CONTAINER = desktop
BROWSER_URL = http://localhost:6080/vnc.html

.DEFAULT_GOAL := help

.PHONY: help build up down reset shell logs status open rebuild test

help: ## Show available commands
	@echo ""
	@echo "  🧪 CVC Extension Test Environment"
	@echo ""
	@awk 'BEGIN {FS = ":.*##"} /^[a-zA-Z_-]+:.*##/ { printf "  \033[36m%-12s\033[0m %s\n", $$1, $$2 }' $(MAKEFILE_LIST)
	@echo ""

build: ## Build the Docker image (run after Dockerfile changes)
	$(COMPOSE) build

up: ## Start the environment (clean slate)
	$(COMPOSE) up -d
	@echo ""
	@echo "  ✅ Environment ready!"
	@echo "  🌐 Open: $(BROWSER_URL)"
	@echo "  🔑 VNC password: password"
	@echo ""

down: ## Tear down (wipes all VS Code state)
	$(COMPOSE) down
	@echo "  🧹 Environment torn down — next launch will be a clean slate"

reset: down up ## Full reset: tear down and bring back up clean

rebuild: ## Rebuild image from scratch and start clean (use after Dockerfile changes)
	$(COMPOSE) down
	$(COMPOSE) build --no-cache
	$(COMPOSE) up -d
	@echo "  🔨 Rebuilt and started fresh"

shell: ## Open a bash shell inside the running container
	$(COMPOSE) exec $(CONTAINER) bash

logs: ## Tail container logs
	$(COMPOSE) logs -f $(CONTAINER)

status: ## Show container status
	$(COMPOSE) ps

open: ## Open the noVNC desktop in your default browser
	@open $(BROWSER_URL) 2>/dev/null || xdg-open $(BROWSER_URL) 2>/dev/null || \
		echo "  👉 Open manually: $(BROWSER_URL)"

test: reset ## Alias for reset — clean environment for a fresh test run
	@echo "  🧪 Clean environment up — ready to test!"