# Staging de POIs — esquema común

Toda fuente de POIs que no es OSM escribe sus POIs en `staging/<fuente>/<país>/<región|_all>/pois.parquet` con estas columnas, y la fusión los lee sin código propio de la fuente (`geko-build/src/fusion/records.rs`). Puede añadir otras columnas suyas detrás: la fusión las ignora. Compresión zstd, coordenadas en EPSG:4326.

| Columna | Tipo | Nulo | Notas |
|---|---|---|---|
| `source_id` | utf8 | no | Identificador del POI en la fuente, estable entre descargas |
| `name` | utf8 | no | Nombre principal tal como lo da la fuente |
| `names` | utf8 | sí | JSON con los demás nombres, con claves al modo de OSM (`name:es`, `alt_name`…) |
| `category` | utf8 | no | Categoría como etiqueta de OSM (`amenity=restaurant`, `shop=bakery`): la fusión compara familias de categorías de OSM. El provider traduce las suyas; si no tiene equivalente, `<fuente>=<categoría>` |
| `wikidata` | utf8 | sí | Identificador de Wikidata (`Q…`) |
| `phone` | utf8 | sí | Un teléfono, tal como lo da la fuente. Opcional: si falta la columna, todos nulos |
| `website` | utf8 | sí | Una web, tal como la da la fuente. Opcional, como `phone` |
| `lat_e7` | int32 | no | 1e-7 grados |
| `lon_e7` | int32 | no | 1e-7 grados |

Una fuente que no se publica por región (un fichero mundial ordenado por zonas) declara `stage_by_country`: se prepara una vez por país en `_all`, y el reparto (`relocated/`) lleva cada POI a la región que lo contiene.

En la fusión, un POI de otra fuente es el de OSM a menos de 75 m cuyo nombre es igual, o lo contiene como palabras completas y es de la misma clase de sitio, o, si se escribe de otra forma, el que además tiene su mismo teléfono o web (de los que no comparten más de 4 POIs), una palabra en común y la misma clave de categoría (PROJECT.md §6.5.2, `docs/FUSION_METHOD.md`). Sus nombres se unen como variantes y la geometría sale de OSM. El staging de POIs de OSM también lleva `phone` y `website` (de `phone`/`contact:phone` y `website`/`contact:website`).
