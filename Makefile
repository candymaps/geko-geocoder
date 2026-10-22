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
# geko with the geocoder linked in (crates/geko-host), the binary of the checks and of `make serve`.
BIN    := $(CARGO_TARGET_DIR)/release/geko
# `make modules`: the geocoder as the library geko loads, in a directory as GEKO_MODULES reads it.
MODULES ?= $(CARGO_TARGET_DIR)/modules
MODULE_NAMES := geocoder
# The repository of the host beside this one (AGENTS.md), whose develop/geko.toml `make install`
# copies the first time.
HOST ?= ../geko
# geko lives in /usr/local/bin, reads /etc/geko/geko.toml and keeps its data in /var/lib/geko.
LISTEN ?= 0.0.0.0:8080
GEKO   := /usr/local/bin/geko

# Where `make release-packs` publishes the country packs.
PACKS_REGISTRY ?= ghcr.io/candymaps/geko/countries
# The commit, read from .git where git is not installed (WSL compiles a checkout made on Windows).
COMMIT   := $(shell git rev-parse --short HEAD 2>/dev/null || { \
              r=$$(sed -n 's|^ref: ||p' .git/HEAD 2>/dev/null); \
              if [ -z "$$r" ]; then cat .git/HEAD; else cat ".git/$$r" 2>/dev/null || sed -n "s| $$r\$$||p" .git/packed-refs; fi; \
            } | cut -c1-7)

.PHONY: help bin modules install serve up data query parse reverse release-packs test test-changed test-golden-es test-golden-lu test-regression golden-snapshot golden-snapshot-local

help: ## This list
	@grep -E "^[a-z-]+:.*## " $(MAKEFILE_LIST) | awk -F ":.*## " '{ printf "  make %-10s %s\n", $$1, $$2 }'
	@echo "  Variables: LISTEN=$(LISTEN) HOST=$(HOST) PACKS_REGISTRY=$(PACKS_REGISTRY)"

bin: ## Compile geko, with the geocoder linked in, in release
	cargo build --release -p geko-host

modules: ## Compile the geocoder, as the library geko loads, in MODULES: GEKO_MODULES=MODULES geko … uses it
	cargo build --release $(foreach m,$(MODULE_NAMES),-p geko-module-$(m)-dylib)
	@for m in $(MODULE_NAMES); do \
	  install -D -m 755 $(CARGO_TARGET_DIR)/release/libgeko_$$m.so $(MODULES)/$$m/libgeko_$$m.so; \
	  install -m 644 modules/geko-$$m/module.toml $(MODULES)/$$m/module.toml; \
	done

install: bin ## Install geko, /etc/geko/geko.toml (that of HOST) and geko-geocoder.toml (the samples, if they are not) and /var/lib/geko
	cmp -s $(BIN) /usr/local/bin/geko || sudo install -m 755 $(BIN) /usr/local/bin/geko
	[ -f /etc/geko/geko.toml ] || sudo install -D -m 644 $(HOST)/develop/geko.toml /etc/geko/geko.toml
	[ -f /etc/geko/geko-geocoder.toml ] || sudo install -D -m 644 modules/geko-geocoder/geko-geocoder.toml /etc/geko/geko-geocoder.toml
	[ -d /var/lib/geko ] || sudo install -d -o $$(id -un) -g $$(id -gn) /var/lib/geko

serve: install ## Serve the published bundle on LISTEN (the demo page is at /)
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

release-packs: ## Sign the country packs and push them to PACKS_REGISTRY (needs a clean tree, oras or docker login ghcr.io)
	@git diff --quiet HEAD || { echo "uncommitted changes: the packs would not be those of $(COMMIT)" >&2; exit 1; }
	GEKO_PACK_SIGNING_KEY=$$(cat $(HOME)/.config/geko/pack-signing.key) GEKO_PACKS_REGISTRY=$(PACKS_REGISTRY) ci/packs.sh publish

test: ## Every check but those of the machine, on the golden of every country (ci/test.sh)
	ci/test.sh

test-changed: ## Only the checks that what changed since BASE (default: where it leaves dev) can fail, as the CI of pull requests
	@args=$$(ci/affected.sh $(or $(BASE),$$(git merge-base HEAD origin/dev))) && if [ -z "$$args" ]; then echo "only documentation changed"; else echo "ci/test.sh $$args"; ci/test.sh $$args; fi

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
