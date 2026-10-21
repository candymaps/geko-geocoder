# geko

Motor de geocodificación (búsqueda, autocompletado e inversa) con servidor de teselas vectoriales opcional. Multipaís, empezando por España.

Objetivo: superar a Pelias en velocidad, cobertura de datos y calidad de resultados. Para ello funde OSM con fuentes oficiales de cada país (en España: CartoCiudad, Catastro, IGN, INE y callejeros autonómicos) en una única fuente de verdad sin duplicados, y usa un índice de búsqueda propio en Rust.

Todo es automático: en `geko.toml` se declaran el ámbito (países y regiones), los módulos y los providers, y geko descarga los datos, genera los índices y los sirve.

La especificación completa está en [PROJECT.md](docs/PROJECT.md).

## Instalación y uso

Solo hace falta Docker.

**1. Un `geko.toml`** con los países que se quieren servir:

```toml
[[scope]]
country = "ES"            # ISO 3166-1; otro [[scope]] por país (LU…)
# regions = ["ES-PM"]     # solo una parte: ISO 3166-2, de las regiones del pack del país (provincias o comunidades, en España); sin regions, el país entero

[data]
accept_licenses = true    # aceptas las licencias de las fuentes: ODbL (OSM), CDLA (Overture), CC BY (CartoCiudad)
```

Sin montar un `geko.toml`, la imagen usa el suyo de ejemplo: Luxemburgo y sin aceptar las licencias, así que se para al momento pidiendo aceptarlas. En el contenedor, geko escucha siempre en `0.0.0.0:8080`, diga lo que diga `[serve] listen`.

**2. Arrancarlo:**

```bash
docker run -d --name geko -p 8080:8080 -v geko-data:/var/lib/geko \
  -v "$PWD/geko.toml:/etc/geko/geko.toml:ro" ghcr.io/edgaza/geko
```

La primera vez descarga los datos de esos países, genera los índices y los sirve (responde 503 hasta entonces): unos 2 minutos con Luxemburgo, alrededor de una hora con España entera (unos 8 GB de disco mientras genera y 5 después, y 3 GB de memoria). Se sigue con `docker logs -f geko`. Los arranques siguientes reutilizan el volumen `geko-data` y solo rehacen lo que cambió.

**3. Usarlo:**

- Página de prueba, con mapa: <http://localhost:8080/>
- Búsqueda: <http://localhost:8080/v1/search?text=gran+via+1,+madrid>
- Autocompletado: `/v1/autocomplete?text=gran+v`; inversa: `/v1/reverse?lat=40.4168&lon=-3.7038`
- La API completa: [openapi.yaml](openapi.yaml), que `geko serve` también sirve en `/openapi.yaml` y, navegable con Swagger UI, en `/docs`. Todas las opciones de `geko.toml`, con sus valores por defecto: [geko.toml](geko.toml).

Para actualizar: `docker pull ghcr.io/edgaza/geko`, borrar el contenedor y arrancarlo de nuevo con la misma orden; los datos se quedan en el volumen.

## Tecnologías

### Base

| Tecnología | Para qué se usa |
|---|---|
| **Rust** (edición 2024) | Todo el motor: descarga, generación, fusión, índices y servicio |
| `clap` | Línea de órdenes: `geko plan`, `fetch`, `build`, `serve`, `query`, `reverse`, `parse` |
| `serde`, `serde_json`, `toml` | Configuración (`geko.toml`), manifiestos de bundles e índices, JSON de las respuestas |
| `thiserror`, `anyhow` | Errores tipados en las bibliotecas y errores con contexto en el binario |
| `tracing`, `tracing-subscriber` | Registro de actividad (descargas, tiempos de cada fase, recargas) |
| `mimalloc` | Asignador de memoria. Con el de glibc, leer OSM gastaba 32 s en llamadas al sistema |

### Descarga de datos

| Tecnología | Para qué se usa |
|---|---|
| `ureq` (con `rustls`) | Cliente HTTP síncrono con TLS en Rust puro: descargas con reanudación, ETag y reintentos |
| `sha2`, `md-5`, `hex` | Sumas de control: md5 de los extractos de Geofabrik, sha256 de descargas y artefactos de los bundles |
| `zstd` | Compresión de las páginas descargadas de CartoCiudad (1,2 GB de JSON quedan en 35 MB) |
| Geofabrik | Extractos de OpenStreetMap por país y región, con su índice de extractos |
| OGC API Features del IDEE | Direcciones de CartoCiudad, descargadas por código postal |

### Lectura de fuentes y formatos de datos

