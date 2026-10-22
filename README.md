<p align="center">
  <img height="100" src="https://placehold.co/100x30/transparent/A4CC47?text=Geko&font=raleway" alt="geko">
</p>
<h3 align="center">A modular, multi-country geospatial engine that is easy to configure.</h3>
<p align="center">geko automatically downloads the data for each configured scope and serves it with outstanding efficiency.</p>
<p align="center">
  <a href="https://github.com/candymaps/geko/releases/latest"><img src="https://img.shields.io/github/v/release/candymaps/geko?label=version&color=blue&style=plastic" alt="Version"></a>
  <a href="https://www.gnu.org/licenses/agpl-3.0"><img src="https://img.shields.io/badge/license-AGPL--3.0-orange?style=plastic" alt="License"></a>
  <a href="https://github.com/orgs/candymaps/packages?repo_name=geko"><img src="https://img.shields.io/badge/docker-ghcr.io-informational?style=plastic&logo=docker" alt="Docker"></a>
  <a href="https://github.com/candymaps/geko/actions/workflows/ci.yml"><img src="https://github.com/candymaps/geko/actions/workflows/ci.yml/badge.svg?branch=main" alt="CI"></a>
</p>
<p align="center">
  <a href="#installation-and-usage"><b>🚀 Installation</b></a>&nbsp;&nbsp;&nbsp;∣&nbsp;&nbsp;&nbsp;
  <a href="#"><b>🗺️ Demo</b></a>&nbsp;&nbsp;&nbsp;∣&nbsp;&nbsp;&nbsp;
  <a href="#"><b>📖 Documentation</b></a>&nbsp;&nbsp;&nbsp;∣&nbsp;&nbsp;&nbsp;
  <a href="#"><b>⚡ API</b></a>
</p>

# geko

A modular, multi-country geospatial engine. Each service is a module, enabled in `geko.toml`, on top of the same data:

- **Geocoding** (available, [modules/geko-geocoder](modules/geko-geocoder/README.md)): turns addresses and place names into coordinates (search and autocomplete), and coordinates into addresses (reverse).
- **Vector tiles, routing…** (planned): new modules that plug in without changing the others.

They all share the same foundation:

- **A single source of truth:** OpenStreetMap, Overture Maps and each country's official sources are merged into one entity per street, address or place, with no duplicates.
- **Fully automatic:** `geko.toml` declares the modules and the countries and regions to serve; geko downloads only what those modules need, builds their indexes, serves them and keeps them up to date without restarting.
- **Fast and lightweight:** written in Rust, with its own indexes. Geocoding answers in milliseconds and serves all of Spain with less than 1 GB of memory.
- **A new country is a data pack**, with no changes to the engine.

The full specification is in [PROJECT.md](docs/PROJECT.md) (in Spanish).

## Installation and usage

All you need is Docker.

**1. A `geko.toml`** with the modules and countries to serve:

```toml
[modules]
geocoder = true          # geocoding; more modules (tiles, routing…) are on the way

[[scope]]
country = "ES"            # ISO 3166-1; one [[scope]] per country (LU…)
# regions = ["ES-PM"]     # only part of it: ISO 3166-2, from the regions in the country pack (provinces or autonomous communities, in Spain); without regions, the whole country

[data]
accept_licenses = true    # you accept the licenses of the sources: ODbL (OSM), CDLA (Overture), CC BY (CartoCiudad)
```

