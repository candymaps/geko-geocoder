<p align="center">
  <img height="100" src="https://placehold.co/300x100?text=geko" alt="geko">
</p>
<h3 align="center">A modular, multi-country geospatial engine that is easy to configure.</h3>
<p align="center">geko automatically downloads the data for each configured scope and serves it with outstanding efficiency.</p>
<p align="center">
  <a href="https://github.com/edgaza/geko/releases/latest"><img src="https://img.shields.io/github/v/release/edgaza/geko?label=version&color=blue&style=plastic" alt="Version"></a>
  <a href="https://www.gnu.org/licenses/agpl-3.0"><img src="https://img.shields.io/badge/license-AGPL--3.0-orange?style=plastic" alt="License"></a>
  <a href="https://github.com/edgaza?tab=packages&q=geko"><img src="https://img.shields.io/badge/docker-ghcr.io-informational?style=plastic&logo=docker" alt="Docker"></a>
  <a href="https://github.com/edgaza/geko/actions/workflows/ci.yml"><img src="https://github.com/edgaza/geko/actions/workflows/ci.yml/badge.svg?branch=main" alt="CI"></a>
</p>
<p align="center">
  <a href="#installation-and-usage"><b>🚀 Installation</b></a>&nbsp;&nbsp;&nbsp;∣&nbsp;&nbsp;&nbsp;
  <a href="#"><b>🗺️ Demo</b></a>&nbsp;&nbsp;&nbsp;∣&nbsp;&nbsp;&nbsp;
  <a href="#"><b>📖 Documentation</b></a>&nbsp;&nbsp;&nbsp;∣&nbsp;&nbsp;&nbsp;
  <a href="#"><b>⚡ API</b></a>
</p>

# geko

A modular, multi-country geospatial engine. Each service is a module, enabled in `geko.toml`, on top of the same data:

- **Geocoding** (available): turns addresses and place names into coordinates (search and autocomplete), and coordinates into addresses (reverse).
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

Without a mounted `geko.toml`, the image uses its sample one: Luxembourg, with the licenses not accepted, so it stops right away asking you to accept them. Inside the container, geko always listens on `0.0.0.0:8080`, whatever `[serve] listen` says.

**2. Start it:**

```bash
docker run -d --name geko -p 8080:8080 -v geko-data:/var/lib/geko \
  -v "$PWD/geko.toml:/etc/geko/geko.toml:ro" ghcr.io/edgaza/geko
```

The first time, it downloads the data for those countries, builds the indexes and serves them (it answers 503 until then): about 2 minutes for Luxembourg, around an hour for all of Spain (about 8 GB of disk while building and 5 GB afterwards, and 3 GB of memory). Follow it with `docker logs -f geko`. Later starts reuse the `geko-data` volume and only rebuild what changed.

**3. Use it** (geocoding module):

- Test page, with a map: <http://localhost:8080/>
- Search: <http://localhost:8080/v1/search?text=gran+via+1,+madrid>
- Autocomplete: `/v1/autocomplete?text=gran+v`; reverse: `/v1/reverse?lat=40.4168&lon=-3.7038`
- The full API: [openapi.yaml](openapi.yaml), which `geko serve` also serves at `/openapi.yaml` and, browsable with Swagger UI, at `/docs`. Every `geko.toml` option, with its default value: [geko.toml](geko.toml).

To update: `docker pull ghcr.io/edgaza/geko`, remove the container and start it again with the same command; the data stays in the volume.

## Data sources

By default, the data comes from OpenStreetMap and Overture Maps Places (POIs), for any country. Each country can also contribute its own sources in its pack (`countries/<country>/`), and geko merges them all into a single source of truth, with no duplicates. Spain, for example, adds CartoCiudad ([countries/es/README.md](countries/es/README.md)). OSM cannot be disabled; Overture can, with `enabled = false` in `[sources.overture]`.

Downloading requires `accept_licenses = true` in `[data]`, after reviewing the licenses listed by `geko plan`. The options of each source and of data updates are in [PROJECT.md](docs/PROJECT.md), §4.1 and §7.

## License

geko is licensed under the [GNU Affero General Public License v3.0](LICENSE) (AGPL-3.0-only). You may use, study, modify and share it freely. If you distribute it, modified or not, you must pass on its source code under the same license; if you modify it and let others use it over a network, you must offer them the source code of your version. Running it, or modifying it for internal use only, requires nothing more.