| Tecnología | Para qué se usa |
|---|---|
| `osmpbf` | Lectura de los ficheros `.pbf` de OpenStreetMap, bloque a bloque |
| `flate2` con `zlib-rs` | Descompresión de los bloques de OSM (mucho más rápida que el motor por defecto) |
| `rayon` | Paralelismo de datos: lectura de OSM, fusión y construcción de índices |
| `roaring` | Conjuntos compactos de ids: nodos de OSM necesarios y uniones de listas en el motor |
| **Apache Parquet** (`arrow-array`, `arrow-schema`, `parquet`) | Formato de los datos intermedios: staging de cada fuente y canónico fusionado (`schemas/`) |

### Geometría y espacio

| Tecnología | Para qué se usa |
|---|---|
| `geko-geom` (propio) | Unión de tramos en anillos, punto en polígono con aristas por franjas, WKB y distancias en metros |
| `rstar` | R-tree: asignar municipio a cada punto en la fusión, y cajas de los límites administrativos en la inversa |
| `h3o` (H3) | Celdas hexagonales de las zonas de código postal, para inferir CP |

### Fusión y reglas de cada país

| Tecnología | Para qué se usa |
|---|---|
| Fusión propia en Rust (`geko-build`) | Resolución de entidades con *union-find*, registro maestro, atributos inferidos, ids e importancia |
| `text-rules` (propio, genérico) | Normalización del texto y análisis de consultas de cualquier país, con los datos de su pack: tipos de vía, conectores, palabras de piso y puerta, formato de CP. Coordenadas decimales, GMS y UTM
| `geko-country-pack` (propio) | Lee los packs de país al arrancar: datos de texto, regiones y lo que cada fuente necesita saber del país |
| Pack `countries/es/` (datos) | España: tipos de vía en castellano, catalán, gallego y euskera, palabras de dirección, formato de CP, provincias y comunidades |

### Motor de búsqueda y geocodificación inversa

| Tecnología | Para qué se usa |
|---|---|
| `fst` | Índices de palabras, de nombres completos, de prefijos y de ids públicos, como autómatas compactos |
| Autómatas de Levenshtein (`fst`) | Tolerancia a erratas en la búsqueda |
| `memmap2` | Ficheros del índice mapeados en memoria: se abren en milisegundos, sin copiarlos |
| `bytemuck` | Lectura de los arrays del índice directamente desde el fichero mapeado, sin copias |
| R-tree empaquetado por curva de Hilbert (propio, en `reverse-rstar`) | Geocodificación inversa (portales, calles cada 20 m, POIs y lugares), leída desde ficheros mapeados |

### Servicio HTTP

| Tecnología | Para qué se usa |
|---|---|
| `axum` | Rutas HTTP de la API (`/v1/search`, `/v1/autocomplete`, `/v1/reverse`, `/v1/place`, `/health`) |
| `tokio` | Servidor asíncrono; solo lo usa `geko serve` |
| `arc-swap` | Cambio atómico de la versión del índice al recargarlo en caliente |

### Herramientas

| Tecnología | Para qué se usa |
|---|---|
| WSL Debian | Entorno de compilación y ejecución (Linux, como en producción) |
| `cargo`, `clippy`, `rustfmt` | Compilación, análisis estático y formato |
| `tempfile`, `tiny_http` | Pruebas: directorios temporales y servidor HTTP falso para el descargador |
| DuckDB (CLI) | Inspeccionar y validar los Parquet de staging y canónico |
| `geko-eval` (propio) | Generar consultas de referencia desde el canónico y evaluar geko o Pelias por HTTP (acierto en top-1, top-5, MRR, latencia); comparar con una línea base y fallar si algo empeora |
| `geko-conformance` (propio) | Suites de conformidad de cada puerto, que cada provider ejecuta desde sus tests |
| `ci/test.sh` (propio) | Las comprobaciones, iguales en local y en la CI (GitHub Actions) |

### Previstas en fases posteriores

| Tecnología | Para qué se usará |
|---|---|
| Planetiler (Java) | Generar las teselas vectoriales (fase 3) |
| PMTiles | Formato de las teselas, servidas por rangos HTTP (fase 3) |
| `criterion`, `oha` | Benchmarks internos y pruebas de carga HTTP (caudal a la latencia objetivo) |

## Fases

1. **Prueba de concepto con una sola provincia (Islas Baleares).**
   - Esqueleto del proyecto: núcleo, traits y módulos activables (`geocoder` y `tiles`).
   - Descarga automática de OSM y CartoCiudad según la configuración.
   - Fusión en una única fuente de verdad: una sola entidad por cada calle, portal o lugar, enriquecida con todas las fuentes y sin duplicados. Particionada por país y región, e incremental.
   - Motor de búsqueda propio por segmentos de país, con búsqueda, autocompletado, inversa y consulta por id.
   - Primera comparación con un Pelias de referencia, para comprobar con números que el diseño cumple lo prometido.
