# Pack de España (`es`)

Todo lo propio de España (PROJECT.md §2.4). El motor y los providers de `providers/` son genéricos; este pack aporta lo que solo tiene sentido aquí.

| Contenido | Qué es |
|---|---|
| `data/regions.toml` | Provincias (las particiones de la generación, con el prefijo de su CP, que es su código INE) y comunidades autónomas, con su código ISO 3166-2 |
| `sources/osm.toml` | Los extractos de Geofabrik de cada comunidad y de España entera: el índice no da códigos ISO a sus regiones, y Canarias está en África |
| `pack.toml`, `data/` | Datos de texto, leídos por el normalizador y el analizador genéricos (`providers/text-rules`): lenguas y formato de CP (`pack.toml`), tipos de vía en castellano, catalán, gallego y euskera (`data/street-types.toml`), conectores, marcas de número, palabras de piso y puerta, «s/n» y sufijos de portal (`data/words.toml`) |
| `src/`, `tests/` | Crate `country-pack-es`, solo para los tests: lee el pack del repositorio; sus tests comprueban la normalización y el análisis de consultas de España |
| `sources/cartociudad.toml` | Direcciones de CartoCiudad, por la OGC API Features del IDEE (PROJECT.md §7.6): la declaración que lee el provider genérico `source-ogc-features` (servicio, colección, recorrido por CP y columnas del staging) |
| `golden/` | Consultas de referencia: Baleares (`generated`, `holdout`, `manual`, `focus`) y España completa (`spain-*`) |
| `golden/baseline/` | Línea base de Baleares que compara `ci/test.sh --scope es` |

- **Lenguas:** castellano, catalán, gallego y euskera.
- **Particiones:** las 52 provincias (`common/`).

## Fuentes

Con `country = "ES"` en el ámbito, España usa siempre **OSM + CartoCiudad + Overture**, sin escribir nada más en `geko.toml`:

- **OSM:** es la base de todos los países y no se puede desactivar (README del proyecto).
- **Overture Maps:** POIs de todos los países (unos 650.000 más en España), opcional con `[sources.overture]` (README del proyecto). El golden de Baleares la deja fuera (`GEKO_GOLDEN_DISABLE`), porque su instantánea solo congela OSM y CartoCiudad.
- **CartoCiudad:** se usa por defecto. Su apartado `[sources.cartociudad]` es opcional:

| Campo | Por defecto | Qué hace |
|---|---|---|
| `parallel_requests` | `4` | Peticiones simultáneas a la OGC API del IDEE; pocas, por cortesía con un servicio público |
| `max_unchanged_days` | `365` | Aunque su catálogo diga que no ha cambiado, una provincia descargada hace más días se vuelve a bajar |
| `base` | `https://api-features.idee.es` | URL de la OGC API Features |
| `enabled` | `true` | `false` desactiva CartoCiudad: España se genera solo con OSM (unos 2,3 M de portales en vez de 16,1 M) |

Los campos que aparecen en el apartado sustituyen a los valores por defecto; los que no, los conservan. Cada cuánto se comprueba si hay datos nuevos no es de CartoCiudad sino de todas las fuentes a la vez: `[data] max_age_days` (README del proyecto). Al comprobarlo, CartoCiudad lee la fecha de su catálogo y solo se vuelve a bajar si hay datos más nuevos que su descarga (unos 35 minutos para España, 1–2 por provincia).

Otro país solo tiene que escribir sus propios `pack.toml` y `data/*.toml`: el motor no cambia (guía en [countries/README.md](../README.md)).

## Pendiente y decisiones

Lo que solo tiene sentido en España. Lo del motor está en [PROJECT.md](../../docs/PROJECT.md), sección 17; el historial, en `git show 8679454:docs/BACKLOG_ES.md`.

- **⬜ CartoCiudad desde el GeoPackage del CNIG, descargado a mano** (bloqueado). La descarga del CNIG exige reCAPTCHA y no se automatiza; sin una muestra no se conoce su esquema. Con una, sería otra declaración en `sources/` para un provider genérico de GeoPackage, que habría que añadir al motor.
- **⬜ «calle olmos» → «Carrer dels Oms».** El par oms/olmos no aparece en bastantes nombres de OSM para aprenderlo. Se puede añadir a mano a `data/equivalences.txt`.
- **Catastro y catastros forales: aplazados (2026-10-18).** Las direcciones ya las cubre CartoCiudad, que se construye en buena parte desde el Catastro (direcciones completas al 96–99 %). Lo propio del Catastro es la búsqueda por referencia catastral y por polígono y parcela rústicos, de nicho, a cambio de unos 8.000 feeds ATOM de varios GB más los catastros forales. Retomarlo si un uso concreto lo pide.
- **INE e IGN: no compensan por ahora (medido el 2026-10-18).** En el canónico, el 98,5 % de los 8.262 municipios ya trae población y código INE de OSM, y las 568.000 localidades `place=locality` vienen casi todas de la importación del NGBE a OSM. De unos 190 fallos en consultas completas de España, solo 2 tienen que ver con una localidad. Revisar si cambian los datos de OSM o si un fallo nuevo lo pide.
- **Nomenclátor y callejeros autonómicos: sin estudiar.** El Nomenclátor llega en buena parte a través del NGBE importado en OSM.
- **Punto kilométrico: hecho (2026-10-18), sin fuente nueva.** CartoCiudad trae los kilómetros de las carreteras como portales («Carretera N-340», 691). La colección `markerpost` de la OGC API del IDEE no sirve: sus hitos no traen el código de la carretera.
