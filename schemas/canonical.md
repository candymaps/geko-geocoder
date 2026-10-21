# Canónico — `canonical/1`

Escrito por la fusión (`geko-build/src/fusion`) en `canonical/<país>/`. Una entidad por cada cosa del mundo real, enriquecida con todas las fuentes (PROJECT.md §6.5). Cada fichero lleva la clave de metadatos `geko.schema = canonical/1`. Compresión zstd. Coordenadas en EPSG:4326, en 1e-7 grados (`lat_e7`, `lon_e7`).

Convenciones:

- `public_id`: id público estable (PROJECT.md §5.5). Las claves que se repiten llevan `-2`, `-3`… en orden determinista.
- `local_id` (`uint32`, en `places`, `streets`, `pois`, `postcodes` y `addresses`): id local denso del segmento, compartido por el motor de búsqueda y la inversa. Primero las entidades buscables en orden de importancia descendente; después los portales, agrupados por calle y ordenados por número (cada calle forma un bloque contiguo).
- `*_id`: `public_id` de otra entidad (municipio, calle, portal).
- `names`, `aliases`, `postcodes`, `source_ids`: JSON.
- `sources`: fuentes que aportaron la entidad, separadas por comas.
- `importance`: `u16`, de 0 a 65535 (PROJECT.md §6.3).
- `*_source` de un atributo: la fuente de la que sale, o `inferred` si se dedujo (PROJECT.md §6.5.4).

## `places.parquet`

Comunidades, provincias, municipios y barrios (de límites administrativos), y localidades, barrios y topónimos (de nodos `place`). Un nodo `place` que es el mismo lugar que un límite (p. ej. `place=city` Barcelona y el municipio de Barcelona) se funde en él.

| Columna | Tipo | Nulo | Notas |
|---|---|---|---|
| `public_id` | utf8 | no | `geko:municipality:es-08019`, `geko:province:es-b`, `geko:locality:es-osm-n123`… |
| `kind` | utf8 | no | `region`, `province`, `municipality`, `neighbourhood`, `locality`, `toponym` |
| `name`, `names` | utf8 | no | |
| `lat_e7`, `lon_e7` | int32 | no | Punto representativo |
| `population` | int64 | sí | |
| `wikidata`, `official_ref`, `iso3166_2`, `place_type` | utf8 | sí | `official_ref`: código INE en España |
| `municipality_id`, `province_id`, `region_id` | utf8 | sí | Por coordenadas |
| `postcodes` | utf8 | sí | Solo municipios: JSON `[{"postcode", "addresses", "sources": {fuente: portales}}]` con todos sus CP. Nulo en el resto |
| `postcode` | utf8 | sí | Localidades, barrios y topónimos: el CP de su zona. Nulo en municipios, provincias y comunidades |
| `postcode_source` | utf8 | sí | `inferred` cuando hay `postcode` |
| `importance` | uint16 | no | |
| `sources`, `source_ids` | utf8 | no | |
| `geom` | binary | sí | MultiPolygon WKB de los límites administrativos |

## `streets.parquet`

| Columna | Tipo | Nulo | Notas |
|---|---|---|---|
| `public_id` | utf8 | no | `geko:street:es-08019-cl-mallorca` |
| `name` | utf8 | no | Nombre para mostrar (el de la fuente con geometría si la hay) |
| `type_code` | utf8 | sí | Código común de tipo de vía (`CL`, `AV`, `PZ`…) |
| `core` | utf8 | no | Nombre normalizado sin tipo ni conectores |
| `names` | utf8 | no | Nombres multilingües de OSM |
| `aliases` | utf8 | no | Todos los nombres con que la conoce alguna fuente |
| `municipality_id` | utf8 | sí | |
| `locality` | utf8 | sí | Entidad de población (núcleo, urbanización) de la mayoría de sus portales, según las fuentes que la dan (CartoCiudad). Distingue calles homónimas de un mismo municipio |
| `lat_e7`, `lon_e7` | int32 | no | Punto representativo |
| `highway` | utf8 | sí | Clase vial más alta de sus tramos |
| `postcodes` | utf8 | sí | JSON `[{"postcode", "from", "to", "addresses", "sources": {fuente: portales}}]`: cada CP de la calle con su intervalo de números, sus portales y la procedencia de su CP. Una calle sin portales tiene una sola entrada, la de su zona, con `"sources": {"inferred": 1}`. Las calles nunca tienen un `postcode` único |
| `address_count` | uint32 | no | |
| `importance` | uint16 | no | |
| `sources`, `source_ids` | utf8 | no | |
| `geom` | binary | sí | MultiLineString WKB; nulo si ninguna fuente tiene su geometría |

## `addresses.parquet`

| Columna | Tipo | Nulo | Notas |
|---|---|---|---|
| `public_id` | utf8 | no | `geko:address:es-08019-cl-mallorca:401` |
| `street_id`, `street_name` | utf8 | no | |
| `number` | uint32 | sí | Nulo = sin número |
| `suffix` | utf8 | sí | Letra, `BIS`… |
| `range_end` | uint32 | sí | Último número de un rango como `187-189` |
| `label` | utf8 | no | `401`, `12B`, `187-189`, `s/n` |
| `lat_e7`, `lon_e7` | int32 | no | De la fuente con prioridad para la posición |
| `postcode` | utf8 | sí | |
| `postcode_source` | utf8 | sí | `cartociudad`, `osm` o `inferred` |
| `municipality_id` | utf8 | sí | |
| `importance` | uint16 | no | |
| `sources`, `source_ids` | utf8 | no | |

## `pois.parquet`

| Columna | Tipo | Nulo | Notas |
|---|---|---|---|
| `public_id`, `name`, `names`, `category` | utf8 | no | `category` como `clave=valor` de OSM |
| `lat_e7`, `lon_e7` | int32 | no | |
| `wikidata`, `municipality_id` | utf8 | sí | |
| `postcode`, `postcode_source` | utf8 | sí | Inferido de su zona si la fuente no lo trae |
| `address_id` | utf8 | sí | Portal más cercano (inferido, a menos de 25 m y en el mismo municipio) |
| `address_inferred` | bool | no | |
| `importance` | uint16 | no | |
| `sources`, `source_ids` | utf8 | no | |

## `postcodes.parquet`

| Columna | Tipo | Nulo | Notas |
|---|---|---|---|
| `public_id`, `postcode` | utf8 | no | `geko:postcode:es-08013` |
| `lat_e7`, `lon_e7` | int32 | no | Centroide de sus portales con CP de una fuente |
| `municipality_id` | utf8 | sí | Municipio con más portales de ese CP |
| `address_count` | uint32 | no | |

## `postcode_cells.parquet`

Zonas de CP (PROJECT.md §6.5.4): celda H3 de resolución 9 → CP mayoritario de sus portales con CP de una fuente.

| Columna | Tipo | Nulo |
|---|---|---|
| `cell` | uint64 | no |
| `postcode` | utf8 | no |

## `conflicts.parquet`

Atributos en los que las fuentes discrepan (PROJECT.md §6.5.3).

| Columna | Tipo | Nulo |
|---|---|---|
| `entity_kind`, `public_id`, `attribute` | utf8 | no |
| `chosen`, `chosen_source`, `discarded`, `discarded_source` | utf8 | no |

## `fusion_report.json`

Recuentos por fuente y por entidad, estadísticas de emparejamiento, atributos inferidos, métricas de calidad (PROJECT.md §13.6) con ejemplos, umbrales y fallos.