2. **España completa y más datos.**
   - Fuentes nuevas, con descarga automática: IGN, INE, Nomenclátor, Catastro y catastros forales, callejeros autonómicos y Wikidata.
   - Búsquedas nuevas: punto kilométrico *(hecho)*, referencia catastral y parcelas *(aplazadas con el Catastro, 2026-10-18)*.
   - Índices versionados, generados en una máquina y servidos en otra, descargando solo lo que cambia *(hecho: `[serve] bundles_url`)*.
   - Comparación con Pelias *(hecho: `ci/test.sh --scope pelias`)*.
3. **Teselas y API completa.**
   - Servidor de teselas con Planetiler, estilo, fuentes tipográficas e iconos. *(Aplazado por decisión, 2026-10-18: el módulo `tiles` se queda en su esqueleto.)*
   - Búsqueda estructurada, lotes y modos compatibles con Pelias y Photon *(hecho: `/v1/search/structured`, `/v1/batch`, `/pelias/v1/*` y `/photon/*`, comprobados en el golden contra la API propia)*.
   - POIs de Overture *(hecho, 2026-10-19)* y de registros oficiales.
   - Caché de respuestas *(hecho, 2026-10-18; apagada por defecto)*.
4. **Optimización guiada por métricas.**
   - Optimizaciones de velocidad del servicio (JSON preserializado, SIMD, PGO…) e índice fonético.
   - Ajuste del ranking y de la importancia con las consultas de referencia.
5. **Operación y más países.**
   - Actualización de OSM con diffs diarios.
   - Métricas y registro de consultas lentas *(hecho)*.
   - Regeneración programada de los índices y almacenamiento en S3.
   - Reparto de países entre nodos.
   - Pack de un segundo país *(hecho: Luxemburgo)*, y packs de país como datos firmados que se descargan al generar, con publicación automática de packs e imagen *(hecho, 2026-10-20)*.

## Fuentes de datos

- **OSM, siempre:** es la base de todos los países y se usa sin configurar nada; no se puede desactivar. Su apartado `[sources.osm]` en `geko.toml` es opcional y solo cambia sus opciones:

  | Campo | Por defecto | Qué hace |
  |---|---|---|
  | `mirror` | `auto` | `auto` descarga de Geofabrik; una URL usa un espejo con la misma estructura |

  ```toml
  [sources.osm]
  mirror = "https://mi-espejo.example/"
  ```

  Poner `enabled = false` en `[sources.osm]` es un error de configuración.

- **Fuentes nacionales:** las aporta el pack de cada país (`countries/<país>/`). Se usan por defecto cuando su país está en el ámbito, y se descartan solas si no lo está. Su apartado también es opcional: sirve para cambiar opciones o para desactivarlas con `enabled = false`. Las de España, en [countries/es/README.md](countries/es/README.md).

- **Overture Maps Places,** para todos los países: aporta POIs que OSM no tiene (en las ciudades de España, entre el 65 y el 80 % de sus lugares abiertos con confianza alta). Se usa por defecto y se desactiva con `enabled = false` en `[sources.overture]`. Solo descarga las partes de sus ficheros mundiales que tocan las particiones del ámbito, con las cajas de `data/region-boxes.tsv` del pack, o la del país si no hay (de 11 GB en todo el mundo: España entera, 270 MB; Baleares, 11 MB). Opciones: `release` (`latest` o una versión como `2026-09-23.1`), `min_confidence` (0,8) y `base` (la dirección del almacenamiento de Overture).

- **Licencias:** descargar exige `accept_licenses = true` en `[data]`, tras revisar las licencias que lista `geko plan`.

### Actualización de los datos y disco

Tres opciones de `[data]` deciden cuándo se contacta con las fuentes y qué se guarda de lo descargado:

| Opción | Por defecto | Qué hace |
|---|---|---|
| `refresh` | `"if-changed"` | `if-changed`: comprueba si las fuentes tienen datos nuevos cada `max_age_days` y solo baja lo que cambió. `always`: lo baja todo en cada build. `never`: nunca comprueba; solo baja lo que falta |
| `max_age_days` | `30` | Días entre dos comprobaciones, de todas las fuentes a la vez. Entretanto, un build no contacta ninguna fuente y solo baja lo que falta. `geko build --refresh` (o `fetch`, `up`) comprueba ya. `geko plan` dice cuándo toca |
| `keep_downloads` | `false` | Con `false`, tras publicar un bundle se borra lo descargado (en España, unos 2,4 GB) y se conservan sus fichas: sumas, etiquetas y fechas de cada descarga. Un build sin cambios reutiliza el staging sin las descargas |

