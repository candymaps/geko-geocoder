# Staging CartoCiudad — `cartociudad-staging/2`

Escrito por el provider genérico `source-ogc-features`, según la declaración `countries/es/sources/cartociudad.toml`, en `staging/cartociudad/ES/<región|_all>/addresses.parquet`. Clave de metadatos `geko.schema = cartociudad-staging/2`. Las seis primeras columnas y las coordenadas son las del esquema común de direcciones ([addresses.md](addresses.md)). Compresión zstd. Coordenadas en EPSG:4326. Los valores se guardan tal como los da la fuente, salvo los números ficticios, sin normalizar: la normalización para comparar es parte de la fusión (PROJECT.md §6.5.1).

Origen: OGC API Features del IDEE, colección `address` (`https://api-features.idee.es/collections/address/items`), descargada por código postal (PROJECT.md §7.6).

| Columna | Tipo | Nulo | Propiedad de origen | Notas |
|---|---|---|---|---|
| `source_id` | utf8 | no | `id` | P. ej. `AD_ADDRESS_PPK_09.08.G08_080190073758` |
| `street` | utf8 | sí | `component_ThoroughfareName` | Tipo de vía incluido y en mayúsculas: `AVENIDA DIAGONAL` |
| `number` | utf8 | sí | `locator_designator_addressNumber` | `9998` y `9999`, números ficticios de portales sin número, se guardan como nulo |
| `number_extension` | utf8 | sí | `locator_designator_addressNumberExtension` | Letra, bis… |
| `postcode` | utf8 | sí | `component_PostalDescriptor` | |
| `place` | utf8 | sí | `component_AddressAreaName` | Población o entidad |
| `municipality` | utf8 | sí | `component_AdminUnitName_4` | |
| `province` | utf8 | sí | `component_AdminUnitName_3` | |
| `data_provider` | utf8 | sí | `dataProvider` | P. ej. `Dirección General de Catastro` |
| `version` | utf8 | sí | `beginLifespanVersion` | Fecha de la versión, `AAAA-MM-DD` |
| `lat_e7` | int32 | no | `geometry.coordinates[1]` | 1e-7 grados |
| `lon_e7` | int32 | no | `geometry.coordinates[0]` | 1e-7 grados |

Se descartan los elementos sin geometría o con coordenadas fuera de rango.