The data that geko serves keeps the licenses of its sources (ODbL for OpenStreetMap, CDLA for Overture, CC BY for CartoCiudad…), and every response carries their attribution. Contributions require signing the [Contributor License Agreement](CLA.md); see [CONTRIBUTING.md](CONTRIBUTING.md).

Read the full terms in [LICENSE](LICENSE).

## Technologies

### Foundation

| Technology | What it is used for |
|---|---|
| **Rust** (2024 edition) | The whole engine: download, build, merge, indexes and serving |
| `clap` | Command line: `geko plan`, `fetch`, `build`, `serve`, `query`, `reverse`, `parse` |
| `serde`, `serde_json`, `toml` | Configuration (`geko.toml`), bundle and index manifests, JSON responses |
| `thiserror`, `anyhow` | Typed errors in the libraries and errors with context in the binary |
| `tracing`, `tracing-subscriber` | Activity logging (downloads, timing of each phase, reloads) |
| `mimalloc` | Memory allocator. With glibc's, reading OSM spent 32 s in system calls |

### Data download

| Technology | What it is used for |
|---|---|
| `ureq` (with `rustls`) | Synchronous HTTP client with pure-Rust TLS: downloads with resume, ETag and retries |
| `sha2`, `md-5`, `hex` | Checksums: md5 of the Geofabrik extracts, sha256 of downloads and bundle artifacts |
| `zstd` | Compression of the pages downloaded from CartoCiudad (1.2 GB of JSON become 35 MB) |
| Geofabrik | OpenStreetMap extracts by country and region, with their extract index |
| IDEE's OGC API Features | CartoCiudad addresses, downloaded by postcode |

### Source reading and data formats

| Technology | What it is used for |
|---|---|
| `osmpbf` | Reading OpenStreetMap `.pbf` files, block by block |
| `flate2` with `zlib-rs` | Decompressing OSM blocks (much faster than the default backend) |
| `rayon` | Data parallelism: OSM reading, merging and index building |
| `roaring` | Compact id sets: required OSM nodes and list unions in the engine |
| **Apache Parquet** (`arrow-array`, `arrow-schema`, `parquet`) | Format of the intermediate data: each source's staging and the merged canonical data (`schemas/`) |

### Geometry and space

| Technology | What it is used for |
|---|---|
| `geko-geom` (own) | Joining segments into rings, point in polygon with banded edges, WKB and distances in meters |
| `rstar` | R-tree: assigning a municipality to each point during the merge, and administrative boundary boxes for reverse geocoding |
| `h3o` (H3) | Hexagonal cells of the postcode zones, to infer postcodes |

### Merging and country rules

| Technology | What it is used for |
|---|---|
| Own merge in Rust (`geko-build`) | Entity resolution with *union-find*, master record, inferred attributes, ids and importance |
| `text-rules` (own, generic) | Text normalization and query parsing for any country, with the data of its pack: street types, connectors, floor and door words, postcode format. Decimal, DMS and UTM coordinates |
| `geko-country-pack` (own) | Reads the country packs at startup: text data, regions and what each source needs to know about the country |
| `countries/es/` pack (data) | Spain: street types in Spanish, Catalan, Galician and Basque, address words, postcode format, provinces and autonomous communities |

### Geocoding module: search and reverse

| Technology | What it is used for |
|---|---|
| `fst` | Indexes of words, full names, prefixes and public ids, as compact automata |
| Levenshtein automata (`fst`) | Typo tolerance in search |
| `memmap2` | Memory-mapped index files: they open in milliseconds, without copying them |
| `bytemuck` | Reading the index arrays straight from the mapped file, without copies |
| Hilbert-packed R-tree (own, in `reverse-rstar`) | Reverse geocoding (addresses, streets every 20 m, POIs and places), read from mapped files |

### HTTP service

| Technology | What it is used for |
|---|---|
| `axum` | HTTP routes of each module's API (geocoding: `/v1/search`, `/v1/autocomplete`, `/v1/reverse`, `/v1/place`; shared: `/health`, `/metrics`) |
| `tokio` | Asynchronous server; only `geko serve` uses it |
| `arc-swap` | Atomic swap of the index version on hot reload |

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