Con `keep_downloads = false`, repetir el staging de una fuente sin datos nuevos (cambia su código, su `sources/*.toml` del pack, o `--force`) vuelve a bajar esa fuente, solo lo que leen las particiones que se repiten y con los datos más recientes que sirva. Lo más lento es CartoCiudad: unos 35 minutos para España. `geko plan` avisa cuando un build lo vaya a hacer. En desarrollo, donde el código cambia a menudo, conviene `keep_downloads = true`; las comprobaciones (`ci/test.sh`) ya lo usan.

Con `offline = true` no se borra nada, aunque `keep_downloads` sea `false`, y el build lo avisa al empezar. Si las descargas se borraron en un build anterior con conexión, repetir un staging sin conexión da un error que dice qué falta: hay que volver a generar con conexión, o con `keep_downloads = true` desde el principio.

## Rutas

- `geko` lee siempre `/etc/geko/geko.toml` y guarda siempre sus datos en `/var/lib/geko`: descargas (`raw/`), ficheros de trabajo (`staging/`, `relocated/`, `canonical/`, `segments/`) y bundles publicados (`bundles/`). Ni el `geko.toml` ni la línea de órdenes cambian estas rutas.
- Para generar en una máquina y servir en otra, la que genera ofrece su `/var/lib/geko/bundles` por HTTP (cualquier servidor web o almacenamiento de objetos) y la que sirve lo indica en `[serve] bundles_url`: en cada recarga baja la versión nueva a su propio `/var/lib/geko/bundles`, solo con los artefactos que cambiaron.
- Los packs de país se leen al arrancar de `/var/lib/geko/packs/<país>/`, solo los de los países que se piden. `geko plan`, `fetch`, `build` y `up` bajan los del ámbito que falten o hayan cambiado de `ghcr.io/edgaza/geko/countries/<país>` (`[packs] registry`), con la etiqueta del formato que entiende el binario (`format-2`), y solo los usan si llevan la firma de los packs publicados. Cada pack lleva su versión (`version` en `pack.toml`, publicada también como `pack-<n>`), que muestran `geko plan` y `geko build`. Un pack copiado a mano se usa tal cual y nunca se reemplaza; para probar uno sin publicar, `GEKO_PACKS=$PWD/countries` usa los del repositorio. Cada bundle lleva el pack con que se generó, y `geko serve` lee ese: la máquina que solo sirve no necesita la carpeta de los packs. La imagen de Docker no lleva ninguno: baja los de su ámbito al volumen.
- Las variables de entorno `GEKO_CONFIG` y `GEKO_DATA` cambian las dos rutas, y `GEKO_PACKS` la de los packs. Son para las comprobaciones (`ci/test.sh`) y la CI, que generan varios países a la vez y no escriben en `/etc` ni en `/var/lib`.
- Al cambiar el ámbito, lo que ya no está en él se queda en `/var/lib/geko`, por si vuelve. Se borra con `geko purge`:
  - `geko purge data`: las descargas y los ficheros de trabajo de los países, las regiones y las fuentes que la configuración ya no pide. `geko build --purge` y `geko up --purge` hacen lo mismo antes de generar.
  - `geko purge country-packs`: los packs descargados de los países fuera del ámbito; con `--all`, todos (se vuelven a bajar al generar). Los copiados a mano se quedan.
  - `geko purge`, sin más: todo lo anterior.
  - Las versiones antiguas de los bundles las retira `[build] keep`: por defecto solo se conserva la que se sirve; con 2 se puede volver a la anterior. Un servicio que aún tenga cargada la versión retirada sigue funcionando hasta cargar la nueva.

## Estado

Fase 1 terminada (PROJECT.md §15). Resultados en las Islas Baleares (PROJECT.md §13.4 bis), con consultas de referencia que no se usaron para afinar el ranking (semilla 7):

| | geko | Pelias (OSM) |
|---|---|---|
| Acierto en top-1, comparación justa de motor | **95,9 %** | 63,7 % |
| Acierto en top-1, producto completo (con CartoCiudad) | **93,5 %** | 38,2 % |
| Latencia por petición HTTP (mediana) | **0,1–3 ms** (local) | 60–90 ms (en la nube, con red) |
| Caudal (concurrencia 16, máquina de desarrollo) | búsqueda 3.372 QPS, autocompletado 7.409 QPS, inversa 67.718 QPS | — |

Hecho tras la fase 1 (el detalle, en `git show 8679454:docs/BACKLOG.md`; lo pendiente, en [PROJECT.md](docs/PROJECT.md), sección 17):

