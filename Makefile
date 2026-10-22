# Shortcuts for working on geko: `make help` lists them. Linux and macOS run it as is; Windows
# through make.cmd, which runs it inside WSL, where geko is built and its data live.

# Run from Windows, make starts without the login profile that puts cargo in the PATH.
export PATH := $(HOME)/.cargo/bin:$(PATH)
export CARGO_TARGET_DIR ?= $(HOME)/.cache/geko-target
# Without a C compiler (WSL without build-essential), Zig compiles and links, if it is there (AGENTS.md).
ifeq ($(shell command -v cc 2>/dev/null),)
ifneq ($(wildcard $(HOME)/.local/bin/zigcc),)
export CC := $(HOME)/.local/bin/zigcc
export AR := $(HOME)/.local/bin/zigar
export CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER := $(HOME)/.local/bin/zigcc
endif
endif
BIN    := $(CARGO_TARGET_DIR)/release/geko
# `make modules`: the modules that run as processes of their own, one directory each, as
# GEKO_MODULES reads them.
MODULES ?= $(CARGO_TARGET_DIR)/modules
MODULE_KIND ?= process
MODULE_NAMES := geocoder tiles
# geko lives in /usr/local/bin, reads /etc/geko/geko.toml and keeps its data in /var/lib/geko.
LISTEN ?= 0.0.0.0:8080
GEKO   := /usr/local/bin/geko

IMAGE    ?= geko
# Where `make release` publishes the image, and the tags it gives it: the version of geko and the
# commit, besides `latest`.
REGISTRY ?= ghcr.io/candymaps/geko
# Without the CR of a checkout made on Windows.
VERSION  := $(shell tr -d '\r' < Cargo.toml | sed -n '/^\[workspace.package\]/,/^\[/s/^version = "\(.*\)"/\1/p')
# The commit, read from .git where git is not installed (WSL compiles a checkout made on Windows).
COMMIT   := $(shell git rev-parse --short HEAD 2>/dev/null || { \
              r=$$(sed -n 's|^ref: ||p' .git/HEAD 2>/dev/null); \
              if [ -z "$$r" ]; then cat .git/HEAD; else cat ".git/$$r" 2>/dev/null || sed -n "s| $$r\$$||p" .git/packed-refs; fi; \
            } | cut -c1-7)
# An image built with uncommitted changes says so in its revision; without git, it cannot tell.
DIRTY    := $(shell command -v git > /dev/null && { git diff --quiet HEAD 2>/dev/null || echo -dirty; })
# The glibc of the base of the image (debian:bookworm-slim): a binary that needs a newer one does
# not start in it.
IMAGE_GLIBC := 2.36
# geko alone, without the modules linked in, as the image carries it.
HOST_BIN := $(CARGO_TARGET_DIR)/host/release/geko
# Fails if the binary $(1) needs a newer glibc than the image has.
define glibc
need=$$(grep -aoE 'GLIBC_[0-9]+\.[0-9]+' $(1) | sed 's/GLIBC_//' | sort -Vu | tail -1); \
[ "$$(printf '%s\n' $$need $(IMAGE_GLIBC) | sort -V | tail -1)" = "$(IMAGE_GLIBC)" ] || { \
  echo "$(notdir $(1)) needs glibc $$need, newer than the $(IMAGE_GLIBC) of the image: it would not start." >&2; \
  echo "Build it on an older system, with Zig (AGENTS.md), or let docker build compile it: docker build -t $(IMAGE) ." >&2; exit 1; };
endef

# `make start-dev` (develop/compose.yaml): its port, its configuration and the modules it builds, by
# default those enabled with `<module> = true` in the [modules] of that configuration.
DEV_PORT    ?= 8080
DEV_CONFIG  ?= develop/geko.toml
DEV_MODULES ?= $(shell awk '/^\[/ { on = ($$1 == "[modules]") } on && $$2 == "=" && $$3 == "true" { print $$1 }' $(DEV_CONFIG) | paste -sd, -)
DEV_COMPOSE := GEKO_DEV_CONFIG="$(abspath $(DEV_CONFIG))" GEKO_DEV_PORT=$(DEV_PORT) GEKO_DEV_MODULES=$(DEV_MODULES) \
               GEKO_DEV_VERSION=$(VERSION)-$(COMMIT)$(DIRTY) docker compose -f develop/compose.yaml

.PHONY: help bin modules install serve up data query parse reverse image start-dev stop-dev release release-packs test test-golden-es test-golden-lu test-regression golden-snapshot golden-snapshot-local

help: ## This list
	@grep -E "^[a-z-]+:.*## " $(MAKEFILE_LIST) | awk -F ":.*## " '{ printf "  make %-10s %s\n", $$1, $$2 }'
	@echo "  Variables: LISTEN=$(LISTEN) IMAGE=$(IMAGE) REGISTRY=$(REGISTRY)"

bin: ## Compile geko in release
	cargo build --release -p geko-app

modules: ## Compile every module (geocoder, tiles) in MODULES, run as MODULE_KIND (process or dylib): GEKO_MODULES=MODULES geko … uses them
	cargo build --release $(foreach m,$(MODULE_NAMES),-p geko-module-$(m) --bin geko-$(m))
	cargo build --release $(foreach m,$(MODULE_NAMES),-p geko-module-$(m)-dylib)
	@for m in $(MODULE_NAMES); do \
	  install -D -m 755 $(CARGO_TARGET_DIR)/release/geko-$$m $(MODULES)/$$m/geko-$$m; \
	  install -m 755 $(CARGO_TARGET_DIR)/release/libgeko_$$m.so $(MODULES)/$$m/libgeko_$$m.so; \
	  if [ "$(MODULE_KIND)" = dylib ]; then \
	    printf 'name = "%s"\nkind = "dylib"\nentry = "libgeko_%s.so"\n' $$m $$m > $(MODULES)/$$m/module.toml; \
	  else \
	    install -m 644 modules/geko-$$m/module.toml $(MODULES)/$$m/module.toml; \
	  fi; \
	done