Every section and option of geko itself is in [Configuration reference](#configuration-reference-gekotoml), below, with its default value and what it does. Each module has its own file beside it, `geko-<module>.toml`, documented in the README of the module: the geocoder's (its sources, time budgets, cache…) in [modules/geko-geocoder](modules/geko-geocoder/README.md#configuration-geko-geocodertoml). Without it, a module takes its defaults.

The image carries no configuration: without a mounted `geko.toml`, it stops right away asking for one. Inside the container, geko always listens on `0.0.0.0:8080`, whatever `[serve] listen` says.

**2. Start it:**

```bash
docker run -d --name geko -p 8080:8080 -v geko-data:/var/lib/geko \
  -v "$PWD/geko.toml:/etc/geko/geko.toml:ro" ghcr.io/candymaps/geko
```

To change the options of a module, mount its file too, beside it (`-v "$PWD/geko-geocoder.toml:/etc/geko/geko-geocoder.toml:ro"`).

The first time, it downloads the data for those countries, builds the indexes and serves them (it answers 503 until then): about 2 minutes for Luxembourg, around an hour for all of Spain (about 8 GB of disk while building and 5 GB afterwards, and 3 GB of memory). Follow it with `docker logs -f geko`: while it builds, one line every 2 seconds with how far it has gone (with `GEKO_VERBOSE=1`, `-e GEKO_VERBOSE=1` in Docker, or `geko --verbose`, every step instead). Later starts reuse the `geko-data` volume and only rebuild what changed.

**3. Use it.** The demo page, a map with the search of the geocoder, is at <http://localhost:8080/>. Each module serves its routes under its name: the geocoder under `/geocoder/`, with its API in [its README](modules/geko-geocoder/README.md#usage). Common to all of them: `/health`, `/metrics`, the licenses and attribution of the served data at `/v1/attribution`, and the API of the host and of every active module, browsable with Swagger UI, at `/docs` (the host's, [openapi.yaml](openapi.yaml), also at `/openapi.yaml`).

To update: `docker pull ghcr.io/candymaps/geko`, remove the container and start it again with the same command; the data stays in the volume.

## Configuration reference (`geko.toml`)

Every section and option of geko itself (the host), with its default value unless a comment says otherwise. geko reads `/etc/geko/geko.toml` (or `GEKO_CONFIG`), and each module its `geko-<module>.toml` beside it: the geocoder's options are in [modules/geko-geocoder](modules/geko-geocoder/README.md#configuration-geko-geocodertoml).

```toml
# Every module, enabled or not (`true`, `false`, or a table with `enabled` and `config`, the file
# it reads, by default geko-<module>.toml beside this one). A module linked into geko is used as it
# is; otherwise geko downloads it into /var/lib/geko/modules: the newest version of the contract it
# speaks, or the one fixed with `version` (geko plan shows which). `geko serve` uses the version that
# built the bundle it serves, and downloads it if missing.
[modules]
geocoder = true
tiles    = true                     # still a skeleton: it serves nothing yet

# [modules.geocoder]                # instead of `geocoder = true` above
# version  = "1.4.2"                # default: the newest of the contract of this geko
# registry = "ghcr.io/candymaps/geko-geocoder"   # default: ghcr.io/candymaps/geko-<module>

[[scope]]                           # one per country; no default
country = "ES"                      # ISO 3166-1
regions = ["ES-PM"]                 # ISO 3166-2, from the regions of the country pack; empty: the whole country

[data]
refresh            = "if-changed"   # never | if-changed | always
max_age_days       = 30             # days between two checks of every source (geko build --refresh: now)
keep_downloads     = false          # true keeps the downloads after publishing (handy in development)
offline            = false          # never contact the sources; keeps the downloads
accept_licenses    = false          # required to download: review the licenses listed by geko plan
parallel_downloads = 4              # downloads at the same time

[build]
keep = 1                            # published versions kept (2 to be able to roll back)

[serve]
listen                 = "127.0.0.1:8080"   # in Docker, always 0.0.0.0:8080
reload_secs            = 30         # check for a new bundle version this often
cors_origins           = []         # origins a browser page may call the API from ("*": any)
api_keys               = []         # keys a request must carry (Authorization: Bearer, X-Api-Key or api_key=); empty: open
rate_limit_per_s       = 0          # requests per second from one client address (0: none)
rate_limit_burst       = 0          # burst above that rate (0: twice the rate)
bundles_url            = ""         # HTTP address of bundles published on another machine; each reload pulls only what changed
attribution_contact    = ""         # where to ask for the code of the derivative database (ODbL 4.6), in /v1/attribution
```

## License

geko is licensed under the [GNU Affero General Public License v3.0](LICENSE) (AGPL-3.0-only). You may use, study, modify and share it freely. If you distribute it, modified or not, you must pass on its source code under the same license; if you modify it and let others use it over a network, you must offer them the source code of your version. Running it, or modifying it for internal use only, requires nothing more.

The data that geko serves keeps the licenses of its sources (ODbL for OpenStreetMap, CDLA for Overture, CC BY for CartoCiudad…), and every response carries their attribution. Contributions require signing the [Contributor License Agreement](CLA.md); see [CONTRIBUTING.md](CONTRIBUTING.md).

Read the full terms in [LICENSE](LICENSE).

## Technologies

### Foundation

| Technology | What it is used for |
|---|---|
| **Rust** (2024 edition) | The whole engine: download, build, merge, indexes and serving |
| `clap` | Command line: `geko plan`, `fetch`, `build`, `serve`, `up`, `purge` and the commands of each module (`geko geocoder query`…) |
| `serde`, `serde_json`, `toml` | Configuration (`geko.toml`), bundle manifest, JSON responses |
| `thiserror`, `anyhow` | Typed errors in the libraries and errors with context in the binary |
| `tracing`, `tracing-subscriber` | Activity logging (downloads, timing of each phase, reloads) |
| `mimalloc` | Memory allocator. With glibc's, reading OSM spent 32 s in system calls |

### Data download

| Technology | What it is used for |
|---|---|
| `ureq` (with `rustls`) | Synchronous HTTP client with pure-Rust TLS: downloads with resume, ETag and retries |
| `sha2`, `md-5`, `hex` | Checksums: md5 of the Geofabrik extracts, sha256 of downloads and bundle artifacts |

### HTTP service

| Technology | What it is used for |
|---|---|
| `axum` | HTTP server of the host: it mounts the routes of each module under its name (`/geocoder/…`) and serves the shared ones (`/health`, `/metrics`, `/v1/attribution`, `/docs`) |
| `tokio` | Asynchronous server; only `geko serve` uses it |
| `arc-swap` | Atomic swap of the bundle version on hot reload |

### Tooling

| Technology | What it is used for |
|---|---|
| WSL Debian | Build and run environment (Linux, as in production) |
| `cargo`, `clippy`, `rustfmt` | Building, static analysis and formatting |
| `tempfile`, `tiny_http` | Tests: temporary directories and a fake HTTP server for the downloader |
| DuckDB (CLI) | Inspecting and validating the staging and canonical Parquet files |
| `geko-eval` (own) | Generating reference queries from the canonical data and evaluating geko over HTTP (top-1 and top-5 accuracy, MRR, latency); comparing with a baseline and failing if anything gets worse |
| `geko-conformance` (own) | Conformance suites for each port, which every provider runs from its tests |
| `ci/test.sh` (own) | The checks, the same locally and in CI (GitHub Actions) |

### Planned modules and tooling ([PROJECT.md](docs/PROJECT.md), §15)

| Technology | What it will be used for |
|---|---|
| Planetiler (Java) | Tiles module: generating the vector tiles |
| PMTiles | Tiles module: tile format, served with HTTP range requests |
| To be decided | Routing module: route calculation on the OSM road network, in the style of OSRM |
| `criterion`, `oha` | Internal benchmarks and HTTP load tests (throughput at the target latency) |