- **Generación:** incremental, que solo rehace las etapas cuyo código o entradas cambian (Baleares sin cambios: de 81 a 0,5 s).
- **Puertos:** cada uno tiene su suite de conformidad.
- **CI:** en GitHub Actions, en cada push a `main`: comprueba lo que cambió con `ci/test.sh` y, si pasa, lo publica.
- **España completa:** ya se genera y se sirve (PROJECT.md §13.4 ter):

| España | |
|---|---|
| Datos | 16,1 M de portales, 1,27 M de calles, 685.000 lugares, 880.000 POIs |
| Descarga (OSM + CartoCiudad), solo la primera vez | 36 min |
| Generación | 16 min sin descargas, con Overture (2026-10-20; 6,6 min sin Overture el 2026-10-16), 2,9 GB de pico en el índice |
| Segmento | índice de 1,8 GB e inversa de 687 MB (mapeada) |
| Servicio | carga en 0,97 s, 932 MB residentes |
| Acierto en top-1: dirección completa / con errata / sin municipio, con foco | 92–96 % / 90–95 % / 94 % |

- **Packs de país como datos:** leídos al arrancar, firmados y publicados en ghcr.io; `geko` baja los de su ámbito, y la imagen de Docker sirve para cualquier combinación de países sin llevar ninguno (PROJECT.md §2.4).
- **Segundo país:** Luxemburgo, con su pack (`countries/lu/`) y su golden, sin tocar el motor salvo arreglos genéricos ([countries/README.md](countries/README.md)).
- **Servicio:** página de demostración en `/`, `geko up` (generar y servir), `lang`, CORS, claves de API y límite de peticiones; red de seguridad contra duplicados con su contador en `/metrics`.
- Las últimas medidas completas, del 2026-10-18, en [docs/2026-10-18_ANALYSIS_RESULTS.md](docs/2026-10-18_ANALYSIS_RESULTS.md).

### Comprobaciones y evaluación

Todas viven en `ci/test.sh`, igual en local que en la CI (GitHub solo lo llama), que solo ejecuta pruebas: `--variant` dice qué se prueba y `--scope`, sobre qué datos. Los datos del golden los prepara aparte `ci/golden-snapshot.sh`. Cada prueba se usa donde tiene sentido:

- **Antes de cada push, en local**: `make test`, que lanza solo `.githooks/pre-push` (se activa una vez por clon con `git config core.hooksPath .githooks`; `git push --no-verify` lo salta). Es todo lo determinista, con el golden de todos los países.
- **En cada push a `main`, en la CI** (`ci.yml`): lo determinista de lo que cambió. El código, si cambió el motor; los contratos, si cambió el motor o algún pack; y el golden de cada país, de todos si cambió el motor o solo de los packs cambiados. Si todo pasa, publica los packs cambiados y, si el push toca lo que entra en ella, la imagen, con la versión del `Cargo.toml`. Las demás ramas no tienen CI.
- **Solo en local y a demanda** (o en un runner propio con el hardware de referencia de PROJECT.md §1.1): España completa (`make test-regression`) y toda latencia, caudal o memoria, los datos frescos (`GEKO_GOLDEN_FRESH=1`) y la comparación con Pelias.

```bash
ci/test.sh [--variant <variantes>] [--scope <ámbitos>] [--include-performance]
```

| `--variant` (por defecto, todas) | Qué prueba |
|---|---|
| `code` | clippy (avisos como errores), reglas de dependencias, licencias y avisos de seguridad (cargo-deny) y cada combinación de features |
| `contracts` | tests unitarios y suites de conformidad de cada puerto |
| `results` | aciertos consulta a consulta y en cada pulsación, calibración de `confidence`, duplicados y modos Pelias y Photon |
| `data` | recuentos e integridad del canónico, tamaño del bundle y determinismo del build |
| `robustness` | fuzz y recarga del bundle con carga |
| `instructions` | instrucciones de las rutas calientes, con valgrind (obligatorio; `GEKO_BENCH=0` las salta), frente a las del último commit subido (`GEKO_BASE` para otro), contadas en la misma pasada y en la misma máquina |

| `--scope` (por defecto, `es,lu`) | Sobre qué datos |
|---|---|
| `es`, `lu`… | el golden de un pack, con sus datos congelados (`countries/<país>/golden/snapshot`): Baleares para `es`, el país entero para los demás |
| `spain` | España completa, en local, frente al último commit subido |
| `pelias` | el golden de `es` frente a una instancia de Pelias con los mismos datos de OSM, la de `GEKO_PELIAS_URL` (solo `results`) |

