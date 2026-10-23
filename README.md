<p align="center">
  <img height="100" src="https://placehold.co/210x30/transparent/A4CC47?text=Geko+Geocoder&font=raleway" alt="geko-geocoder">
</p>
<h3 align="center">The geocoding module of geko: addresses and places into coordinates, and back.</h3>
<p align="center">It merges OpenStreetMap, Overture Maps and each country's official sources into one entity per street, address or place, and answers in milliseconds.</p>
<p align="center">
  <a href="https://github.com/candymaps/geko-geocoder/releases/latest"><img src="https://img.shields.io/github/v/release/candymaps/geko-geocoder?label=version&color=blue&style=plastic" alt="Version"></a>
  <a href="https://www.gnu.org/licenses/agpl-3.0"><img src="https://img.shields.io/badge/license-AGPL--3.0-orange?style=plastic" alt="License"></a>
  <a href="https://github.com/orgs/candymaps/packages?repo_name=geko-geocoder"><img src="https://img.shields.io/badge/module-ghcr.io-informational?style=plastic&logo=docker" alt="Module"></a>
  <a href="https://github.com/candymaps/geko-geocoder/actions/workflows/publish.yml"><img src="https://github.com/candymaps/geko-geocoder/actions/workflows/publish.yml/badge.svg?branch=main" alt="publish"></a>
</p>
<p align="center">
  <a href="#installation-and-usage"><b>🚀 Installation</b></a>&nbsp;&nbsp;&nbsp;∣&nbsp;&nbsp;&nbsp;
  <a href="#"><b>🗺️ Demo</b></a>&nbsp;&nbsp;&nbsp;∣&nbsp;&nbsp;&nbsp;
  <a href="#"><b>📖 Documentation</b></a>&nbsp;&nbsp;&nbsp;∣&nbsp;&nbsp;&nbsp;
  <a href="openapi.yaml"><b>⚡ API</b></a>
</p>

# geko-geocoder

