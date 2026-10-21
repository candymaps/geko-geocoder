# Shortcuts for working on geko: `make help` lists them. Linux and macOS run it as is; Windows
# through make.cmd, which runs it inside WSL, where geko is built and its data live.

# Run from Windows, make starts without the login profile that puts cargo in the PATH.
export PATH := $(HOME)/.cargo/bin:$(PATH)
export CARGO_TARGET_DIR ?= $(HOME)/.cache/geko-target
# Without a C compiler (WSL without build-essential), Zig compiles and links, if it is there (CLAUDE.md).
ifeq ($(shell command -v cc 2>/dev/null),)
ifneq ($(wildcard $(HOME)/.local/bin/zigcc),)
export CC := $(HOME)/.local/bin/zigcc
export AR := $(HOME)/.local/bin/zigar
export CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER := $(HOME)/.local/bin/zigcc
endif
endif
BIN    := $(CARGO_TARGET_DIR)/release/geko
# geko lives in /usr/local/bin, reads /etc/geko/geko.toml and keeps its data in /var/lib/geko.
LISTEN ?= 0.0.0.0:8080
GEKO   := /usr/local/bin/geko

IMAGE    ?= geko
# Where `make release` publishes the image, and the tags it gives it: the version of geko and the
# commit, besides `latest`.
REGISTRY ?= ghcr.io/edgaza/geko
VERSION  := $(shell sed -n '/^\[workspace.package\]/,/^\[/s/^version = "\(.*\)"/\1/p' Cargo.toml)
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

# `make dev`: the container, its port, its configuration and its volume.
DEV_PORT   ?= 8080
DEV_CONFIG ?= docker/dev.toml

.PHONY: help bin install serve up data query parse reverse image dev release release-packs test test-golden-es test-golden-lu test-regression golden-snapshot golden-snapshot-local

help: ## This list
	@grep -E "^[a-z-]+:.*## " $(MAKEFILE_LIST) | awk -F ":.*## " '{ printf "  make %-10s %s\n", $$1, $$2 }'
	@echo "  Variables: LISTEN=$(LISTEN) IMAGE=$(IMAGE) REGISTRY=$(REGISTRY)"

bin: ## Compile geko in release
	cargo build --release -p geko-app

install: bin ## Install geko, /etc/geko/geko.toml (the repository's, if there is none) and /var/lib/geko
	cmp -s $(BIN) /usr/local/bin/geko || sudo install -m 755 $(BIN) /usr/local/bin/geko
	[ -f /etc/geko/geko.toml ] || sudo install -D -m 644 geko.toml /etc/geko/geko.toml
	[ -d /var/lib/geko ] || sudo install -d -o $$(id -un) -g $$(id -gn) /var/lib/geko

serve: install ## Serve the published bundle on LISTEN (the demo page is at /)
	$(GEKO) serve --listen $(LISTEN)

up: install ## Serve, and build in the background what the configuration asks for (now and every max_age_days)
	$(GEKO) up --listen $(LISTEN)

data: install ## Download and build what the configuration asks for
	$(GEKO) build

query: install ## A search with timings: make query Q="gran via 1, madrid"
	$(GEKO) query "$(Q)" --debug

parse: install ## The readings of a query: make parse Q="gran via 1, madrid"
	$(GEKO) parse "$(Q)"

reverse: install ## Reverse geocoding: make reverse LAT=40.4168 LON=-3.7038
	$(GEKO) reverse $(LAT) $(LON)

image: bin ## Docker image IMAGE with the last release build (docker build compiles it instead)
	@need=$$(grep -aoE 'GLIBC_[0-9]+\.[0-9]+' $(BIN) | sed 's/GLIBC_//' | sort -Vu | tail -1); \
	[ "$$(printf '%s\n' $$need $(IMAGE_GLIBC) | sort -V | tail -1)" = "$(IMAGE_GLIBC)" ] || { \
	  echo "geko needs glibc $$need, newer than the $(IMAGE_GLIBC) of the image: it would not start." >&2; \
	  echo "Build it on an older system, with Zig (CLAUDE.md), or let docker build compile it: docker build -t $(IMAGE) ." >&2; exit 1; }
	rm -rf $(CARGO_TARGET_DIR)/image && mkdir -p $(CARGO_TARGET_DIR)/image
	cp $(BIN) $(CARGO_TARGET_DIR)/image/geko
	BUILDX_GIT_INFO=false docker build --build-context binary=$(CARGO_TARGET_DIR)/image --build-arg VERSION=$(VERSION) --build-arg REVISION=$(COMMIT)$(DIRTY) -t $(IMAGE) .

dev: ## Compile the working tree and (re)create the container geko-dev with DEV_CONFIG and the packs of the repository
	$(MAKE) image IMAGE=geko:dev
	docker rm -f geko-dev > /dev/null 2>&1 || true
	docker run -d --name geko-dev -p $(DEV_PORT):8080 -v geko-dev-data:/var/lib/geko \
	  -v "$(abspath $(DEV_CONFIG))":/etc/geko/geko.toml:ro -v "$(CURDIR)/countries":/packs:ro -e GEKO_PACKS=/packs geko:dev
	@echo "geko-dev builds, then serves http://localhost:$(DEV_PORT)/ (demo) and /docs (API). Its log follows; Ctrl-C leaves it running."
	docker logs -f geko-dev

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