`--include-performance` añade lo que depende de la máquina: las latencias del golden frente a las de esta máquina (con `results`) y, en `spain`, la carga, el acierto con carga y el servidor frente a los de su base. `code` y `contracts` no dependen del ámbito.

```bash
make test              # ci/test.sh: todo menos lo de la máquina, con el golden de todos los países
make test-golden-es    # el golden de Baleares: results, data, robustness, instructions
make test-golden-lu    # lo mismo con Luxemburgo entero
make test-regression   # España completa, en local y a demanda, con la carga (--include-performance)
GEKO_GOLDEN_UPDATE=1 make test-golden-es     # acepta los resultados como nueva línea base
GEKO_SPAIN_BUILD_AB=1 make test-regression   # además, el tiempo del build: base, nuevo, base, nuevo
GEKO_PELIAS_URL=<url> ci/test.sh --variant results --scope pelias   # frente a Pelias, en Baleares
```

#### Instantáneas del golden

**Para qué sirven.** El golden son las consultas de referencia de cada pack (`countries/<país>/golden/`): miles de direcciones, calles, lugares y POIs con su resultado esperado. Cada cambio se compara con su línea base consulta a consulta, con tolerancia 0. Eso solo vale si los datos no cambian entre una medida y otra: con datos frescos, OSM cambia a diario y no se sabría si una consulta perdida viene del código o de los datos. Por eso el golden no descarga, sino que usa una **instantánea**: las descargas de su ámbito (Baleares, Luxemburgo) congeladas en un archivo, sobre el que se midió la línea base. Con ella, solo un cambio del código puede cambiar los resultados. Renovarla solo hace falta para que el golden mida con datos actuales, cuando las fuentes se han alejado bastante.

**Dónde están.** La instantánea de cada país es la que nombra `countries/<país>/golden/snapshot`, con su sha256, y se guarda como paquete privado en `ghcr.io/edgaza/geko/countries/<país>/golden:<instantánea>`. La CI la lee con su propio token y corre el golden en cada push a `main` que puede cambiar sus resultados: el de todos los países si cambia el motor o `ci/`, solo el de uno si solo cambia su pack. En local, si no está en disco, se baja de ahí: hace falta una vez `oras login --registry-config ~/.config/oras/config.json ghcr.io -u <usuario>` con un token con `read:packages`.

**Cuándo renovarlos.** El golden avisa, sin fallar, en local y en la CI, cuando su instantánea pasa de los días que fija su pack en `golden/snapshot-max-age-days`, según lo deprisa que cambian sus fuentes: 60 en Baleares (OSM, CartoCiudad y POIs turísticos) y 120 en Luxemburgo (solo OSM, con direcciones estables). Son estimaciones, que se afinan con lo que pierde el golden con datos frescos a cada edad; `GEKO_SNAPSHOT_MAX_AGE_DAYS` las sobrescribe en una pasada. Solo hace falta cuando hay que medir con datos actuales: cuando las fuentes reales se han alejado bastante de la instantánea, o cuando un cambio necesita datos que la instantánea no tiene (una fuente nueva, una región nueva). Para saber si se han alejado, `GEKO_GOLDEN_FRESH=1 ci/test.sh --variant results,robustness --scope es,lu` pasa el golden con datos recién descargados: si pierde bastantes consultas que con la instantánea acierta, toca renovarla. Nunca en cada cambio: el golden vale porque compara siempre sobre los mismos datos.

**Renovar los datos del golden**, en local:

```bash
make golden-snapshot                 # todos los países con golden; SCOPE=es o SCOPE=lu para uno
make golden-snapshot-local           # lo mismo, sin publicar: solo en esta máquina
```

Por cada país, `ci/golden-snapshot.sh renew` (lo que lanzan los dos):

1. Descarga de nuevo las fuentes del ámbito del golden y mide sobre ellas la línea base nueva (`GEKO_GOLDEN_FRESH=1 GEKO_GOLDEN_UPDATE=1 ci/test.sh`).
2. Congela esas mismas descargas como instantánea y escribe `golden/snapshot` (`ci/golden-snapshot.sh create`).
3. La publica en ghcr.io (`ci/golden-snapshot.sh publish`), salvo con `golden-snapshot-local`. Hace falta una vez `oras login --registry-config ~/.config/oras/config.json ghcr.io -u <usuario>` con un token con `write:packages`.

Después, a mano:

4. Revisa la tabla de datos del informe (`entities …`: línea base y ahora), con entidades, duplicados e integridad, antes de aceptarla (CLAUDE.md).
5. Commit de `golden/snapshot` junto con `golden/baseline`. La CI pasará el golden sobre la instantánea nueva en el push a `main`, y para eso tiene que estar publicada: una hecha con `golden-snapshot-local` se publica antes con `ci/golden-snapshot.sh publish --scope <país>`.

