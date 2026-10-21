# geko-geocoder

The geocoding module of [geko](../../README.md): it turns addresses and place names into coordinates (search and autocomplete), and coordinates into addresses (reverse). It downloads its sources and the country packs of the scope, merges them into one entity per street, address or place, builds an index per country and serves it under `/geocoder/`.

## Configuration: `geko-geocoder.toml`

The host (`geko.toml`) decides which countries and regions to serve, whether downloads are allowed and where it listens. Everything specific to geocoding goes in this module's own file, beside it: `/etc/geko/geko-geocoder.toml`, or the file named by `[modules.geocoder] config` (relative to `geko.toml`). Without it, every option takes its default value:

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
registry = "ghcr.io/candymaps/geko/countries"   # where the country packs are downloaded from

[serve]
countries              = []         # the countries this node serves; empty: every country of the bundle
search_budget_ms       = 15         # time budget of a search (0: none); the likeliest reading always completes
autocomplete_budget_ms = 5          # and of an autocomplete request
cache_entries          = 0          # answers kept for repeated queries, about 2.5 KB each (0: none)
```

Every section and option of the geocoder is in [geko-geocoder.toml](geko-geocoder.toml), in this folder, with its default value and what it does: use it as the reference to configure the geocoder. An unknown option is an error that names the file and the key, and an option that used to be in `geko.toml` and is now here (`[sources.*]`, `[packs]`, `[serve] search_budget_ms`…) stops geko at startup, saying to move it here. Options that change what is built (those of the sources) are part of the stamps of the build stages: changing one rebuilds only what it affects.

In Docker, mount it beside `geko.toml`:

```bash
docker run -d --name geko -p 8080:8080 -v geko-data:/var/lib/geko \
  -v "$PWD/geko.toml:/etc/geko/geko.toml:ro" \
  -v "$PWD/geko-geocoder.toml:/etc/geko/geko-geocoder.toml:ro" ghcr.io/candymaps/geko
```

## Usage

With the host on port 8080:

- Test page, with a map: <http://localhost:8080/geocoder/> (`/` leads there)
- Search: <http://localhost:8080/geocoder/v1/search?text=gran+via+1,+madrid>
- Autocomplete: `/geocoder/v1/autocomplete?text=gran+v`; reverse: `/geocoder/v1/reverse?lat=40.4168&lon=-3.7038`
- Also `/geocoder/v1/search/structured`, `/geocoder/v1/place/{id}` and `/geocoder/v1/batch` (POST).
- The full API: [openapi.yaml](openapi.yaml), served at `/geocoder/openapi.yaml` and, browsable with Swagger UI with the routes of the host, at `/docs`.
- The method of the derivative database (ODbL 4.6): `/geocoder/v1/attribution/method`. The licenses of the sources are in the host's `/v1/attribution`.

Debugging commands, on the last bundle built: `geko geocoder parse "<query>"` (the readings of a query), `geko geocoder query "<query>" [--autocomplete] [--focus lat,lon] [--debug]` (search with the time of each reading, without the time budget) and `geko geocoder reverse <lat> <lon>`. `geko geocoder` lists them.

## Data sources

By default, the data comes from OpenStreetMap and Overture Maps Places (POIs), for any country. Each country can also contribute its own sources in its pack (`countries/<country>/`), and the geocoder merges them all into a single source of truth, with no duplicates. Spain, for example, adds CartoCiudad ([countries/es/README.md](../../countries/es/README.md)).

Downloading requires `accept_licenses = true` in the `[data]` of `geko.toml`, after reviewing the licenses listed by `geko plan`. How sources are merged is in [FUSION_METHOD.md](../../docs/FUSION_METHOD.md), and the rest of the design in [PROJECT.md](../../docs/PROJECT.md) (in Spanish), §5–§9.

## Technologies

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
