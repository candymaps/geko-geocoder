# Staging de direcciones — esquema común

Toda fuente de direcciones que no es OSM escribe sus portales en `staging/<fuente>/<país>/<región|_all>/addresses.parquet` con estas columnas, y la fusión los lee sin código propio de la fuente (`geko-build/src/fusion/records.rs`). Puede añadir otras columnas suyas detrás: la fusión las ignora. Compresión zstd, coordenadas en EPSG:4326.

| Columna | Tipo | Nulo | Notas |
|---|---|---|---|
| `source_id` | utf8 | no | Identificador del portal en la fuente, estable entre descargas |
| `street` | utf8 | sí | Nombre de la vía tal como lo da la fuente, con su tipo |
| `number` | utf8 | sí | Número tal como lo da la fuente. Nulo si no tiene: el provider convierte los números ficticios de su fuente en nulo |
| `number_extension` | utf8 | sí | Letra, bis… |
| `postcode` | utf8 | sí | |
| `place` | utf8 | sí | Población o entidad a la que la fuente asigna el portal. El municipio se asigna siempre por coordenadas |
| `lat_e7` | int32 | no | 1e-7 grados |
| `lon_e7` | int32 | no | 1e-7 grados |

En la fusión, la posición y el CP de una dirección salen de una fuente nacional antes que de OSM.