`ci/test.sh --scope spain` compara el árbol de trabajo con el último commit subido (`GEKO_BASE` para otro), cuyos binarios compila en un worktree (`/var/lib/geko/ab/base`). Lo que no depende de lo ocupada que esté la máquina (aciertos, datos, tamaños, memoria) lo compara con la línea base de ese commit, que mide sola una vez por commit y por datos y guarda junto a ellos (`/var/lib/geko/check-baseline/`). Las instrucciones de cada ruta las cuenta con valgrind en los dos binarios, cada uno sobre su bundle. Lo que sí depende de la máquina (latencia, caudal, acierto con carga), solo con `--include-performance`, lo mide en los dos servidores a la vez, en cinco tajadas por turnos de cada combinación de ruta y concurrencia, y compara la mediana de las tajadas: así una máquina más o menos cargada pesa igual en los dos, y una racha en una tajada no mueve el resultado. Con 8 núcleos o más, los servidores van fijados a 4 núcleos, como el hardware de referencia, y el cliente a los demás.

Para añadir un país, la guía está en [countries/README.md](countries/README.md).

Las consultas de referencia se generan y evalúan con `geko-eval`. Las de `countries/es/golden/` son de Baleares; las `spain-*.jsonl`, de España completa:

```bash
geko-eval generate --canonical /var/lib/geko/canonical/ES --seed 42 --out countries/es/golden/generated.jsonl
geko-eval focus --canonical /var/lib/geko/canonical/ES --cases countries/es/golden/generated.jsonl --out countries/es/golden/focus.jsonl
geko-eval english --canonical /var/lib/geko/canonical/ES --words countries/es/golden/english.toml --out countries/es/golden/english.jsonl
geko-eval run --name geko --url http://127.0.0.1:8080 --cases countries/es/golden/generated.jsonl --out geko.json
geko-eval compare geko.json pelias.json
geko-eval check --baseline countries/es/golden/baseline/generated.json --report geko.json
geko-eval load --url http://127.0.0.1:8080 --cases countries/es/golden/spain-generated.jsonl --route search --concurrency 1,4,16
```

### Con Docker

La imagen, `ghcr.io/edgaza/geko`, lleva el binario y una configuración de ejemplo (`docker/geko.toml`), y ningún pack de país: al generar, `geko` baja los de su ámbito al volumen. Sirve igual para un país que para varios. Se monta la propia en `/etc/geko/geko.toml` (con `accept_licenses = true`; escucha siempre en `0.0.0.0:8080`) y un volumen en `/var/lib/geko`, donde quedan las descargas y los bundles:

```bash
docker build -t geko .
docker run -p 8080:8080 -v geko-data:/var/lib/geko -v ./geko.toml:/etc/geko/geko.toml geko
```

La orden por defecto es `up`: empieza a servir enseguida y, mientras, descarga y genera en segundo plano lo que pide la configuración. Hasta que se publica el primer bundle responde 503; Luxemburgo tarda unos 2 minutos desde un volumen vacío, y España completa, una media hora la primera vez. Después sigue sirviendo y, cada `[data] max_age_days` (30 días), comprueba las fuentes y genera de nuevo lo que cambió, sin cron y sin reiniciar: sirve el bundle de antes hasta que el nuevo está publicado, y lo recoge sin cortes. Si una generación falla, se queda el que sirve y lo vuelve a intentar al cabo de una hora. Un pack de país nuevo se usa al reiniciar el contenedor. Con `serve` en vez de `up`, solo sirve lo que ya hay en `/var/lib/geko` o, con `[serve] bundles_url`, lo que publica otra máquina. La imagen ocupa 162 MB.

### Atajos: `make`

En Linux y macOS, `make <objetivo>` en la raíz del repositorio. En Windows, `make.cmd`, que lo ejecuta dentro de WSL: `make <objetivo>` en cmd y `.\make <objetivo>` en PowerShell. `make help` los lista:

- `make serve`: sirve el bundle publicado en el puerto 8080 de todas las interfaces (`LISTEN=127.0.0.1:8080` lo limita a la propia máquina): `http://localhost:8080`, con la página de prueba en `/` y la API en `/docs`. `make up` descarga y genera antes lo que pide la configuración.
- `make query Q="gran via 1, madrid"`, `make parse Q="…"` y `make reverse LAT=40.4168 LON=-3.7038`.
- `make test`, `make test-golden-es`, `make test-golden-lu` y `make test-regression`: las comprobaciones de `ci/test.sh`.
- `make image`: la imagen de Docker con el binario de la última compilación, sin compilar dentro de Docker. Se niega si el binario pide una glibc más nueva que la de la imagen (2.36), porque no arrancaría.
- `make dev`: compila el código tal como está en la rama, cambios sin commit incluidos, construye la imagen `geko:dev` y vuelve a crear el contenedor `geko-dev`. Este sirve en `http://localhost:8080` (`DEV_PORT=` lo cambia): la demo en `/` y la API en `/docs`. Usa la configuración de prueba `docker/dev.toml` (Baleares, con `keep_downloads = true`; `DEV_CONFIG=` usa otra) y los packs del repositorio, sin publicar. Sus datos van al volumen `geko-dev-data`, que se conserva entre ejecuciones; `docker volume rm geko-dev-data`, con el contenedor parado, lo borra todo. Desde WSL necesita la integración de Docker Desktop con la distribución (Settings → Resources → WSL integration).
  - Volver a lanzarlo con el código cambiado recompila solo lo que cambió y sustituye el contenedor por uno con la imagen nueva, unos segundos sin servicio. El contenedor nuevo sirve enseguida el bundle que ya había en el volumen, salvo que el cambio altere su formato: entonces responde 503 hasta publicar el nuevo.
  - En segundo plano rehace solo las etapas cuyo código o entradas cambiaron, sin volver a bajar nada, y recoge el bundle nuevo sin reiniciar. Un cambio de ranking o de búsqueda no rehace ningún dato; uno en el staging de una fuente rehace ese staging y lo que depende de él. Los cambios de los packs y de `docker/dev.toml` también entran, porque van montados.
  - Solo toca el contenedor `geko-dev`. El de «Arranque rápido» (`geko`) usa el mismo puerto: para tener los dos a la vez, `make dev DEV_PORT=8081`.
- `make release-packs`: firma los packs de país con `~/.config/geko/pack-signing.key` y sube los que cambiaron a `ghcr.io/edgaza/geko/countries/<país>` (`ci/packs.sh publish`, con `oras`, o con su imagen si no está instalado).
- **Publicación automática:** el trabajo `publish` de `ci.yml`, tras pasar las comprobaciones en `main`, publica los packs que cambian y, si el push toca algo que entra en ella (no solo documentación o packs), la imagen, etiquetados `<versión>-<commit>` con la versión del `Cargo.toml`. Para sacar una versión basta subirla en `[workspace.package]`: el workflow añade `<versión>` a packs e imagen, `latest` a la imagen y crea la etiqueta de git `v<versión>`. Necesita el secreto `GEKO_PACK_SIGNING_KEY`.
- `make release`: compila, hace la imagen y la sube a `ghcr.io/edgaza/geko` con tres etiquetas: la versión y el commit (`0.1.0-6fb024c`), la versión y `latest`. Solo con todo en commit, para que la imagen sea la de ese commit, y tras `docker login ghcr.io` con un token con permiso `write:packages`.

Compilan `geko` en `--release` y lo instalan antes de usarlo (`make install`, con `sudo`): el binario en `/usr/local/bin/geko`, el `geko.toml` del repositorio en `/etc/geko/geko.toml` si aún no hay ninguno, y `/var/lib/geko` a nombre del usuario. El `/etc/geko/geko.toml` es el que se edita (ámbito, `accept_licenses = true`…); el del repositorio se queda con `accept_licenses = false`. El puerto se cambia con `LISTEN=…`.

### Prueba local (Islas Baleares), desde PowerShell

Primero, la ruta del repositorio en WSL, por ejemplo `$GEKO_REPO = '/mnt/d/geko'` para `D:\geko`. Desde la línea de órdenes:

```powershell
wsl -e bash -lc "cd '$GEKO_REPO' && ~/.cache/geko-target/release/geko query 'carrer de sant miquel 30, palma'"
wsl -e bash -lc "cd '$GEKO_REPO' && ~/.cache/geko-target/release/geko reverse 39.5675 2.6482"
```

Como servicio HTTP (deja la consola ocupada; se para con Ctrl+C):

```powershell
wsl -e bash -lc "cd '$GEKO_REPO' && ~/.cache/geko-target/release/geko serve"
```

Y desde otra consola o el navegador:

```
http://127.0.0.1:8080/v1/search?text=carrer de sant miquel 30, palma
http://127.0.0.1:8080/v1/autocomplete?text=valldem
http://127.0.0.1:8080/v1/reverse?lat=39.5675&lon=2.6482
http://127.0.0.1:8080/v1/place/geko:municipality:es-07040
http://127.0.0.1:8080/health
```