install: bin ## Install geko, /etc/geko/geko.toml and geko-geocoder.toml (the samples, if they are not) and /var/lib/geko
	cmp -s $(BIN) /usr/local/bin/geko || sudo install -m 755 $(BIN) /usr/local/bin/geko
	[ -f /etc/geko/geko.toml ] || sudo install -D -m 644 develop/geko.toml /etc/geko/geko.toml
	[ -f /etc/geko/geko-geocoder.toml ] || sudo install -D -m 644 modules/geko-geocoder/geko-geocoder.toml /etc/geko/geko-geocoder.toml
	[ -d /var/lib/geko ] || sudo install -d -o $$(id -un) -g $$(id -gn) /var/lib/geko

serve: install ## Serve the published bundle on LISTEN (the demo page is at /geocoder/)
	$(GEKO) serve --listen $(LISTEN)

up: install ## Serve, and build in the background what the configuration asks for (now and every max_age_days)
	$(GEKO) up --listen $(LISTEN)

data: install ## Download and build what the configuration asks for
	$(GEKO) build

query: install ## A search with timings: make query Q="gran via 1, madrid"
	$(GEKO) geocoder query "$(Q)" --debug

parse: install ## The readings of a query: make parse Q="gran via 1, madrid"
	$(GEKO) geocoder parse "$(Q)"

reverse: install ## Reverse geocoding: make reverse LAT=40.4168 LON=-3.7038
	$(GEKO) geocoder reverse $(LAT) $(LON)

image: ## Docker image IMAGE with geko alone, from a local release build (docker build compiles it instead): it downloads its modules
	cargo build --release -p geko-app --no-default-features --target-dir $(CARGO_TARGET_DIR)/host
	@$(call glibc,$(HOST_BIN))
	rm -rf $(CARGO_TARGET_DIR)/image && mkdir -p $(CARGO_TARGET_DIR)/image
	cp $(HOST_BIN) $(CARGO_TARGET_DIR)/image/geko
	BUILDX_GIT_INFO=false docker build --build-context binary=$(CARGO_TARGET_DIR)/image --build-arg VERSION=$(VERSION) --build-arg REVISION=$(COMMIT)$(DIRTY) -t $(IMAGE) .

start-dev: ## Build geko and DEV_MODULES of the working tree inside Docker and (re)create the container of develop/compose.yaml with DEV_CONFIG and the packs of the repository
	@[ -n "$(DEV_MODULES)" ] || { echo "no module enabled in $(DEV_CONFIG): DEV_MODULES=geocoder,…" >&2; exit 1; }
	@docker rm -f geko-dev > /dev/null 2>&1 || true
	$(DEV_COMPOSE) up -d --build
	@echo "geko-dev ($(DEV_MODULES)) builds, then serves http://localhost:$(DEV_PORT)/geocoder/ (demo) and /docs (API). Its log follows; Ctrl-C leaves it running."
	$(DEV_COMPOSE) logs -f

stop-dev: ## Stop the container of make start-dev, keeping its volume (geko-dev_data)
	$(DEV_COMPOSE) down

release: ## Compile, make the image and push it to REGISTRY (needs a clean tree and docker login ghcr.io)
	@git diff --quiet HEAD || { echo "uncommitted changes: the image would not be that of $(COMMIT)" >&2; exit 1; }
	$(MAKE) image IMAGE=$(REGISTRY):$(VERSION)-$(COMMIT)
	docker tag $(REGISTRY):$(VERSION)-$(COMMIT) $(REGISTRY):$(VERSION)
	docker tag $(REGISTRY):$(VERSION)-$(COMMIT) $(REGISTRY):latest
	docker push $(REGISTRY):$(VERSION)-$(COMMIT)
	docker push $(REGISTRY):$(VERSION)
	docker push $(REGISTRY):latest

release-packs: ## Sign the country packs and push them to REGISTRY/countries (needs a clean tree, oras or docker login ghcr.io)
	@git diff --quiet HEAD || { echo "uncommitted changes: the packs would not be those of $(COMMIT)" >&2; exit 1; }
	GEKO_PACK_SIGNING_KEY=$$(cat $(HOME)/.config/geko/pack-signing.key) GEKO_PACKS_REGISTRY=$(REGISTRY)/countries ci/packs.sh publish

test: ## Every check but those of the machine, on the golden of every country (ci/test.sh)
	ci/test.sh

test-golden-es: ## The golden of the Balearic Islands: results, data, robustness and instructions
	ci/test.sh --variant results,data,robustness,instructions --scope es

test-golden-lu: ## The golden of Luxembourg: results, data, robustness and instructions
	ci/test.sh --variant results,data,robustness,instructions --scope lu

test-regression: ## Whole Spain against the last pushed commit, load included (on demand)
	ci/test.sh --variant results,data,instructions --scope spain --include-performance

golden-snapshot: ## Renew the data of the golden and publish them in ghcr.io (SCOPE=es,lu; needs oras login, write:packages)
	ci/golden-snapshot.sh renew $(if $(SCOPE),--scope $(SCOPE))

golden-snapshot-local: ## Renew the data of the golden on this machine only, without publishing them (SCOPE=es,lu)
	ci/golden-snapshot.sh renew --only-local $(if $(SCOPE),--scope $(SCOPE))
