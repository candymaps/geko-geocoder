# Staging OSM — `osm-staging/1`

Escrito por `source-osm` en `staging/osm/<país>/<región|_all>/`. Cada fichero Parquet lleva la clave de metadatos `geko.schema = osm-staging/1`. Compresión zstd. Coordenadas en EPSG:4326.

## Columnas comunes (todas las tablas)

| Columna | Tipo | Nulo | Descripción |
|---|---|---|---|
| `osm_type` | utf8 | no | `n`, `w` o `r` |
| `osm_id` | int64 | no | Id de OSM |
| `lat_e7` | int32 | no | Latitud del punto representativo, en 1e-7 grados |
| `lon_e7` | int32 | no | Longitud del punto representativo, en 1e-7 grados |
| `name` | utf8 | sí | Etiqueta `name` |
| `names` | utf8 | no | JSON con `name`, `name:*`, `alt_name`, `old_name`, `official_name`, `short_name`, `loc_name`, `reg_name` (y sus variantes `:<idioma>`) |
| `wikidata` | utf8 | sí | Etiqueta `wikidata` |

Punto representativo: el nodo para nodos; el vértice central para viales; el centroide de los vértices para otras vías; el nodo `label` o `admin_centre`, o si no el centroide del anillo exterior mayor, para límites.

## `streets.parquet`

Vías con `highway` de tipo vial y con `name` o `ref`. Se excluyen las aceras y los pasos de peatones (`footway=sidewalk|crossing`).

| Columna | Tipo | Nulo | Descripción |
|---|---|---|---|
| `highway` | utf8 | no | Valor de `highway` |
| `geom` | binary | no | LineString en WKB |

## `addresses.parquet`

Nodos, vías y relaciones multipolígono con `addr:housenumber`.

| Columna | Tipo | Nulo |
|---|---|---|
| `housenumber` | utf8 | no |
| `street` | utf8 | sí |
| `place` | utf8 | sí |
| `postcode` | utf8 | sí |
| `city` | utf8 | sí |

## `pois.parquet`

Nodos, vías y relaciones multipolígono (edificios emblemáticos, parques…) con `name` y alguna de las claves `amenity`, `shop`, `tourism`, `leisure`, `office`, `craft`, `healthcare`, `historic`, `aeroway`, `railway`, `public_transport`, `natural`, `man_made`, `sport`. Las vías que son viales no se repiten como POI.

| Columna | Tipo | Nulo | Descripción |
|---|---|---|---|
| `category` | utf8 | no | Primera clave encontrada, como `clave=valor` |

## `places.parquet`

Nodos con `place` (`city`, `town`, `village`, `hamlet`, `isolated_dwelling`, `suburb`, `quarter`, `neighbourhood`, `borough`, `locality`, `square`) y `name`.

| Columna | Tipo | Nulo |
|---|---|---|
| `place` | utf8 | no |
| `population` | int64 | sí |

## `admins.parquet`

Relaciones `boundary=administrative` con `admin_level` cuya geometría se pudo cerrar. Se incluyen la región de la partición, los límites de nivel superior que la cortan y los de nivel inferior cuyo punto representativo cae dentro.

| Columna | Tipo | Nulo | Descripción |
|---|---|---|---|
| `admin_level` | uint8 | no | |
| `iso3166_1` | utf8 | sí | `ISO3166-1:alpha2` o `ISO3166-1` |
| `iso3166_2` | utf8 | sí | `ISO3166-2` |
| `official_ref` | utf8 | sí | `ine:municipio`, `ref:ine` o `ref` |
| `population` | int64 | sí | |
| `geom` | binary | no | MultiPolygon en WKB |

## Recorte

Con región, se conservan las entidades cuyo punto representativo cae dentro del límite OSM etiquetado con su `ISO3166-2`. Sin región, dentro del límite de país (`admin_level=2`) si el extracto lo contiene completo. El límite de país suele faltar en los extractos regionales, porque sus vías no están completas; la fusión lo deduce de la partición.