The geocoding module of [geko](https://github.com/candymaps/geko). It serves, under `/geocoder/`:

- **Search and autocomplete:** addresses, streets, places and POIs into coordinates, with typo tolerance, as the user types.
- **Reverse:** coordinates into the nearest address, street, POI or place.
- **Structured search, lookup by id and batch** requests, in the same API.

On top of the same foundation as the rest of geko:

- **A single source of truth:** OpenStreetMap, Overture Maps Places and each country's official sources (CartoCiudad, in Spain) are merged into one entity per street, address or place, with no duplicates.
- **Fully automatic:** it downloads its sources and the country packs of the scope of `geko.toml`, builds an index per country and keeps it up to date.
- **Fast and lightweight:** written in Rust, with its own memory-mapped indexes. It answers in milliseconds and serves all of Spain with less than 1 GB of memory.
- **A new country is a data pack** (`countries/<country>/`), with no changes to the engine.

This repository is the module, which the host ([geko](https://github.com/candymaps/geko)) downloads, signed, and serves. The country packs (`countries/`), with their reference queries (`golden/`), live here too. How the sources are merged is in [FUSION_METHOD.md](module/FUSION_METHOD.md).

## Installation and usage

All you need is Docker and the image of the host: geko downloads the geocoder on its own.

**1. A `geko.toml`** with the geocoder enabled and the countries to serve (every option of the host is in its [configuration reference](https://github.com/candymaps/geko#configuration-reference-gekotoml)):

```toml
[modules]
geocoder = true

[[scope]]
country = "ES"            # ISO 3166-1; one [[scope]] per country (LU…)
# regions = ["ES-PM"]     # only part of it: ISO 3166-2, from the regions in the country pack

[data]
accept_licenses = true    # you accept the licenses of the sources: ODbL (OSM), CDLA (Overture), CC BY (CartoCiudad)
```

**2. Optionally, a `geko-geocoder.toml`** beside it, with the options of the geocoder: its sources, time budgets, cache… (in [Configuration reference](#configuration-reference-geko-geocodertoml), below). Without it, every option takes its default value.

**3. Start it:**

```bash
docker run -d --name geko -p 8080:8080 -v geko-data:/var/lib/geko \
  -v "$PWD/geko.toml:/etc/geko/geko.toml:ro" \
  -v "$PWD/geko-geocoder.toml:/etc/geko/geko-geocoder.toml:ro" ghcr.io/candymaps/geko
```

Without a `geko-geocoder.toml`, leave out its `-v`. The first time, geko downloads the geocoder, its sources and the country packs, builds the indexes and serves them (it answers 503 until then): about 2 minutes for Luxembourg, around an hour for all of Spain. Follow it with `docker logs -f geko`.

**4. Use it.** With the host on port 8080:

- Test page, with a map: <http://localhost:8080/geocoder/> (`/` leads there)
- Search: <http://localhost:8080/geocoder/v1/search?text=gran+via+1,+madrid>
- Autocomplete: `/geocoder/v1/autocomplete?text=gran+v`; reverse: `/geocoder/v1/reverse?lat=40.4168&lon=-3.7038`
- Also `/geocoder/v1/search/structured`, `/geocoder/v1/place/{id}` and `/geocoder/v1/batch` (POST).
- The full API: [openapi.yaml](openapi.yaml), served at `/geocoder/openapi.yaml` and, browsable with Swagger UI with the routes of the host, at `/docs`.
- The method of the derivative database (ODbL 4.6): `/geocoder/v1/attribution/method`. The licenses of the sources are in the host's `/v1/attribution`.

Debugging commands, on the last bundle built: `geko geocoder parse "<query>"` (the readings of a query), `geko geocoder query "<query>" [--autocomplete] [--focus lat,lon] [--debug]` (search with the time of each reading, without the time budget) and `geko geocoder reverse <lat> <lon>`. `geko geocoder` lists them.

## Configuration reference (`geko-geocoder.toml`)

The host (`geko.toml`) decides which countries and regions to serve, whether downloads are allowed and where it listens. Everything specific to geocoding goes in this module's own file, beside it: `/etc/geko/geko-geocoder.toml`, or the file named by `[modules.geocoder] config` (relative to `geko.toml`). Every option, with its default value:

```toml
# Sources are used by default when they cover some country of the scope: their sections only
# change options, or disable a source with `enabled = false`. OSM cannot be disabled.
[sources.osm]
mirror = "auto"                     # or the URL of a mirror with Geofabrik's layout

[sources.overture]                  # Overture Maps Places (POIs), for every country
enabled        = true
release        = "latest"           # or a fixed release, such as "2026-09-23.1"
min_confidence = 0.8                # places Overture is less sure exist are left out (0 to 1)

[sources.cartociudad]               # declared by the pack of Spain; any key of its declaration can be overridden
enabled = true

[packs]
registry = "ghcr.io/candymaps/geko-geocoder/countries"   # where the country packs are downloaded from

[serve]
countries              = []         # the countries this node serves; empty: every country of the bundle
search_budget_ms       = 15         # time budget of a search (0: none); the likeliest reading always completes
autocomplete_budget_ms = 5          # and of an autocomplete request
cache_entries          = 0          # answers kept for repeated queries, about 2.5 KB each (0: none)
```

They are also in [develop/geko-geocoder.toml](develop/geko-geocoder.toml), with what each one does. An unknown option is an error that names the file and the key, and an option that used to be in `geko.toml` and is now here (`[sources.*]`, `[packs]`, `[serve] search_budget_ms`…) stops geko at startup, saying to move it here. Options that change what is built (those of the sources) are part of the stamps of the build stages: changing one rebuilds only what it affects.

## Data sources

By default, the data comes from OpenStreetMap and Overture Maps Places (POIs), for any country. Each country can also contribute its own sources in its pack (`countries/<country>/`), and the geocoder merges them all into a single source of truth, with no duplicates. Spain, for example, adds CartoCiudad ([countries/es/README.md](countries/es/README.md)).

Downloading requires `accept_licenses = true` in the `[data]` of `geko.toml`, after reviewing the licenses listed by `geko plan`. How sources are merged is in [FUSION_METHOD.md](module/FUSION_METHOD.md).

## License

geko-geocoder is licensed under the [GNU Affero General Public License v3.0](LICENSE) (AGPL-3.0-only). You may use, study, modify and share it freely. If you distribute it, modified or not, you must pass on its source code under the same license; if you modify it and let others use it over a network, you must offer them the source code of your version. Running it, or modifying it for internal use only, requires nothing more.

The data that it serves keeps the licenses of its sources (ODbL for OpenStreetMap, CDLA for Overture, CC BY for CartoCiudad…), and every response carries their attribution. Contributions are accepted under the same license, from the collaborators of the repository; see [CONTRIBUTING.md](CONTRIBUTING.md).

Read the full terms in [LICENSE](LICENSE).

## Technologies

### Foundation

| Technology | What it is used for |
|---|---|
| **Rust** (2024 edition) | The module: download of its sources, merge, indexes and search |
| `geko-module-sdk`, `geko-fetch`, `geko-artifact` | The contract with the host and what it shares with its modules, from [geko](https://github.com/candymaps/geko) (tag `sdk-<contract>`) |
| `clap` | The commands of the module: `geko geocoder parse`, `query` and `reverse` |
| `serde`, `serde_json`, `toml` | Configuration (`geko-geocoder.toml`), country packs, JSON responses |
| `thiserror`, `anyhow` | Typed errors in the libraries and errors with context in the binaries |
| `tracing` | Activity logging (downloads, timing of each stage) |
| `mimalloc` | Memory allocator of the module |
| `arc-swap` | Atomic swap of the index on hot reload |

### Data download

| Technology | What it is used for |
|---|---|
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

### Search and reverse

| Technology | What it is used for |
|---|---|
| `fst` | Indexes of words, full names, prefixes and public ids, as compact automata |
| `levenshtein_automata` | Typo tolerance in search |
| `memmap2` | Memory-mapped index files: they open in milliseconds, without copying them |
| `bytemuck` | Reading the index arrays straight from the mapped file, without copies |
| Hilbert-packed R-tree (own, in `reverse-rstar`) | Reverse geocoding (addresses, streets every 20 m, POIs and places), read from mapped files |

### Tooling

| Technology | What it is used for |
|---|---|
| WSL Debian | Build and run environment (Linux, as in production) |
| `cargo`, `clippy`, `rustfmt`, `cargo-deny` | Building, static analysis, formatting and dependency rules |
| Conformance suites (own, `geko-conformance` and the host's) | The contract of each port and of the module |
| Golden (own, `geko-eval`) | The reference queries of each country (`countries/<country>/golden/`), on frozen data, against their baseline |
| `geko-bench` (own) and `valgrind` | Instructions of the hot paths, the same from one run and one machine to another |
| DuckDB | Inspecting the staging and canonical Parquet files |
| `ci/test.sh` (own) | The checks, the same locally and in CI (GitHub Actions) |
