# geko — motor de geocodificación y teselas

## 1. Objetivo

Motor de geocodificación (directa, autocompletado e inversa) y servidor de teselas vectoriales que supere a Pelias en **velocidad**, **cobertura de datos** y **calidad de resultados**, medido con el protocolo de la sección 13.

Ámbito inicial: **España**, con el diseño preparado desde el principio para **varios países** (sección 2.4).

Todo el ciclo es **automático**: se declaran en `geko.toml` el ámbito, los módulos y los providers, y geko descarga los datos necesarios, genera los índices y los sirve (sección 7).

### 1.1 Objetivos medibles (iniciales, a revisar tras la primera medición)

Hardware de referencia del nodo de servicio: **Linux, 4 vCPU, 8 GB de RAM, disco NVMe**.

| Métrica | Objetivo |
|---|---|
| Latencia en servidor, autocompletado | p99 < 5 ms |
| Latencia en servidor, búsqueda | p99 < 15 ms |
| Latencia en servidor, inversa | p99 < 5 ms |
| Caudal de autocompletado manteniendo su p99 (sin caché de respuestas) | ≥ 2000 QPS |
| RAM de la parte caliente (sección 9.6) del segmento de España, sin capa de parcelas | ≤ 4 GB |
| Calidad (top-1 en las consultas de referencia) | ≥ Pelias + 10 puntos en total, y nunca peor que Pelias en ninguna categoría, en cada país (instancia de referencia de §13.2) |

### 1.2 Diferencias frente a Pelias y Photon

- Portales de fuentes oficiales nacionales (en España: CartoCiudad, Catastro y callejeros autonómicos), no solo los de OSM.
- **Una única fuente de verdad**: todas las fuentes se funden en un registro maestro por entidad, sin duplicados y con la procedencia de cada dato (sección 6.5).
- Toponimia rural (en España: Nomenclátor del IGN y del INE).
- Búsqueda por referencia catastral, por polígono y parcela, por punto kilométrico y por coordenadas en varios formatos.
- Nombres antiguos de calles como alias.
- Geocodificación por lotes y búsqueda estructurada, con nombres de parámetros como los de Pelias.
- Índice jerárquico propio por segmentos de país, con latencias de un orden de magnitud menores.
- Descarga, generación y despliegue automáticos e incrementales.

### 1.3 No objetivos (por ahora)

- Enrutamiento.
- Actualización por minuto. Se regenera por lotes (con diffs diarios de OSM en la fase 5).
- Cobertura mundial con la misma calidad que en los países con pack propio.

## 2. Principios

### 2.1 Abstracción selectiva

Se abstrae **solo donde hay una alternativa realista o la decisión técnica está abierta**. Un trait diseñado antes de tener dos implementaciones tiende a copiar la forma de la primera y deja de servir para cambiarla. Por eso:

- Los puertos de la sección 4 son los únicos traits de arquitectura. Cada tecnología que los implementa es un *provider* en su propio crate.
- Lo demás (fusión de fuentes, resolución administrativa, transporte HTTP, almacenamiento de bundles) son módulos normales. **Su trait se extrae cuando aparezca una segunda implementación real**, no antes.
- En la generación, la frontera principal no es un trait sino un **contrato de datos**: esquemas Parquet versionados (sección 5).

Reglas:

1. `geko-core` **no depende** de ninguna tecnología concreta. Solo tipos de dominio, traits y lógica pura.
2. Cada provider genérico (válido para cualquier país) vive en `providers/<puerto>-<tecnología>`. Lo propio de un país vive en su pack, `countries/<país>/` (sección 2.4). Los providers se compilan detrás de un *feature* de Cargo; un pack no se compila: es datos que geko lee al arrancar.
3. Los traits solo exponen **tipos de dominio**. Ningún tipo de un provider (p. ej. `fst::Map`) cruza la frontera.
4. Los providers se eligen **por configuración** (`geko.toml`) en la raíz de composición (`geko-app`). Ningún otro sitio los instancia. Hoy hay uno compilado por puerto; elegir entre varios por configuración llegará con la segunda implementación.
5. Cada puerto tiene una **suite de conformidad** que toda implementación debe superar.
6. Cada artefacto de un bundle lleva en el manifiesto qué provider y qué versión de formato lo generó. El servidor rechaza artefactos incompatibles con los providers configurados.

### 2.2 Rendimiento y abstracción

- La frontera de un trait se cruza **una vez por consulta y segmento**, nunca una vez por documento o por *posting*. Los bucles calientes viven dentro del provider.
- **No se separan recuperación, filtrado geográfico y ranking en puertos distintos.** La velocidad sale de hacerlo todo en una sola pasada (sección 9.4). Por eso el motor de búsqueda es un único puerto (`GeocodeEngine`) que devuelve ya los k mejores, y los pesos del ranking entran como configuración.
- Se usa `Arc<dyn Trait>` en la composición. Si una medición demuestra que la indirección importa, ese puerto pasa a genérico sin cambiar su contrato.
- Los traits escriben en buffers que aporta quien llama (`&mut Vec<Hit>`) y devuelven ids numéricos y referencias, no estructuras asignadas por resultado.

### 2.3 Módulos activables: `geocoder` y `tiles`

El motor tiene dos módulos funcionales independientes. Cada uno se puede activar o desactivar sin afectar al otro:

| Módulo | Incluye | Puertos que usa |
|---|---|---|
| `geocoder` | Búsqueda, autocompletado, inversa, lotes, `/v1/*` y modos compatibles | `SourceProvider`, `Downloader`, `TextNormalizer`, `AddressParser`, `GeocodeIndexBuilder`, `GeocodeEngine`, `ReverseIndex` |
| `tiles` | Generación y servicio de teselas, estilo, fuentes tipográficas e iconos, `/tiles/*` | `TileBuilder`, `TileSource`, `Downloader` (y `SourceProvider` si el profile incluye fuentes extra) |

Comunes a ambos: descarga de datos, almacenamiento y descarga de bundles, transporte HTTP, `/health` y `/metrics`.

Reglas:

1. **Un módulo desactivado no consume recursos.** No se construyen sus providers, no se descargan sus datos, no se abren ni se mapean sus ficheros, no se registran sus rutas HTTP, no se lanzan sus tareas de fondo y no se reserva memoria para sus cachés.
2. **Dos niveles de activación:**
   - **Compilación** (*features* de Cargo `geocoder` y `tiles`, ambas activas por defecto): un binario compilado sin `tiles` no contiene ni el código ni las dependencias de teselas.
   - **Ejecución** (`[modules]` en `geko.toml`, o `--modules` en la CLI). Si se activa un módulo que no está compilado, el arranque falla con un error claro.
3. **Al menos un módulo activo.** Si los dos están desactivados, el arranque falla.
4. **La descarga y la generación respetan los módulos.** Con solo `tiles`, no se descargan ni se procesan las fuentes de direcciones. Con solo `geocoder`, no se descarga ni se lanza Planetiler (y no hace falta Java).
5. **Bundles parciales.** El manifiesto declara qué módulos, países y capas contiene. `geko serve` exige que el bundle contenga lo activo y no abre el resto.
6. **Independencia de código.** Ningún módulo depende del otro. Lo compartido va a `geko-core` o a los crates comunes.
7. **Cambio de módulos.** Activar o desactivar un módulo exige reiniciar el proceso. El cambio de bundle en caliente solo recarga lo activo.
8. **Teselas sin servidor.** Como alternativa a activar `tiles` en `geko serve`, el `.pmtiles` y los recursos de estilo del bundle se pueden publicar tal cual en almacenamiento de objetos o en un servidor estático, y el cliente lee las teselas por rangos HTTP. En ese caso el módulo `tiles` solo se usa en la generación.

La misma regla se aplica a otros dos niveles más finos:

- **Capas opcionales** de gran tamaño dentro de `geocoder` (sección 5.4, p. ej. `parcels`).
- **Países servidos**: `[serve] countries` elige qué segmentos de país carga cada nodo (sección 2.4).

```rust
pub trait Module: Send + Sync {
    fn name(&self) -> ModuleName;
    fn load(&self, bundle: &BundleView) -> Result<()>;
    fn router(&self) -> Router;
    fn shutdown(&self) -> Result<()>;
}
```

### 2.4 Varios países

- **Códigos ISO.** El ámbito se declara con ISO 3166-1 (país) e ISO 3166-2 (región). Cada provider traduce esos códigos a los suyos (INE en España, INSEE en Francia…).
- **Fuentes globales y nacionales.** Las globales (OSM, Wikidata, Overture, Natural Earth) sirven a todos los países. Las nacionales (CartoCiudad y Catastro en España, BAN en Francia, BAG en Países Bajos…) solo al suyo. Cada provider declara su cobertura con `coverage()`.
- **Pack de país**, en `countries/<país>/` (código ISO 3166-1 en minúsculas). Contiene todo lo propio de un país; el motor y los providers de `providers/` son genéricos. Añadir un país es crear su pack, sin tocar el motor:

  ```
  countries/es/
    pack.toml          país, versión del pack, lenguas y formato de CP
    data/              datos, no código
      street-types.toml    tipos de vía por código común, con sus palabras en cada lengua del país
      words.toml           conectores, marcas de número, palabras de piso y puerta, por lengua
      equivalences.txt     equivalencias de palabras entre lenguas aprendidas con todo el país, y
                           pares descartados (sección 9.5)
      admin-levels.toml    admin_level de OSM → PlaceKind en ese país
      regions.toml         regiones: las particiones de la generación (provincias) y las que las agrupan
      region-boxes.tsv     generado: una caja que contiene cada partición, para descargar solo lo del ámbito
    sources/<fuente>.toml  lo que una fuente genérica necesita saber del país (los extractos de
                       OSM…) o, con `kind`, una fuente nacional declarada para el provider
                       genérico de ese tipo (CartoCiudad, `kind = "ogc-api-features"`)
    golden/            consultas de referencia del país, su línea base (golden/baseline/) y su
                       instantánea de datos (golden/snapshot); no se publican con el pack
    Cargo.toml, src/, tests/   crate country-pack-<país>, solo para los tests
  ```

  - **Motores genéricos:** normalizador, analizador y equivalencias son uno solo, en `providers/`, y leen los datos del pack.
  - **Publicación y descarga:** cada pack se publica firmado (ed25519, `signature.json` con el hash de cada fichero) como artefacto OCI en `ghcr.io/candymaps/geko/countries/<país>`, con la etiqueta del formato (`format-2`), la de su versión (`pack-<n>`, el `version` de `pack.toml`, que se sube a mano en cada cambio del pack) y la del commit (`ci/packs.sh publish`, `.github/workflows/ci.yml`, trabajo `publish`, con la versión del `Cargo.toml` como la imagen). La publicación se niega, sin subir ningún pack, si uno cambió con una versión ya publicada. `geko plan`, `geko build` y `build_report.json` muestran la versión del pack de cada país. El formato sube cuando un geko anterior ya no puede leer los packs (el 2 añadió `version`): cada geko sigue bajando el último pack de su formato. `geko plan`, `fetch`, `build` y `up` bajan los del ámbito que falten o hayan cambiado, y los rechazan si la firma no es de una clave que el binario conoce (`TRUSTED_KEYS`, una lista para poder cambiar de clave) o si son de un formato más nuevo. Errores claros, antes de descargar fuentes, para un país sin pack publicado y para `offline` sin el pack; con el registro inalcanzable se usa la copia en disco. Un pack copiado a mano (sin `.digest`) se usa tal cual. El bundle lleva el pack con que se generó (`segments/<país>/pack/`) y `serve` lee ese. `geko purge country-packs` borra los packs descargados de países fuera del ámbito.
  - **Carga:** el pack es datos, no código. `geko` lo lee al arrancar de la carpeta de los packs (`/var/lib/geko/packs/<país>/`, o la de `GEKO_PACKS`, que nunca se descarga), solo para los países que pide, y el hash de sus ficheros forma parte de la versión de código de las etapas que los usan (sección 8.3). Un binario sirve así para cualquier combinación de países sin llevar dentro los que no se usan. Las comprobaciones leen los packs del repositorio; `ci/packs.sh` copia a una carpeta solo lo que `geko` lee.
  - **País sin pack:** no se puede generar: falta su normalizador y su analizador. El error lo dice antes de descargar nada.
  - **Fuentes nacionales:** son declaraciones, no código. CartoCiudad es `sources/cartociudad.toml` para el provider genérico de OGC API Features (`providers/source-ogc-features`), con la URL, la colección, la propiedad por la que se recorre cada partición y las columnas del staging. Una fuente que no quepa en un tipo genérico obliga a añadir el tipo al motor, no código al pack. La fusión lee el staging de direcciones de cualquier fuente con un esquema común (§5.2) y los niveles administrativos de OSM del pack (`data/admin-levels.toml`).
- **Segmento por país** (sección 9.1): cada país es una unidad independiente de generación, despliegue, carga y escala.
- **Calidad por país**: consultas de referencia y objetivos por país. Mejorar un país nunca debe empeorar otro.

## 3. Arquitectura

La generación y el servicio corren en **máquinas distintas**. La generación necesita mucha RAM y CPU durante minutos u horas (fusión de fuentes, Planetiler). El servicio necesita poca. Así, la máquina de servicio no se dimensiona para el pico de la generación.

```
 ┌────────────────────────── nodo de generación (CI o efímero) ───────────────────────────┐
 │                                                                                        │
 │  geko.toml ─► plan (módulos activos → providers → datos por país/región)               │
 │                 │                                                                      │
 │                 ▼                                                                      │
 │  Downloader ─► raw/<fuente>/  (caché, ETag, sumas de control, licencias)               │
 │                 │                                                                      │
 │                 ▼  por partición (país, región), en paralelo, cacheado por hash        │
 │  SourceProvider ─► staging/<fuente>/<país>/<región>/*.parquet  (EPSG:4326)             │
 │                 │                                                                      │
 │                 ▼                                                                      │
 │  fusión (Rust): admin por coordenadas, resolución de entidades, prioridades,         │
 │                   importancia, ids ─► canonical/<país>/*.parquet                       │
 │                 │                                                                      │
 │       ┌─────────┼──────────────────────────┐                                           │
 │       ▼         ▼                          ▼                                           │
 │  GeocodeIndexBuilder   ReverseIndex   TileBuilder (Planetiler) + estilo, glyphs, sprites│
 │   (un segmento por país)                                                               │
 │                 ▼                                                                      │
 │  bundle/vN/ (segmentos por país + teselas + manifest.json) ─► publicación              │
 └────────────────────────────────────────────────────────────────────────────────────────┘
                                    │ descarga solo de lo que cambió
 ┌────────────────────────────── nodo de servicio (Linux) ────────────────────────────────┐
 │  BundleManager: descarga, valida, mmap (caliente fijado, frío bajo demanda), ArcSwap   │
 │                                                                                        │
 │  HTTP ─► caché de respuestas ─► pista de país ─► AddressParser(es) ─► segmentos        │
 │                                                      GeocodeEngine por segmento        │
 │                                                      mezcla de los k mejores ─► JSON   │
 │          ReverseIndex + admin por polígonos  ·   TileSource                           │
 └────────────────────────────────────────────────────────────────────────────────────────┘
```

### 3.1 Comandos

| Comando | Hace |
|---|---|
| `geko plan` | Muestra qué se descargaría y generaría (fuentes, URLs, tamaños, licencias) sin hacer nada |
| `geko fetch` | Solo descarga los datos necesarios |
| `geko build` | Descarga lo que falte o haya cambiado y genera el bundle (solo las particiones afectadas) |
| `geko serve` | Sirve un bundle. **Nunca descarga fuentes**, solo bundles ya generados |
| `geko up` | `serve` y, en segundo plano, `build`: ahora y cada `[data] max_age_days`, sin cron. Sirve el bundle publicado mientras genera el nuevo y lo recoge al terminar, con la recarga de siempre; si un build falla, sigue con el que tiene y reintenta a la hora. Con `offline` o `refresh = never` genera una sola vez. Es la orden de la imagen de Docker |
| `geko purge [data | packages [--all]]` | Borra lo que la configuración ya no pide: descargas y ficheros de trabajo, y paquetes de datos que bajaron los módulos (los packs de país del geocoder) fuera del ámbito;| country-packs [--all]]` | Borra lo que la configuración ya no pide: descargas y ficheros de trabajo, y packs descargados de países fuera del ámbito; sin subcomando, todo. `build --purge` y `up --purge` lo hacen antes de generar |
| `geko <módulo> <orden>` | Las órdenes de un módulo activo; `geko <módulo>` las lista. Las del geocoder son de depuración: `geko geocoder parse`, `query` y `reverse` (interpretaciones de una consulta, búsqueda con tiempos e inversa, sobre el segmento generado) |

### 3.2 Plataformas

El nodo de servicio de producción es Linux (las optimizaciones de la sección 11 son de Linux). El desarrollo puede hacerse en Windows o WSL; allí las optimizaciones específicas se desactivan sin error.

## 4. Puertos (traits) y providers

| Puerto | Responsabilidad | Provider inicial | Alternativas realistas |
|---|---|---|---|
| `Downloader` | Descargar ficheros con caché, reanudación, ETag, reintentos y sumas de control | `geko-fetch` (HTTP) | descargador falso para tests, espejo local |
| `SourceProvider` | Declarar cobertura y datos necesarios, descargarlos con el `Downloader` y escribir sus tablas de staging | uno por fuente (sección 6) | fuentes de otros países |
| `TextNormalizer` | Normalizar y generar variantes de texto de un país (tildes, abreviaturas, artículos, cooficiales, claves fonéticas) | `text-rules` (`RulesNormalizer`), genérico, con los datos del pack de cada país (`countries/es/`) | `normalizer-generic`, para países sin pack |
| `AddressParser` | Convertir la consulta en interpretaciones puntuadas según el formato de un país | `text-rules` (`RulesParser`), genérico, con los datos del pack de cada país | `parser-libpostal` |
| `GeocodeIndexBuilder` / `GeocodeEngine` | Construir y consultar **un segmento** del índice; devolver sus k mejores con ranking y filtro incluidos | `geocode-fst` (motor propio, sección 9) | `geocode-tantivy`, otros |
| `ReverseIndex` | Vecino más cercano, búsqueda por caja y resolución administrativa de un punto | `reverse-rstar` | `reverse-s2` |
| `TileBuilder` | Generar el tileset y sus recursos de estilo | `tiles-planetiler` (proceso Java externo) | tilemaker |
| `TileSource` | Servir teselas `z/x/y`, TileJSON, `style.json`, glyphs y sprites | `tiles-pmtiles` | `tiles-mbtiles` |

`TextNormalizer` y `AddressParser` se registran **por país**. Un segmento se genera con el normalizer de su país.

**Sin trait (módulos normales hasta que haya una segunda implementación):**

- Fusión de fuentes, resolución de entidades, jerarquía administrativa, ids e importancia: módulo `fusion` de `geko-build` (Rust).
- Mezcla de resultados entre segmentos, pista de país y caché de respuestas: `geko-module-geocoder`.
- Transporte: axum en `geko-server`.
- Almacenamiento de bundles: sistema de ficheros. Se extraerá `BundleStore` al añadir S3.

Contratos orientativos:

```rust
pub trait Downloader: Send + Sync {                                          // síncrono; gestiona caché, refresco y offline
    fn fetch(&self, req: &DownloadRequest, dest: &Path) -> Result<DownloadOutcome>;   // {downloaded, sha256, size}
}

pub trait SourceProvider: Send + Sync {                                       // construcción barata y sin E/S
    fn id(&self) -> SourceId;
    fn license(&self) -> License;
    fn coverage(&self) -> Coverage;                                           // Global | Countries(&[CountryCode])
    fn used_by(&self) -> &'static [ModuleName];                               // módulos que la necesitan
    fn plan(&self, scope: &Scope, dl: &dyn Downloader, raw: &RawDir)          // qué necesita, con URL y tamaño estimado;
        -> Result<Vec<DatasetRequest>>;                                       // solo baja metadatos pequeños (índices)
    fn fetch(&self, scope: &Scope, dl: &dyn Downloader, raw: &RawDir) -> Result<FetchReport>;
    fn stage(&self, partition: &Partition, raw: &RawDir, out: &StagingDir) -> Result<SourceStats>;
    fn stage_by_country(&self) -> bool;                                       // una partición por país (Overture)
    fn stage_inputs(&self, partition: &Partition, raw: &RawDir) -> Result<Vec<PathBuf>>;  // §8.3
    fn unused_raw(&self, scope: &Scope, raw: &RawDir) -> Result<Vec<PathBuf>>;            // geko purge
}

pub trait GeocodeIndexBuilder: Send {
    fn format(&self) -> IndexFormat;
    fn build(&self, data: &CanonicalPartition, normalizer: &dyn TextNormalizer, dir: &Path) -> Result<IndexManifest>;
}

pub trait GeocodeEngine: Send + Sync {
    fn country(&self) -> CountryCode;
    fn search(&self, q: &ParsedQuery, ctx: &QueryContext, out: &mut Vec<Hit>) -> Result<()>;
    fn autocomplete(&self, q: &ParsedQuery, ctx: &QueryContext, out: &mut Vec<Hit>) -> Result<()>;
    fn place(&self, id: LocalId) -> Option<PlaceRef<'_>>;
    fn resolve(&self, id: &PublicId) -> Option<LocalId>;
}

pub trait ReverseIndex: Send + Sync {
    fn country(&self) -> CountryCode;
    fn nearest(&self, p: Coord, filter: KindFilter, k: usize, out: &mut Vec<Hit>) -> Result<()>;
    fn admin_at(&self, p: Coord) -> Option<AdminChain<'_>>;
}

pub trait TileSource: Send + Sync {
    fn tile(&self, z: u8, x: u32, y: u32) -> Result<Option<TileBytes<'_>>>;
    fn asset(&self, path: &AssetPath) -> Result<Option<AssetBytes<'_>>>;
    fn metadata(&self) -> &TileMetadata;
}
```

- `ParsedQuery` contiene varias interpretaciones con su probabilidad y el país del parser que las produjo.
- `QueryContext` lleva foco, límites, países, idioma, tamaño k, pesos del ranking, `debug` y **`min_score`**: la puntuación del k-ésimo resultado ya encontrado en otros segmentos o interpretaciones, para que el motor pare antes (sección 9.4).
- `Hit` lleva id (segmento + id local), puntuación y `match_type`.

### 4.1 Configuración

La configuración está siempre en `/etc/geko/geko.toml`, con la de cada módulo a su lado (`geko-geocoder.toml`, o el fichero que diga `[modules.<módulo>] config`), y los datos siempre en `/var/lib/geko`; ninguna de las dos rutas se cambia desde el fichero ni desde la línea de órdenes. Las variables `GEKO_CONFIG` y `GEKO_DATA` las cambian para las comprobaciones y la CI. `geko build --purge` borra antes de generar las descargas y los ficheros de trabajo de los países, regiones y fuentes que la configuración ya no pide.

```toml
# geko.toml (todas las opciones, con sus valores por defecto, en el geko.toml del repositorio)

[modules]
geocoder = true                 # o una tabla: { enabled = true, config = "geko-geocoder.toml" }
tiles    = true

[[scope]]
country = "ES"
regions = ["ES-B"]            # ISO 3166-2, una partición o una región que las agrupa; vacío = país entero

# [[scope]]
# country = "LU"

[data]
refresh         = "if-changed"  # never | if-changed | always
max_age_days    = 30            # días entre dos comprobaciones de todas las fuentes
keep_downloads  = false         # true: conservar las descargas tras publicar
offline         = false         # true: no descargar nada; fallar si falta algo
accept_licenses = false         # debe ponerse a true para descargar (sección 7.5)
parallel_downloads = 4

[build]
keep = 1                        # versiones publicadas que se conservan (2 para poder volver atrás)

[serve]
listen    = "0.0.0.0:8080"
reload_secs = 30                # cada cuánto se mira si hay un bundle nuevo
cors_origins           = []     # orígenes desde los que una página puede llamar a la API ("*": cualquiera)
api_keys               = []     # claves que exige cada petición (Authorization: Bearer, X-Api-Key o api_key=); vacío: abierta
rate_limit_per_s       = 0      # peticiones por segundo de una misma dirección de cliente (0: sin límite)
rate_limit_burst       = 0      # ráfaga por encima de ese ritmo (0: el doble del ritmo)
bundles_url            = ""     # dirección HTTP de los bundles publicados en otra máquina; cada recarga trae solo lo que cambió
attribution_contact    = ""     # dónde pedir el código de la base derivada (ODbL 4.6), en /v1/attribution
```

Las opciones del geocoder van en su fichero; si alguna sigue en `geko.toml`, el arranque falla diciendo que se mueva:

```toml
# geko-geocoder.toml (todas, con sus valores por defecto, en modules/geko-geocoder/geko-geocoder.toml)

# Fuentes: las compiladas y las que declaran los packs del ámbito se usan si cubren algún país
# del ámbito; sus apartados son opcionales y solo cambian opciones o las desactivan. OSM es
# obligatoria (enabled = false es un error).
[sources.osm]
mirror  = "auto"                # o URL de un espejo con la estructura de Geofabrik

[sources.overture]
enabled = true                  # POIs de Overture Maps Places
min_confidence = 0.8

[sources.cartociudad]           # declarada por el pack de España: cualquier campo de su declaración
max_unchanged_days = 365

[packs]
registry = "ghcr.io/candymaps/geko/countries"   # de dónde se descargan los packs de país

[serve]
countries = []                  # vacío = todos los del bundle
preload   = true                # fijar en RAM la parte caliente (sección 9.6)
search_budget_ms       = 15     # presupuesto de tiempo de una búsqueda (0: sin límite)
autocomplete_budget_ms = 5      # y de una petición de autocompletado; la lectura más probable siempre se completa
slow_query_ms          = 50     # las peticiones más lentas se registran con su consulta (0: ninguna)
same_entity_m          = 100    # resultados con el mismo nombre a menos de esto se funden en la respuesta (6.5.6)
batch_max              = 100    # peticiones como mucho en un /geocoder/v1/batch
cache_entries          = 0      # respuestas de búsqueda y autocompletado guardadas, unos 2,5 KB cada una (0: ninguna)
```

Previstos y sin implementar: elegir provider por configuración (`[build] geocode`, `[serve] geocode`…), capas opcionales (`[serve.layers] parcels`) y pesos del ranking (`[serve.ranking]`).

`geko-module-geocoder` mantiene un registro de factorías por puerto, y otro de providers genéricos por tipo de servicio para las fuentes que declaran los packs (`kind`). Un provider solo se registra si su *feature* está compilado, y solo se instancia si lo necesita un módulo activo y cubre algún país del ámbito. Las fuentes compiladas se usan por defecto, sin apartado en `geko-geocoder.toml`; el apartado solo cambia sus opciones o las desactiva. Las fuentes obligatorias (OSM, la base de todos los países) no se pueden desactivar.

## 5. Contratos de datos (generación)

Todo se **particiona por país y región**, y cada partición se identifica por el hash de sus entradas (sección 8.3). Los esquemas Parquet están versionados en `schemas/`.

### 5.1 Coordenadas

Todas las coordenadas se guardan en **punto fijo**: dos `i32` en unidades de 1e-7 grados (precisión de ~1 cm, 8 bytes por punto). Es el tipo `Coord` de `geko-core`. `f64` solo se usa para cálculos intermedios.

### 5.2 Staging

`staging/<fuente>/<país>/<región>/*.parquet`, escrito por el `SourceProvider` directamente con Arrow (compresión zstd), sin formatos intermedios. Datos de la fuente ya en tipos comunes y **reproyectados a EPSG:4326**, sin fusionar. La reproyección es responsabilidad del provider (p. ej. Catastro INSPIRE viene en EPSG:25829, 25830 o 25831 según la zona, y en EPSG:32628 en Canarias).

Las fuentes de direcciones que no son OSM escriben sus portales con un esquema común (`schemas/staging/addresses.md`): identificador, vía, número, extensión, CP, población y coordenadas, con los números ficticios de la fuente ya convertidos en «sin número». La fusión los lee sin código propio de cada fuente, solo de las fuentes activas, y toma la posición y el CP de una fuente nacional antes que de OSM. Una fuente nacional nueva no toca el motor.

### 5.3 Canónico

`canonical/<país>/*.parquet`. Salida de la fusión y entrada de todos los índices. Esquema en `schemas/canonical.md` (`canonical/1`).

| Tabla | Contenido |
|---|---|
| `places` | Niveles administrativos (comunidad, provincia, municipio, barrio), localidades y topónimos: nombres multilingües, punto, población, código oficial, CP y límites |
| `streets` | Vial **agregado** (sección 6.5): nombres, alias multilingües, tipo de vía, municipio, entidad de población, CP con su intervalo de números, geometría, importancia |
| `addresses` | Portal: vial, número, sufijo, rango, CP con su procedencia, coordenadas. Los puntos kilométricos son portales de su carretera |
| `pois` | Nombre, categoría, municipio, dirección inferida, coordenadas, importancia |
| `postcodes` | Código postal, centroide, municipio principal y número de portales |
| `postcode_cells` | Celda H3 (resolución 9) → código postal mayoritario (zonas de CP, sección 6.5.4) |
| `conflicts` | Atributos en los que las fuentes discrepan: el valor elegido y el descartado (sección 6.5.5) |

Cada entidad lleva sus `sources` y `source_ids`.

### 5.4 Capas opcionales

| Tabla | Contenido | Motivo de ser opcional |
|---|---|---|
| `parcels` | Referencia catastral, municipio, polígono y parcela si es rústica, centroide | Decenas de millones de filas: más de 1 GB adicional por país. Se sirve como parte fría |

### 5.5 Identificadores

- **Id público estable y determinista** (`PublicId`, texto `geko:<tipo>:<país>-<clave>`), igual entre regeneraciones mientras la entidad exista. `<país>` es el código ISO 3166-1 en minúsculas. Claves en España:
  - niveles administrativos: código oficial (INE o IGN);
  - vial: código INE del municipio + nombre normalizado principal;
  - portal: clave del vial + número + sufijo;
  - POI: fuente + id de origen;
  - punto kilométrico: carretera + km;
  - parcela: referencia catastral.
- **Id interno**: (segmento, `LocalId`). `LocalId` es un `u32` denso que asigna la fusión y se guarda en el canónico (`local_id`), para que el motor de búsqueda y la inversa compartan los mismos ids: primero las entidades buscables **en orden de importancia descendente** (id 0 = la más importante del país), después los portales agrupados por calle y ordenados por número.
- Cada segmento guarda la tabla `PublicId ↔ LocalId`. El país del `PublicId` indica directamente el segmento.

## 6. Fuentes y fusión

### 6.1 Fuentes

**Globales:**

| Fuente | Aporta | Licencia | Fase |
|---|---|---|---|
| OSM (Geofabrik) | Viales, POIs, geometría, nombres, `old_name`, wikidata, algunos portales | ODbL 1.0 | 1 |
| Wikidata | Importancia (enlaces), nombres multilingües | CC0 | 2 (sin hacer: la importancia sale de los nombres multilingües de OSM, §6.3) |
| Overture Maps Places | POIs | CDLA-Permissive 2.0 | 3 (hecho, 2026-10-18: `providers/source-overture`) |
| Natural Earth y polígonos de agua (para teselas) | Contexto cartográfico | Dominio público / ODbL | 3 |

**Nacionales, España:**

| Fuente | Aporta | Licencia | Fase |
|---|---|---|---|
| CartoCiudad (IGN, por la OGC API del IDEE) | Portales, viales, códigos postales, puntos kilométricos | CC BY 4.0 | 1 (hecho) |
| IGN | Límites administrativos oficiales | CC BY 4.0 | 2 (no compensa por ahora: OSM los trae) |
| INE | Códigos de municipio, población, Nomenclátor (entidades singulares y núcleos) | Reutilización con atribución | 2 (no compensa por ahora: el 98,5 % de los municipios ya los trae de OSM) |
| Nomenclátor Geográfico Básico (IGN) | Topónimos: parajes, pedanías, accidentes geográficos | CC BY 4.0 | 2 (llega en buena parte por su importación a OSM) |
| Catastro INSPIRE (DGC) | Direcciones, edificios y parcelas. **No cubre Navarra ni el País Vasco** | Uso libre con atribución | 2 (aplazado: `countries/es/README.md`) |
| Catastros forales (Navarra, Bizkaia, Gipuzkoa, Araba) | Direcciones y parcelas de esos territorios | Según cada organismo | 2 |
| Callejeros autonómicos (CDAU de Andalucía, ICGC de Cataluña, otros) | Viales y portales de alta calidad | Según cada organismo | 2 |
| Registros oficiales de POIs (centros sanitarios y educativos, gasolineras, farmacias) | POIs con categoría fiable | Según cada organismo | 3 |

Antes de incorporar una fuente se comprueba y se anota su licencia en el provider (`license()`).

### 6.2 Fusión

Se hace **en Rust**, en el módulo `fusion` de `geko-build`, por país, con `rayon` para el paralelismo. Se descartó SQL de DuckDB: la resolución de entidades es procedimental (*union-find*, puntuación de pares, reglas por tipo), la normalización para comparar es Rust del pack de país, y el crate `duckdb` compila DuckDB entero en C++. DuckDB queda para inspeccionar y validar los Parquet. Pasos:

1. **Asignación administrativa por coordenadas**: R-tree de las cajas de los límites y comprobación exacta con polígonos preparados (aristas indexadas por franjas de latitud). Asignar 1,7 millones de puntos lleva unos segundos.
2. **Resolución de entidades y registro maestro** (sección 6.5): agregación de tramos de vial, emparejamiento entre fuentes, agrupación y elección de cada atributo. Sale una sola entidad por cada cosa del mundo real.
3. **Atributos inferidos** (sección 6.5.4): zonas de CP, CP de portales, POIs y lugares, dirección de POIs.
4. **Importancia** (sección 6.3) e **ids** (sección 5.5).
5. **Validación** (secciones 13.4 y 13.6). Si la tasa de duplicados supera su umbral, el build falla después de escribir el canónico y el informe, para poder revisarlos.

#### Resultados en la provincia de Barcelona (OSM + CartoCiudad)

| | Valor |
|---|---|
| Registros de entrada | 1.165.270 portales de CartoCiudad, 284.980 de OSM, 145.394 tramos de vial, 114.744 POIs, 16.808 lugares |
| Entidades | 1.183.679 portales, 83.926 calles, 109.876 POIs, 16.898 lugares, 390 CP |
| Portales con las dos fuentes | ≈ 243.000 (≈ 85 % de los de OSM) |
| Portales que solo están en OSM | ≈ 20.600 |
| CP de portales | 98,3 % de CartoCiudad, 1,3 % de OSM, 0,4 % inferido |
| Duplicados | portales 0,000 %, calles 0,052 % |
| Tiempo de fusión | ≈ 11 s |

Hallazgos de los datos reales que fijan las reglas:

- CartoCiudad (datos del ICGC) escribe "PASEO" tanto para "Passeig" como para "Passatge": `PS` y `PJ` son compatibles en España (`street_types_compatible` del pack de país).
- Las calles de ciudad aparecen interrumpidas en OSM por plazas, parques o vías de tren (Carrer de Mallorca en Sants): los tramos con el mismo nombre y **exactamente** el mismo tipo se unen hasta 500 m. Entre tramos de OSM el tipo es explícito: "Carrer de Sants" y "Plaça de Sants" son calles distintas.
- La mayoría de direcciones que no casaban con su calle estaban a 150–300 m de su línea: el límite para un nombre exacto es 300 m (150 m para nombres aproximados). Más allá de 600 m suelen ser calles homónimas de otro núcleo del mismo municipio.
- Las direcciones de calles sin geometría en ninguna fuente (diseminados, masías, urbanizaciones, barrios rurales…) forman calles propias, agrupando las de todas las fuentes con el mismo nombre y tipos compatibles.

### 6.3 Importancia

Puntuación estática por entidad, normalizada entre 0 y 1 y guardada como `u16`, calculada con pesos configurables:

- administrativos y topónimos: población, enlaces de Wikidata, nivel administrativo;
- viales: número de portales, clase de vía, importancia del municipio;
- POIs: etiqueta de Wikidata, fama e importancia del municipio. La fama es el número de lenguas ajenas al país (las de su pack) en que OSM nombra el POI (`name:<lengua>`): cuánto se le conoce fuera. Sale de los mismos datos en cualquier país. La Sagrada Família tiene 16; un bar, ninguna. Escala de referencia: 15 lenguas;
- portales: heredan la del vial.

### 6.4 Licencias

Fusionar OSM con otras fuentes crea una base de datos derivada sujeta a ODbL (share-alike) si se usa públicamente. Los resultados sueltos de una búsqueda son obra producida: basta con atribuirlos.

**Decisión (2026-10-21):** el código se licencia con AGPL-3.0-only ([LICENSE](../LICENSE)), y toda contribución externa exige firmar antes el CLA ([CLA.md](../CLA.md), Harmony con cualquier licencia de salida), para poder cambiar de licencia o dar licencias comerciales. Publicar el código cumple por sí solo la cláusula 4.6.

**Decisión (2026-10-18), mientras el repositorio siga privado:** la obligación de la cláusula 4.6 se cumple ofreciendo **el método** de hacer la base derivada, no la base: una descripción pública del algoritmo de fusión y el compromiso de dar el código necesario para reproducirla a quien reciba resultados y lo pida. Las fuentes añadidas a OSM ya son públicas.

- `/v1/attribution` (sin clave de API, del host): las licencias y atribuciones de las fuentes del bundle, que lista su manifiesto; a quién pedir el código (`[serve] attribution_contact`), y lo que añade cada módulo: el geocoder, en `geocoder.derivative_database`, el aviso de base derivada y dónde está el método.
- `/geocoder/v1/attribution/method`: la descripción del método ([FUSION_METHOD.md](FUSION_METHOD.md)), embebida en el módulo. Hay que mantenerla al día cuando cambien las reglas de fusión.
- Cada respuesta del geocodificador lleva `geocoding.attribution`, y la página de demostración muestra las atribuciones en el mapa.
- CartoCiudad (CC BY 4.0) permite redistribuir lo derivado con otra licencia, si se mantiene su atribución.

No es asesoría legal: antes de abrir el servicio al público, conviene que lo revise un abogado.

### 6.5 Fusión y deduplicación: una única fuente de verdad

**Requisito:** el canónico contiene **una sola entidad por cada cosa del mundo real**, por muchas fuentes que la describan. Las fuentes no se sirven por separado: se funden en un registro maestro enriquecido con lo mejor de cada una. El usuario nunca ve dos resultados que sean la misma calle, el mismo portal o el mismo lugar.

#### 6.5.1 Normalización para comparar

Antes de comparar, cada registro de staging se lleva a una forma comparable, con el `TextNormalizer` del país:

- **Nombre:** minúsculas, sin tildes, sin signos, sin artículos ni preposiciones de enlace ("de", "del", "de la", "d'", "dels", "de les"…), y abreviaturas expandidas.
- **Tipo de vía separado del nombre**, mediante un **diccionario de tipos de vía entre lenguas** por país, que lleva cada tipo a un código común. P. ej. en España: `calle`, `carrer`, `rúa`, `kalea`, `c/` → `CL`; `avenida`, `avinguda`, `av.`, `etorbidea` → `AV`; `plaza`, `plaça`, `praza`, `plazuela`, `enparantza` → `PZ`; `paseo`, `passeig`, `pasealekua` → `PS`. Se compara el nombre sin el tipo, y el tipo aparte.
- **Número de portal:** número, sufijo (letra, bis, ter) y rango por separado. "12 B", "12B" y "12-B" son lo mismo; "187-189" es un rango que cubre 187 y 189; "s/n" es sin número. Los números ficticios conocidos de cada fuente (p. ej. los `9998`/`9999` de CartoCiudad para portales sin número) se tratan como sin número.
- **Código postal y municipio:** el código oficial del municipio (INE en España) tras la asignación administrativa.

Ejemplo: "AVENIDA DIAGONAL" (CartoCiudad) y "Avinguda Diagonal" (OSM) quedan como tipo `AV` + nombre `diagonal`. "PLAZA MOSSEN JACINT VERDAGUER" y "Plaça de Mossèn Jacint Verdaguer" quedan como `PZ` + `mossen jacint verdaguer`.

Casos límite del tipo de vía:

- **Tipo genérico solo:** no es un nombre. Ocurre con calle, avenida, camino, carretera, travesía, pasaje, urbanización y polígono. Las fuentes lo escriben así cuando falta el nombre: en Baleares, 1.510 portales de CartoCiudad están en "CALLE". Esos portales quedan sin calle.
- **Tipo distintivo solo:** sí es un nombre ("La Rambla", "Ronda", "La Plaza", "El Paseo", "Diseminado").
- **Una letra tras el tipo:** es el nombre ("Calle A"), no un conector.
- **Mismo nombre con y sin tipo:** dentro de una misma fuente de geometría, el tipo es explícito ("Carrer de Sants" y "Plaça de Sants" son calles distintas). Pero un nombre sin tipo ("Retuerta") se une a la calle cercana del mismo nombre con tipo ("Barrio Retuerta") si todas las candidatas tienen el mismo tipo. En Cantabria, OSM nombra así las calles de los pueblos, y la tasa de calles duplicadas bajó del 0,70 % al 0,04 %.

#### 6.5.2 Resolución de entidades

Por tipo de entidad, en tres pasos:

1. **Bloqueo.** Solo se comparan candidatos plausibles: del mismo municipio y de la misma celda H3 o una vecina (resolución según el tipo: más fina para portales, más gruesa para viales y lugares). Así la comparación es casi lineal y escala a decenas de millones de registros.
2. **Puntuación de cada par**, con pesos configurables por tipo:
   - similitud del nombre normalizado (exacta, por palabras y por distancia de edición);
   - coincidencia del tipo de vía, del número y sufijo de portal, del código postal y de la categoría;
   - distancia: punto a punto para portales, POIs y lugares; punto a línea, o línea a línea, para viales;
   - incompatibilidades que anulan el par: números distintos en el mismo vial, municipios distintos, tipos de entidad incompatibles.
3. **Agrupación.** Los pares por encima del umbral se unen con *union-find*. Cada grupo es una entidad. Un grupo con dos registros de la misma fuente que esa fuente considera distintos se revisa: se parte por el par más débil, para no fundir dos entidades reales.

Reglas por tipo:

| Tipo | Se considera la misma entidad si… |
|---|---|
| Tramo de vial (dentro de una fuente) | mismo nombre normalizado y tipo, misma localidad, y tramos conectados o a menos de una distancia configurable |
| Vial (entre fuentes) | misma localidad, nombre normalizado igual o muy similar, tipo compatible y geometrías cercanas |
| Portal | mismo vial ya fundido, mismo número y sufijo, y a menos de una distancia configurable (por defecto 75 m) |
| Lugar y topónimo | nombre igual o muy similar, tipo compatible y cercanía según el tipo (más tolerante para núcleos que para parajes) |
| Límite administrativo | mismo código oficial; si falta, nombre y nivel iguales y solape alto |
| Municipio y su núcleo principal | un nodo de ciudad, villa o pueblo, o una unidad administrativa de nivel de barrio (la *localité* de una comuna de Luxemburgo), con el mismo nombre que el municipio que lo contiene: se funde en el municipio |
| POI (fase 3) | categoría compatible, nombre similar y cercanía; con identificadores externos (wikidata, ids oficiales) cuando existan |

Los registros de una fuente que no emparejan con nada son entidades nuevas: así se añaden los portales y viales que solo están en fuentes oficiales.

#### 6.5.3 Registro maestro

De cada grupo sale una entidad, eligiendo cada atributo por separado:

- **Prioridad por atributo** (configurable por país). En España: límites → IGN; código postal → CartoCiudad; portal y su posición → callejero autonómico o foral, luego CartoCiudad, luego Catastro, luego OSM; geometría de viales y POIs → OSM; población → INE.
- **Si la fuente prioritaria no tiene el atributo**, se toma el de la siguiente.
- **Los nombres se unen, no se eligen.** Todos los nombres y alias de todas las fuentes, deduplicados tras normalizar, quedan como variantes de la entidad. Para mostrar se elige uno por idioma, con la prioridad configurada (p. ej. en Cataluña, el nombre oficial en catalán para `ca` y el castellano para `es`).
- **Conflictos registrados.** Cuando dos fuentes discrepan en un atributo (p. ej. dos códigos postales distintos para el mismo portal), se guarda el elegido y el descartado en `conflicts`, para poder revisarlos.

#### 6.5.4 Atributos inferidos

Algunos atributos que una entidad no trae de ninguna fuente se **deducen** de otras entidades del canónico. Se guardan siempre marcados como inferidos, para no confundirlos con datos de una fuente.

**Código postal**:

| Entidad | De dónde sale el CP |
|---|---|
| Portal fundido con uno de CartoCiudad | Directo de CartoCiudad (prioridad del atributo, 6.5.3) |
| Portal sin gemelo en CartoCiudad | Inferido: CP de los portales de la misma calle con números cercanos del mismo lado (par/impar); si no hay, el de su zona de CP |
| POI y lugar | El de la fuente si lo trae; si no, inferido de su zona de CP |
| Vial | Lista de CP con el intervalo de números de cada uno, a partir de sus portales |
| Municipio, barrio, localidad | Lista de CP que contienen |

**Zonas de CP**: a partir de todos los portales con CP directo, cada celda H3 (resolución 9) recibe el CP mayoritario de sus portales; las celdas sin portales dentro de un municipio reciben el de sus vecinas más cercanas. Las zonas se guardan en el canónico (`postcode_cells`) y la inversa también las usa.

**Dirección de un POI** sin dirección propia: la del portal más cercano, solo si está a menos de una distancia configurable (por defecto 25 m) y en el mismo municipio.

**Municipio y jerarquía administrativa**: siempre por coordenadas, con los límites oficiales; nunca por el nombre de municipio que traiga una fuente (CartoCiudad trae 321 nombres de municipio distintos en una provincia que tiene 311).

En la API, cada atributo que puede inferirse lleva su procedencia, p. ej. `postcode_source: "cartociudad" | "osm" | "inferred"`. Un atributo inferido rebaja ligeramente la `confidence` del resultado.

**Forma del CP en la respuesta**, según el tipo de entidad:

- **Entidades puntuales** (portal, POI, localidad, barrio, topónimo): `postcode` (un solo valor) y `postcode_source`.
- **Entidades que cruzan varios CP** (calle, municipio): **sin `postcode`**, porque no existe un único CP correcto; solo `postcodes`, una lista con una entrada por CP, con su número de portales y su desglose por fuente (`"sources": {"cartociudad": 175, "osm": 3, "inferred": 2}`). En las calles, cada entrada lleva además el intervalo de números (`from`, `to`). El CP exacto de una dirección lo da su portal.
- **Provincias y comunidades**: sin CP.
- **Entidad de tipo CP**: solo `postcode`.

#### 6.5.5 Procedencia

Cada entidad guarda sus fuentes y sus ids de origen (`sources`, `source_ids`), y `conflicts` los atributos en que las fuentes discreparon. Sirve para depurar la fusión, para cumplir las licencias y para devolver `sources` en la API.

#### 6.5.6 Red de seguridad al consultar

Si aun así se colara un duplicado, el motor fusiona en la respuesta los resultados con el mismo `PublicId`, o con el mismo nombre a menos de una distancia configurable (`[serve] same_entity_m`, 100 m), y se queda con el de mayor puntuación. No sustituye a la fusión: cada caso que actúa se cuenta en `/metrics` y señala un fallo de la fusión que hay que corregir.

#### 6.5.7 Implementación

- Todo en Rust (`geko-build/src/fusion`): adaptadores por fuente (`records`), asignación administrativa con R-tree y polígonos preparados (`admin`), calles (`streets`), portales (`addresses`), zonas de CP (`postcodes`), lugares y POIs (`places`), ids e importancia (`ids`), métricas (`metrics`) y escritura del canónico (`output`). Esquema en `schemas/canonical.md`.
- El diccionario de tipos de vía y las reglas de cada país viven en su pack de país (sección 2.4), no en el código común.
- Los umbrales y pesos están en configuración y se ajustan con las métricas de 13.6.
- La importancia usa escalas de referencia fijas (3,5 millones de habitantes, 1.000 portales por calle), no los máximos de los datos: así no depende de qué regiones hay en el ámbito ni difiere entre particiones.

#### 6.5.8 Fusión por particiones

La memoria de la generación no debe crecer con el país. Cada país del ámbito se divide en sus particiones: las regiones de su pack que no contienen ninguna otra (`data/regions.toml`; en España, las provincias). Un país cuyo pack no lista regiones es una sola partición. Staging y fusión trabajan partición a partición. El staging recorta cada provincia por su límite, así que cada calle, portal y POI pertenece a una sola.

1. `fusion::fuse_partition` fusiona una provincia en `canonical/<país>/parts/<provincia>/`. Ningún municipio cruza una provincia, así que la fusión de calles y portales no cambia.
2. `fusion::merge::merge` une las particiones en `canonical/<país>/` leyendo solo las columnas necesarias y escribiendo por lotes: renumera los ids internos de todo el país (por importancia, y luego portales agrupados por calle), deja una sola vez los límites superiores que fusionan varias provincias (comunidad, país), combina los CP que corta una frontera y falla si cualquier otro id público aparece en dos particiones.

El recorte por provincias tiene tres excepciones, descubiertas al generar España completa:

- **Objetos de OSM que cruzan regiones:** cada partición lee el extracto de su comunidad, que solo tiene parte de la geometría del objeto. Su punto puede caer en dos provincias, así que el mismo objeto aparece en ambas. Es una sola entidad, con id de OSM, y la unión la deja una vez, como los límites (`split_osm_objects` en el informe).
- **CartoCiudad se descarga por prefijo de CP, no por límite:** unas 9.000 direcciones de España (el 0,06 %) tienen el CP de una provincia vecina. Por ejemplo, Tresviso (Cantabria) tiene CP 33xxx. Esas direcciones caen en una partición que no contiene su municipio. Antes de la fusión, `fusion::relocate` lee el límite de cada partición de su staging de OSM y envía cada registro de las demás fuentes que cae fuera de la suya a la partición que lo contiene (`relocated/<país>/<partición>/`: las filas que llegan y los números de las que salen). La fusión de cada partición incluye ese reparto en su sello. Un registro que no contiene ninguna partición del ámbito se queda en la partición de su staging.
- **Entidades sin municipio:** llevan la provincia en el id (`es-x-av-…` en vez del código INE), para que dos particiones nunca den el mismo id.

El índice y la inversa leen el canónico del país como antes. El builder del índice procesa los portales fila a fila en columnas compactas, sin crear un objeto por portal, y `ids.fst` solo guarda los ids de calles, lugares y POIs: el de un portal se resuelve por su calle.

Medido con Baleares y Barcelona (1,9 millones de entidades): el pico de memoria del build bajó de 3.170 a 2.423 MB, y ya lo marca la fusión de la provincia más grande (Barcelona), no el país; el índice se genera en 10 s en vez de 14 s. Las respuestas son idénticas byte a byte a las de antes (2.829 búsquedas, 841 inversas y 11.449 consultas por id).

## 7. Descarga automática

### 7.1 Qué se descarga

`geko plan`, `geko fetch` y `geko build` calculan la cadena:

**módulos activos → providers habilitados que esos módulos necesitan y que cubren algún país del ámbito → datos que cada provider pide para el ámbito**

Solo se descarga lo que resulta de esa cadena. Una fuente deshabilitada, que ningún módulo activo necesita o que no cubre ningún país del ámbito, no se descarga.

### 7.2 Mecánica (`geko-fetch`)

- Escritura en `.part` y renombrado al terminar: una descarga a medias nunca parece completa.
- Reanudación con HTTP Range.
- Reintentos con espera creciente y límite de conexiones por servidor (`parallel_downloads` en total).
- Descarga condicional con ETag e `If-Modified-Since`, según `refresh`.
- Verificación de sumas de control cuando la fuente las publica (p. ej. `.md5` de Geofabrik).
- Con `offline = true`, no se hace ninguna petición; si falta algo, error con la lista de lo que falta.

### 7.3 Caché

`raw/<fuente>/`, con un fichero `<fichero>.raw.json` junto a cada descarga: URL, ETag, `Last-Modified`, sha256, tamaño y fecha de descarga. Las descargas en curso usan `<fichero>.part` y `<fichero>.part.meta.json` para poder reanudarse. Las licencias las aporta el provider (`license()`) al manifiesto del bundle.

La caché, la política de refresco y el modo sin red son responsabilidad del `Downloader`: el provider solo dice qué URL necesita y dónde dejarla.

**Cuándo se comprueba** (`[data] max_age_days`, 30 por defecto): todas las fuentes a la vez, con una marca local de la última comprobación (`raw/.checked`). Mientras no vence, el `Downloader` no comprueba nada (`checks_for_updates` es falso): se conserva cada descarga y solo se baja lo que falta. Al vencer, o con `geko build --refresh`, cada fuente pregunta si cambió (ETag, catálogo…) y solo se baja lo que cambió; la marca se renueva si todas responden. `refresh = always` lo baja todo y `never` no comprueba nunca.

**Descargas borradas** (`[data] keep_downloads = false`, por defecto): tras publicar un bundle, se borra lo que cada fuente declara borrable (`SourceProvider::removable`: los extractos de OSM, las copias de Overture, las páginas de CartoCiudad) y se conservan sus fichas (`*.raw.json`, `*.fetched.json`, la marca de descarga completa). Una ficha sin su fichero cuenta como descarga conservada (`Downloader::keeps_removed`). `fetch` informa de la identidad de cada entrada del staging (`FetchReport::identities`: el sha256 de la ficha, la versión y los grupos de filas de Overture, la marca de CartoCiudad), y el staging se identifica por ella (§8.3), así que se reutiliza sin sus ficheros. Si hay que repetirlo, antes se llama a `fetch` de esa fuente, solo con el ámbito de las particiones que se repiten, con un `Downloader` que devuelve lo borrado y no comprueba nada más. Con `offline = true` no se borra nada.

### 7.4 OSM

- Se eligen los **extractos de Geofabrik** que cubren el ámbito: el de cada región, por su código ISO 3166-2 en el índice de Geofabrik o, si no lo tiene, el que el pack del país le asigna (`sources/osm.toml`; una región sin extracto propio usa el de la región que la contiene); y para el país entero, los que llevan su código ISO 3166-1 o los que declara el pack (España: `spain` y `canary-islands`). Previsto: si el conjunto pesa más que el extracto del continente, el continente.
- El recorte por región se hace **en Rust**, dentro del staging de `source-osm`, con el límite de la propia región. No se depende de `osmium`.
- Fase 5: actualización con los **diffs diarios** de Geofabrik (`.osc.gz`) en lugar de descargar de nuevo el extracto completo.

### 7.5 Licencias

Descargar automáticamente implica aceptar las condiciones de cada fuente. Por eso `accept_licenses = true` es obligatorio para descargar, y `geko plan` lista antes todas las licencias. Las licencias pasan de `raw.json` al manifiesto del bundle.

### 7.6 CartoCiudad

- **Centro de Descargas del CNIG: descartado para descargar.** La descarga exige un token de reCAPTCHA (`preAutorizarDescarga`). Es un control de acceso deliberado y no se automatiza. Su listado de ficheros (`archivosSerie`) sí es accesible y puede servir para detectar versiones nuevas.
- **Vía principal: OGC API Features del IDEE**, declarada en `countries/es/sources/cartociudad.toml` para el provider genérico `source-ogc-features` (`https://api-features.idee.es/collections/address/items`). Es oficial y devuelve GeoJSON en páginas de hasta 10.000 elementos. No admite ordenación ni filtros CQL2, y la paginación por `offset` se degrada mucho (11 s en `offset=0`, 76 s en `offset=1.150.000`). Pero el filtro por código postal está indexado (0,13 s si está vacío, menos de 5 s con 10.000 resultados). Como los códigos postales empiezan por el código INE de la provincia, **cada provincia se descarga recorriendo sus 999 códigos postales posibles** y paginando solo dentro de cada uno.
  - Pocas peticiones en paralelo (`parallel_requests`, 4 por defecto), por cortesía con un servicio público.
  - Páginas guardadas comprimidas con zstd en `raw/cartociudad/<INE>.partial/`; una descarga interrumpida se reanuda. Al terminar se cambia de forma atómica a `raw/cartociudad/<INE>/`, con un marcador de descarga completa.
  - La API no da ETag: mientras no toca comprobar las fuentes (`[data] max_age_days`, 30 por defecto, común a todas, §7.3), una descarga completa se reutiliza sin contactar con la API. Cuando toca, antes de volver a descargar (34 min con España), se lee el registro del conjunto en el catálogo del IDEE (`catalog_url`, ISO 19139): si la descarga es posterior a su fecha más reciente (sello de los metadatos, revisión y publicación), sigue siendo la última versión y no se descarga. Pasados `max_unchanged_days` (365) se descarga de todos modos, por si el catálogo no se actualiza con los datos. Si el catálogo no responde, se descarga como antes.
- **WFS INSPIRE de direcciones** (`https://www.cartociudad.es/wfs-inspire/direcciones`): **descartado** tras medirlo el 2026-10-15.
  - **Formato:** deegree, WFS 2.0.0. Solo devuelve GML 3.2, sin GeoJSON ni gzip, y ocupa unos 3,8 KB por dirección (unos 58 GB para España).
  - **Paginación y filtros:** admite `startIndex`/`count` y filtros FES. Filtrar por el prefijo INE de `localId` (`PPK_07*`) cuenta Baleares en 2,4 s y no se degrada con el desplazamiento (unos 8 s cada 1.000 direcciones). Sin filtro, el desplazamiento profundo sí se degrada (16,8 s para 100 elementos en el 15.000.000).
  - **Velocidad:** unas 210 direcciones por segundo y conexión, unas 25 veces menos que la OGC API.
  - **Modelo de datos:** cada dirección solo enlaza (xlink) a la vía, el municipio y el descriptor postal, así que hay que unir `ThoroughfareName` (1,09 millones), `PostalDescriptor` (15.258) y `AdminUnitName` (8.294).
  - **Datos que faltan:** no tiene población (`AddressAreaName` no se puede consultar), proveedor ni sufijo de número (0 sufijos en 5.000 direcciones, frente al 9 % en la OGC API).
  - **Antigüedad:** la edición es más antigua (versiones de 2025-02 y 2025-10, frente a 2026-06-15 en la OGC API) y tiene 412.247 direcciones en Baleares, frente a 418.098.
- **ATOM:** CartoCiudad no publica ningún feed ATOM de direcciones. La única descarga en ficheros es la del CNIG, que exige reCAPTCHA. El registro CSW del conjunto (`spaign_cartociudad_addresses`) declara una actualización trimestral y una fecha, y sirve para detectar versiones nuevas.
- **Manual** (pendiente): leer el GeoPackage descargado a mano del CNIG.

Catastro publica feeds ATOM por municipio, y el INE y el IGN tienen URLs directas.

### 7.7 Teselas

El jar de Planetiler (versión fija) y sus datos auxiliares (Natural Earth, polígonos de agua) se descargan con el mismo mecanismo, solo si el módulo `tiles` está activo. Planetiler recibe el OSM de la caché común, no lo descarga por su cuenta.

## 8. Generación

### 8.1 Staging de OSM

Tres pasadas en paralelo sobre el `.pbf`:

1. Relaciones: límites administrativos y las vías y nodos que referencian.
2. Vías: viales, direcciones, POIs y vías de los límites; ids de los nodos que usan.
3. Nodos: coordenadas solo de los nodos necesarios (en Cataluña, 4,7 millones de unos 35) y entidades que son nodos.

(Las relaciones van al final del fichero, así que hace falta una pasada por tipo.)

Así no se guardan en memoria las coordenadas de todos los nodos.

Cada pasada decodifica los bloques del `.pbf` en paralelo, recorre solo el grupo que le interesa (relaciones, vías o nodos) y acumula resultados por bloque, no por elemento. La descompresión usa `zlib-rs` y el binario usa `mimalloc`: con el motor zlib y el asignador por defecto, el staging de Cataluña tardaba 72 s; con ellos, unos 13 s.

### 8.2 Índices

`GeocodeIndexBuilder` genera **un segmento por país** a partir de `canonical/<país>/`. Los segmentos de países distintos se generan en paralelo.

### 8.3 Incrementalidad

Cada etapa (staging por partición, fusión por país, segmento por país, teselas) se identifica por el hash de sus entradas y de la versión de su código. Si el hash no cambia, la etapa no se repite y su salida se reutiliza.

- **Etapas:** staging por fuente y partición, fusión por partición, unión por país, índice de texto y de inversa por país, y publicación del bundle.
- **Sello:** cada etapa escribe un sello junto a su directorio de salida (`<dir>.stamp.json`). Tiene dos hashes:
  - `key`: código y entradas de la etapa.
  - `output`: contenido de la salida, sin los informes con tiempos ni los subdirectorios de otras etapas.

  El sello se borra antes de empezar la etapa y se escribe solo si termina bien. Así, una etapa interrumpida o fallida nunca se toma por actual.
- **Entradas:**
  - Las descargas se identifican por la identidad que da su fuente al descargarlas (`FetchReport::identities`, §7.3), que no cambia aunque se borren sus ficheros; las que no tienen, por ruta, tamaño y fecha de modificación. Cada provider declara qué ficheros de `raw/` lee para una partición (`SourceProvider::stage_inputs`). Por ejemplo, OSM solo lee los extractos de la partición, no el índice de Geofabrik, que cambia a diario.
  - La salida de una etapa anterior se identifica por su `output`. Como la generación es determinista, una etapa que se repite y escribe los mismos bytes no obliga a repetir las siguientes.
- **Versión de código:** la calcula `build.rs` de `geko-app` como el hash de las fuentes del crate de la etapa, las de los crates del workspace de los que depende y las entradas de `Cargo.lock` de las dependencias externas que usan (normales y de compilación, con las suyas), no el fichero entero: una dependencia añadida a otro crate no rehace la etapa. Ningún número de versión se sube a mano. Cada etapa usa:
  - Staging: el crate del provider.
  - Staging de una fuente declarada por un pack: el provider genérico y el hash de su declaración.
  - Fusión y unión: `geko-build` y el normalizer del país (el código de `text-rules` y el hash de los ficheros de texto y de regiones de su pack).
  - Índices: su provider; el de texto, también el normalizer.
- **Publicación:** no se publica una versión nueva si la actual se publicó con los mismos segmentos, módulos y licencias.
- **Forzar:** `geko build --force` repite todas las etapas.

## 9. Motor de búsqueda (`geocode-fst`)

### 9.1 Segmentos por país

- Cada país es un **segmento autocontenido**: índices de texto, listas, datos de lugares, inversa, tabla de ids y el pack del país con que se generó.
- Con filtro de país, o con una pista de país clara (9.3), se consulta solo ese segmento o se empieza por él.
- Sin pista, se consultan los segmentos en paralelo y se mezclan sus k mejores. Cada segmento recibe en `min_score` el umbral ya alcanzado, para parar antes.
- `[serve] countries` elige qué segmentos carga cada nodo.

### 9.2 Análisis de la consulta

Cada `AddressParser` de país devuelve interpretaciones puntuadas, y el total se limita (por defecto, 8). Tipos:

| Tipo | Ejemplos (España) |
|---|---|
| Dirección | "calle mayor 23 alcala", "c/ mayor, 23, 28801" |
| Lugar | "tomelloso", "parque del retiro", "el palmar murcia" |
| Código postal | "28801" |
| Coordenadas | "40.4168,-3.7038", "40°25'N 3°42'W", "30T 440000 4474000", Plus Code |
| Referencia catastral | "9872023VH5797S0001WX" |
| Polígono y parcela | "poligono 5 parcela 23 tomelloso" |
| Punto kilométrico | "A-3 km 45", "N-340 pk 1012" |

Las coordenadas y las referencias catastrales con formato válido se resuelven sin pasar por el índice de texto.

**Tipos de vía en otras lenguas.** Un visitante escribe la calle en su lengua: «12 Femenies Street», «Street Femenies» para «Carrer de Femenies». `languages/<lengua>.toml` (hoy, `en`) da las palabras de cada tipo de vía en esa lengua, si van detrás del nombre, y el mismo tipo en otras lenguas (`same_as`). Al leer un pack, cada tipo de la lengua es el primero del pack que tiene una de esas palabras, así que vale para cualquier país sin escribir nada en su pack, y las palabras que el pack ya tiene son suyas. Solo se leen en las consultas, nunca en los nombres de las fuentes: delante del nombre o, si la lengua lo escribe detrás, al final. Medido con `english.jsonl` (2026-10-21), top-1 en Baleares y Luxemburgo: de 29,7 a 94,5 % y de 38,9 a 95,4 %, sin cambiar ninguna otra consulta del golden.

### 9.3 Pista de país

Antes de ejecutar los parsers se calcula una pista barata con:

- el filtro de país, el foco y el idioma de la petición;
- un vocabulario corto por país ("calle", "carrer", "rúa", "rue", "rua", "straße"…) y formatos de CP.

El parser del país más probable va primero. Los demás solo se ejecutan si hace falta (9.5).

### 9.4 Estructura del índice de un segmento

- **Índice de palabras** (FST): cada palabra normalizada apunta a la lista de entidades que la contienen, con el campo y la posición. Encuentra nombres incompletos y desordenados.
- **Índice de nombres completos** (FST): solo para premiar la coincidencia exacta del nombre entero.
- **Índice fonético** (FST, previsto): claves fonéticas del idioma para el último recurso de la cascada.
- **Un juego de índices por tipo de entidad**: administrativos y topónimos, viales, POIs y códigos postales.
- **Los portales no se indexan como documentos de texto.** Cada vial guarda sus números en un bloque compacto (sección 9.7). El índice de texto queda en pocos millones de entradas por país.
- **Puntos kilométricos**: array ordenado por carretera y km; búsqueda binaria.
- **Parcelas** (capa opcional): array ordenado por referencia catastral, y un índice municipio + polígono + parcela.
- **Listas por municipio**: un `roaring` bitmap de entidades por municipio, para filtrar.

Resolución de "Calle Mayor 23, Alcalá": municipios y localidades candidatos para "Alcalá", luego viales con "mayor" filtrados por esos municipios, luego búsqueda binaria del 23 en el bloque de cada vial. Si el 23 no existe, se devuelve el número más cercano del mismo lado (par o impar) o se interpola, con `match_type` aproximado o interpolado.

### 9.5 Búsqueda en cascada y parada temprana

Cascada dentro de cada interpretación:

1. Coincidencia exacta de palabras.
2. Prefijo en la última palabra (y en todas en autocompletado).
3. Tolerancia a erratas con autómatas de Levenshtein, solo si faltan resultados y solo en palabras de 4 o más letras (distancia 1 hasta 7 letras, 2 desde 8).
4. Traducción palabra a palabra a las otras lenguas del país (equivalencias, abajo).
5. Claves fonéticas, solo si siguen faltando resultados (previsto).

**Equivalencias entre lenguas.** Un nombre puede estar escrito en una sola lengua del país y buscarse en otra («plaza mayor, palma» para «Plaça Major»). Al generar el segmento, `geocode-fst` aprende qué palabras son la misma de los nombres de OSM en varias lenguas del país (`name:<lengua>`, lenguas de `pack.toml`):

- Los nombres de una entidad en dos lenguas se alinean palabra a palabra si tienen el mismo tipo de vía y tantas palabras significativas.
- Un par de palabras distintas se queda si aparece en al menos 3 nombres distintos, es la lectura habitual de una de sus palabras (30 % de sus alineaciones) y no una rara de la otra (5 %). Así caen las alineaciones casuales entre lenguas que ordenan las palabras de otro modo («Mikel Deuna» y «San Miguel»).
- El pack añade los pares aprendidos con todo el país, para que una generación regional también los tenga, y descarta los que se le indiquen (`data/equivalences.txt`). El segmento guarda la lista final en `cold/equivalences.txt`.

El índice sigue siendo literal: indexar también las equivalencias alargaba las listas de las palabras comunes («sant» con todas las «san») y la calle buena quedaba fuera de los candidatos. La traducción se usa al consultar, solo en dos casos: cuando una localidad completa nombra un municipio sin ninguna calle o lugar con ese nombre exacto, y como último recurso, cuando ninguna lectura de la consulta encontró nada. No se traduce mientras se teclea ni cuando una palabra solo se encontró con errata.

Parada temprana a tres niveles:

- **Dentro de una lista**: como los `LocalId` están en orden de importancia, recorrer por id es recorrer de más a menos importante. Con un montículo de tamaño k, se para cuando ningún candidato restante puede superar al k-ésimo.
- **Entre interpretaciones**: se evalúan en orden de probabilidad, y se para cuando el k-ésimo resultado supera lo máximo que podría dar la siguiente.
- **Entre segmentos**: mediante `min_score` (9.1).

La puntuación combina coincidencia de texto, importancia, distancia al foco y afinidad de tipo, con los pesos de `QueryContext`. El filtro por municipio interseca con su bitmap; el filtro por zona usa la celda H3 de cada entidad. Con `debug=true`, cada resultado devuelve el desglose de su puntuación.

### 9.6 Parte caliente y parte fría

- **Caliente**, fijada en RAM con `preload = true` (`MAP_POPULATE` o `madvise(WILLNEED)` y `mlock`): FST, listas de entidades, bitmaps por municipio, listas precalculadas de prefijos, coordenadas e importancia.
- **Fría**, mapeada sin fijar: datos de cada lugar, JSON preserializado y capas opcionales. El sistema operativo mantiene en caché lo consultado a menudo.
- Los portales (el 86 % de las entidades) no guardan su JSON preserializado: repiten casi todo de su calle. Guardan índices a tablas pequeñas (cola del id, etiqueta, municipio y fuentes) y su fragmento se genera al responder, idéntico byte a byte al preserializado. En Baleares el segmento bajó de 252 a 50 MB (parte fría de 236 a 34 MB) sin cambiar el caudal; en España serían unos 1,3 GB en vez de 6,5 GB.
Cada parte va en ficheros separados del segmento, para poder tratarlas distinto. El objetivo de RAM del §1.1 se aplica a la parte caliente.

### 9.7 Codificación compacta

- Coordenadas en punto fijo `i32` (sección 5.1).
- Importancia en `u16`.
- Portales de cada vial codificados por diferencias y empaquetados en bits.
- Listas de entidades en `roaring` o empaquetadas en bits, con intersecciones SIMD.
- Datos en columnas (*struct of arrays*), para aprovechar la caché de la CPU.
- Textos codificados por diccionario.

### 9.8 Autocompletado

- Para las primeras letras se **precalculan en la generación las entidades más importantes que empiezan así** (`prefixes.fst`). Un prefijo corto tecleado solo sale de esa lista, filtrada por el prefijo entero; el foco sesga el orden. Se descartó precalcularlo por celda H3: un prefijo de 2–3 letras ya tarda 43–108 µs con España.
- Un prefijo cuya expansión pasa del 0,5 % del índice se comprueba en los nombres en vez de intersecarse; los demás siguen la cascada y la parada temprana de 9.5.

### 9.9 Inversa

- Con la capa `street` sola, la calle del portal más cercano va primera si el punto está más cerca de ese portal que de cualquier calle: un portal suele estar a decenas de metros del eje de su calle, y otra puede quedar más cerca. Entre portales empatados (una esquina), gana la calle con más de ellos. La calle de cada portal está en `address_streets.bin`.
- `ReverseIndex.nearest`: R-tree estático sobre portales, viales (un punto cada 20 m), POIs y topónimos del segmento, empaquetado por curva de Hilbert al generar (nodos de 16) y leído en el sitio desde ficheros mapeados (`coords.bin`, `ids.bin`, `kinds.bin`, `tree.bin`). Abrirlo no construye nada, sus páginas las comparte la caché del sistema y no ocupa memoria anónima: en Baleares, 1,06 millones de puntos, la memoria anónima del servidor bajó de 138 a 35 MB y la carga del bundle de 242 a 22 ms, con el mismo caudal (unas 51.000 inversas por segundo con concurrencia 16).
- `ReverseIndex.admin_at`: cadena administrativa de **cualquier punto**, con un R-tree de las cajas de los límites y polígonos preparados: 0,2–0,3 ms de mediana con España. Se descartó la tabla por celdas H3 (`admin_cells`), que ahorraría menos de 0,1 ms.
- El segmento se elige por la celda del punto. Si no hay portal cerca, se devuelve el vial más cercano, y si tampoco, el topónimo o el nivel administrativo.

## 10. API

La referencia de las rutas ya implementadas (parámetros, respuestas y errores) está en dos ficheros: las del host en [openapi.yaml](../openapi.yaml), en la raíz del repositorio, servida en `/openapi.yaml`, y las del geocoder en [modules/geko-geocoder/openapi.yaml](../modules/geko-geocoder/openapi.yaml), servida en `/geocoder/openapi.yaml`. `/docs` (Swagger UI) las muestra todas.

Cada módulo sirve sus rutas bajo su nombre (`/geocoder/…`, `/tiles/…`; docs/MODULES_PLAN.md): dos módulos no pueden pisarse. `/` lleva a la página del primer módulo que tenga una.

### 10.1 Rutas

```
GET  /                              redirige a /geocoder/
GET  /v1/attribution
GET  /openapi.yaml, /docs
GET  /health
GET  /metrics

GET  /geocoder/                     página de demostración: buscador, autocompletado e inversa sobre un mapa
GET  /geocoder/openapi.yaml
GET  /geocoder/v1/attribution/method
GET  /geocoder/v1/search?text=&focus.lat=&focus.lon=&boundary.*=&boundary.country=&size=&lang=&debug=
GET  /geocoder/v1/search/structured?address=&housenumber=&street=&postalcode=&locality=&country=&size=&lang=
GET  /geocoder/v1/autocomplete?text=&focus.lat=&focus.lon=&boundary.country=&size=&lang=
GET  /geocoder/v1/reverse?lat=&lon=&size=&layers=
GET  /geocoder/v1/place/{id}
POST /geocoder/v1/batch

GET  /tiles/{z}/{x}/{y}.mvt
GET  /tiles/tilejson.json
GET  /tiles/style.json
GET  /tiles/glyphs/{fontstack}/{range}.pbf
GET  /tiles/sprites/{name}.{json|png}
```

`text` admite como máximo 256 caracteres (400 si es más largo) y el motor solo busca sus 12 primeras palabras: un texto más largo solo cuesta tiempo.

`lang` (código de lengua, como `ca`, `fr` o `en`) da el nombre, la calle y el municipio de cada resultado, también en su etiqueta, en esa lengua cuando los datos lo tienen (`name:<lengua>` de OSM). Si no lo tienen, sale el nombre principal. Sin `lang`, el nombre principal, que es el de la fuente de mayor prioridad. Las coordenadas también se aceptan con los nombres de Pelias (`point.lat`/`point.lon` en la inversa, `focus.point.lat`/`focus.point.lon` en el foco), para que un cliente escrito para Pelias funcione sin cambios.

`admin=true` añade a cada resultado las unidades administrativas que lo contienen (`admin`, como en la inversa).

- Las rutas de `/geocoder/*` solo existen con el módulo `geocoder` activo, y las de `/tiles/*` solo con el módulo `tiles`. Si un módulo está desactivado, sus rutas devuelven 404. `/health` informa de los módulos, países y capas activos.
- `/geocoder/v1/batch` (POST, `{"requests": [{"route", "params"}]}`) tiene un tamaño máximo configurable (`[serve] batch_max` de `geko-geocoder.toml`, 100) y se responde en orden, consulta tras consulta, en una sola plaza del pool de búsqueda: un lote no quita todos los núcleos a las peticiones de los demás clientes.

### 10.2 Respuesta

GeoJSON `FeatureCollection`. Cada resultado incluye:

- `gid`: el `PublicId`;
- `confidence`: entre 0 y 1. En búsqueda y autocompletado es la probabilidad de que el resultado sea el pedido: un modelo logístico con la puntuación, la ventaja sobre el siguiente, la distancia al primero, la posición y el tipo de coincidencia, ajustado con las consultas de referencia y las pulsaciones de España (`tools/confidence/`). Un resultado con 0,9 acierta unas nueve de cada diez veces. En inversa y en la consulta por id, es la puntuación relativa a la del mejor;
- `match_type`: `exact`, `interpolated`, `approximate`, `street_only`, `locality_only`, `coordinates`, `cadastral`, `kilometric`;
- `sources`: fuentes que aportaron el resultado;
- procedencia de los atributos que pueden inferirse, p. ej. `postcode_source`: `cartociudad`, `osm` o `inferred` (sección 6.5.4);
- con `debug=true`, el desglose de puntuación y los tiempos por fase.

## 11. Rendimiento en el servicio

- **Lectura sin copias**: columnas mapeadas, sin deserializar al cargar (`rkyv`, previsto si hiciera falta).
- **JSON preserializado**: el fragmento GeoJSON de cada lugar se guarda ya serializado (una variante por idioma si hace falta). La respuesta concatena fragmentos y solo añade los campos dinámicos.
- **Caché de respuestas** (`[serve] cache_entries`; 0 la apaga, que es el valor por defecto en el código para que las comprobaciones midan el motor): búsqueda y autocompletado. Clave: ruta, texto sin mayúsculas ni espacios repetidos, foco redondeado a 0,01° (en torno a 1 km) y el resto de parámetros. Valor: el texto de la respuesta. 16 particiones con dos generaciones cada una: la llena pasa a ser la anterior, y lo que se lee de la anterior vuelve a la actual. Nunca guarda una respuesta recortada por el presupuesto de tiempo, y se vacía al cambiar de bundle. Aciertos y fallos en `geko_cache_lookups_total`. Medida con España y las pulsaciones reales (2026-10-18, 100.000 entradas, 74 % de aciertos): con 16 a la vez, de 3.928–4.138 a 9.844–10.542 QPS y p99 de 15–16 a 11 ms.
- **Normalización sin asignar memoria**: camino rápido para ASCII, tablas de plegado precalculadas, buffers reutilizados por hilo.
- **Hilos**: búsqueda y autocompletado corren en el pool de bloqueo con como mucho una por núcleo (un semáforo), y los hilos de tokio quedan para la red y las rutas baratas: con 48 búsquedas a la vez, la inversa pasó de p99 21 a 6,7 ms. `/v1/batch` ocupa una sola plaza.
- **Presupuesto de tiempo por consulta** (`search_budget_ms`, `autocomplete_budget_ms`): pasado el plazo no se ejecutan más lecturas secundarias ni búsquedas de respaldo; la lectura más probable siempre se completa. Es la decisión de producto que fija el p99 (§13.4 bis).
- **Asignador**: `mimalloc`.
- **Compilación**: `lto = "fat"`, `codegen-units = 1`, `panic = "unwind"` (un fallo en una petición no tumba el servidor). Previstos: binarios para `x86-64-v3` y `x86-64-v2` o `multiversion` en las funciones SIMD, y PGO entrenado con las consultas de referencia.
- **HTTP**: keep-alive y HTTP/2. No se comprimen respuestas de menos de ~1 KB.
- **Teselas**: por rangos sobre el `.pmtiles` con mmap, ya comprimidas (sin recomprimir), directorios de PMTiles en memoria, ETag, cabeceras de caché largas y CDN delante cuando la haya.
- **Observabilidad**: tiempos por fase (análisis, recuperación, ranking, serialización) en `/metrics`, tasa de acierto de la caché y registro de consultas lentas.

## 12. Bundles y despliegue

```
bundles/
  v2026-10-15T030000/
    manifest.json             # versión, módulos, países; providers y formatos; licencias y
                              # atribuciones; hash de cada artefacto
    segments/
      es/
        geocode/hot/          # FST de palabras, nombres y prefijos, listas, coordenadas, portales
        geocode/cold/         # datos de lugares, JSON preserializado, ids, equivalencias
        reverse/              # R-tree de puntos, límites, calle de cada portal
        pack/                 # el pack del país con que se generó (sección 2.4)
      lu/ ...
    tiles/                    # previsto: tiles.pmtiles, style.json, glyphs, sprites
  current -> v2026-10-15T030000
  current.version             # la versión de current, para quien la lee por HTTP
```

- **Generación**: `geko build` genera el bundle y lo publica. Nunca modifica uno existente. Los segmentos sin cambios (mismo hash) se reutilizan de la versión anterior.
- **Servicio**: con `[serve] bundles_url` (la dirección HTTP de una carpeta de bundles publicada, servida por cualquier servidor web o almacenamiento de objetos), `geko serve` lee en cada recarga su `current.version` y descarga **solo los artefactos cuyo hash cambió**; los demás los enlaza (enlaces duros) desde la versión anterior. La publicación escribe `current.version` junto al enlace `current`, que no se puede leer por HTTP. Las descargas pasan por `.downloads/` y entran en la versión ya comprobadas; la versión nueva solo pasa a `current` completa. Comprueba sumas de control y formatos, carga y hace el cambio atómico con `ArcSwap`. Las consultas en curso terminan sobre la versión anterior. Se conservan las últimas `[build] keep` versiones (1 por defecto; 2 para poder volver atrás). Borrar la anterior con el servicio cargado es seguro: todos los ficheros del bundle se abren al cargarlo, y en Linux un fichero abierto o mapeado sigue valiendo aunque se borre.
- **Escala**: primero **réplicas** (cada nodo con todos los países, detrás de un balanceador). Solo cuando no quepan en RAM, **reparto de países** entre nodos con `[serve] countries` y un enrutador delante.
- **Regeneración programada** (prevista, fase 5): OSM a diario; las fuentes oficiales cuando publican una versión nueva (comprobado con peticiones condicionales). Hoy se programa fuera de geko: un `geko build` periódico, y `serve` carga el bundle nuevo sin cortes.

## 13. Calidad y medición

### 13.1 Consultas de referencia

`countries/<país>/golden/*.jsonl`: consulta → resultado esperado (PublicId o coordenadas con tolerancia), con una categoría por consulta.

- **Generadas** con `geko-eval` a partir del canónico, deformando entidades reales: erratas, abreviaturas, orden alterado, municipio ausente, CP en lugar de municipio, nombres en lengua cooficial, nombres antiguos y prefijos de autocompletado.
- **En inglés** (`english.jsonl`, `geko-eval english`), como las escribe un visitante: el tipo de vía en inglés detrás o delante del nombre («12 Femenies Street», «Street Femenies 12»), con y sin errata, y los nombres en inglés de lugares, POIs y calles (`name:en`, si no coincide con otro de sus nombres). Las palabras inglesas de cada tipo de vía están en `golden/english.toml` del pack. Incluye también las entidades cuyo propio nombre lleva una de esas palabras («Road House»), que leerlas como tipo de vía no debe perder.
- **Escritas a mano**: casos reales y difíciles (homónimos, pedanías, direcciones rurales, s/n, km).
- Categorías: dirección completa, dirección sin municipio, vial, lugar, topónimo rural, CP, POI, coordenadas, catastral, punto kilométrico, autocompletado, inversa.

Métricas por país y categoría: acierto en top-1 y top-5, MRR y distancia del resultado al esperado.

### 13.2 Protocolo de comparación con Pelias

- Referencia: una instancia de Pelias con OSM de Baleares, cuya dirección da `GEKO_PELIAS_URL` (`/v1/search`, `/v1/autocomplete` y `/v1/reverse`). No se monta en local.
- Esa instancia tiene OSM de Baleares; del resto de España solo tiene la jerarquía administrativa. Por eso la calidad se compara con las consultas de referencia de Baleares (`countries/es/golden/`), no con las `spain-*.jsonl`.
- Mismas consultas para los dos, con `geko-eval run --url "$GEKO_PELIAS_URL"`. Es un servicio compartido: siempre con pausa entre peticiones (`--delay-ms`) y nunca con pruebas de carga.
- geko se mide dos veces: solo con OSM (comparación justa del motor, `*-osm.jsonl`) y con todas sus fuentes (comparación del producto).
- La latencia de Pelias incluye la red y no es comparable. La latencia y el caudal de geko se miden contra los objetivos de §1.1, en el hardware de referencia, desde otra máquina de la misma red y con calentamiento previo: p50, p95 y p99 a carga creciente hasta encontrar el caudal máximo que mantiene el p99 objetivo. Con y sin caché de respuestas.
- RAM residente (caliente y total) y tiempo de arranque.
- Resultados guardados con la versión del bundle, para ver la evolución.

### 13.3 Benchmarks internos

Instrucciones, no tiempo, de las piezas calientes (análisis, búsqueda y autocompletado en el índice, inversa) sobre las consultas de referencia del bundle del golden: `geko-bench` las recorre en funciones `bench_*` y callgrind cuenta solo las suyas. Las instrucciones apenas varían entre ejecuciones y máquinas, así que se comparan en la CI (tolerancia del 3 %), donde el reloj no sirve. `criterion` se descartó por eso.

### 13.4 Validación del canónico

`geko-eval data` sobre `canonical/<país>/*.parquet`: ids públicos repetidos, portales sin vial, sin municipio o con un municipio o un CP que no existen, y lo mismo para calles y POIs; `geko-eval check-data` falla si empeoran respecto a la línea base. Las coordenadas fuera de su municipio no pueden darse: el municipio sale de ellas.

### 13.4 bis Resultados de la fase 1 (Islas Baleares, octubre de 2026)

Consultas de referencia generadas con `geko-eval` (semilla 42, 100 por categoría) más 30 escritas a mano (`countries/es/golden/`). Un resultado es correcto si cae a menos de la tolerancia del caso (50 m un portal, 150 m un POI, 500 m una calle, 1,5–4 km un CP, un municipio o un lugar). geko con OSM y CartoCiudad; Pelias de referencia: una instancia con OSM de Baleares (`GEKO_PELIAS_URL`).

**Comparación justa de motor** (`generated-osm.jsonl`: solo entidades que también están en OSM), top-1 / top-5:

| Categoría | geko | Pelias |
|---|---|---|
| Dirección completa | 98,0 % / 100 % | 92,0 % / 92,0 % |
| Dirección sin municipio | 82,0 % / 96,0 % | 91,0 % / 95,0 % |
| Dirección con CP | 99,0 % / 99,0 % | 97,0 % / 99,0 % |
| Dirección con el tipo en castellano | 96,0 % / 97,0 % | 2,0 % / 2,0 % |
| Dirección con errata | 94,4 % / 96,7 % | 13,3 % / 14,4 % |
| Autocompletado de municipio | 65,4 % / 80,8 % | 46,2 % / 80,8 % |
| Calle | 94,0 % / 99,0 % | 67,0 % / 68,0 % |
| POI | 92,0 % / 100 % | 75,0 % / 84,0 % |
| Municipio | 100 % / 100 % | 83,3 % / 97,2 % |
| Escritas a mano: lugares | 95,0 % / 100 % | 85,0 % / 95,0 % |
| Escritas a mano: monumentos | 70,0 % / 80,0 % | 40,0 % / 40,0 % |
| **Total** | **92,7 % / 97,6 %** | **63,8 % / 68,0 %** |

**Comparación de producto** (`generated.jsonl`: todas las entidades, incluidas las que solo tiene CartoCiudad): geko **90,2 % / 95,3 %**, Pelias **40,2 % / 44,1 %**. Pelias no conoce los CP (0 %) ni la mayoría de portales que solo están en CartoCiudad.

Lecturas:

- Objetivo de §1.1 (≥ Pelias + 10 puntos): **cumplido**, +28,9 puntos en la comparación justa.
- "Nunca peor en ninguna categoría": **no se cumple en top-1 para "dirección sin municipio"**. Esas consultas son ambiguas (p. ej. "Carrer de Manacor" existe en 12 municipios del canónico): geko devuelve un homónimo real que no es el elegido al generar el caso, y Pelias, con menos datos, suele tener un solo candidato. En top-5, la métrica adecuada para consultas ambiguas, geko gana (96 % frente a 95 %). Queda como trabajo: usar el foco y la importancia del municipio para desempatar homónimos.
- Latencia por petición HTTP (mediana, cliente en la misma máquina): geko 0,1–3 ms. La de Pelias (60–90 ms) incluye la red hasta la nube y no es comparable.
- Pendiente: nombres traducidos entre lenguas (major/mayor, Oms/Olmos, Sant Antoni/San Antonio) y aeropuertos como lugar destacado.

**Validación con un conjunto nuevo** (semilla 7, `holdout.jsonl` y `holdout-osm.jsonl`; solo 97 de 835 consultas coinciden con las de la semilla 42, casi todas de municipios y CP). El ranking se afinó con la semilla 42, así que este conjunto mide si generaliza. Tras corregir la fusión de calles sin geometría (abajo) y regenerar ambos conjuntos, top-1 / top-5:

| Conjunto | geko | Pelias |
|---|---|---|
| Semilla 42, comparación justa | **95,0 % / 98,3 %** | 61,0 % / 65,5 % |
| Semilla 7, comparación justa | **95,9 % / 99,3 %** | 63,7 % / 67,4 % |
| Semilla 42, producto completo | **93,2 % / 97,0 %** | 37,8 % / 42,3 % |
| Semilla 7, producto completo | **93,5 % / 97,8 %** | 38,2 % / 41,4 % |

No hay sobreajuste: el conjunto nuevo da cifras iguales o mejores. "Dirección sin municipio", la única categoría en la que Pelias ganaba en top-1, la gana ahora geko por poco (90 % frente a 88 % con la semilla 42; 85 % frente a 84 % con la 7): geko ya no es peor en ninguna categoría.

El conjunto nuevo destapó un fallo de fusión: 11.428 calles (30 %) compartían nombre con otra de su municipio. Eran carreteras y diseminados de CartoCiudad sin geometría en OSM, cuyos portales distan más de los 400 m de agrupación y se partían en decenas de calles. Ahora, dentro de un municipio, el mismo nombre en la misma entidad de población (o sin ella) es una sola calle, y las calles llevan su entidad de población (`locality`) en la etiqueta para distinguir homónimos reales ("Carrer del Sol, Santa Ponça, Calvià"). Calles: 38.717 → 31.642; homónimas indistinguibles (`homonymous_streets` del informe de fusión): 3.094 → 293.

**Pruebas con entradas aleatorias** (`geko-eval fuzz`): 30.000 peticiones de búsqueda, autocompletado e inversa con texto aleatorio o mutado (Unicode, combinantes, nulos, textos de 400 fragmentos, coordenadas fuera de rango, `size` inválido): ninguna respuesta 5xx ni conexión cortada. Los textos muy largos tardaban hasta 1,8 s; con el límite de 256 caracteres en la API y de 12 palabras en el motor, el peor caso baja a 193 ms y el p99 de 53 a 31 ms.

**Muestreo manual de fusiones** (§13.6): 40 calles con varias fuentes y 40 alias cuyo nombre difiere del de la calle (de las uniones aproximadas): ninguna fusión errónea (error < 7 % con un 95 % de confianza; con más muestra, la cota bajará).

**Prueba de carga** (`oha`, 10 s por prueba, URLs de las consultas de referencia; cliente y servidor en la misma máquina de desarrollo, WSL con 12 núcleos, no en el hardware de referencia):

| Ruta | Concurrencia | Caudal | p50 | p99 |
|---|---|---|---|---|
| `/v1/search` | 4 | 3.018 QPS | 0,27 ms | 13,5 ms |
| `/v1/search` | 16 | 3.372 QPS | 1,1 ms | 50 ms |
| `/v1/autocomplete` | 4 | 3.956 QPS | 0,8 ms | 3,4 ms |
| `/v1/autocomplete` | 16 | 7.409 QPS | 1,6 ms | 8,9 ms |
| `/v1/reverse` | 4 | 20.319 QPS | 0,18 ms | 0,5 ms |
| `/v1/reverse` | 16 | 67.718 QPS | 0,18 ms | 0,8 ms |

Autocompletado e inversa cumplen los objetivos de §1.1. La búsqueda los cumplía con concurrencia 4 pero no con 16 (p99 de 50 ms): la cola la formaban las consultas caras (erratas con distancia 2, relajación, expansiones de prefijo largas), que además ocupaban los hilos de tokio. Se resolvió con el presupuesto de tiempo por consulta y el pool acotado (§11). Con España (2026-10-16, en el servidor): autocompletado p99 5,6 ms y búsqueda 4,0 ms; con 16 a la vez, 16,5 y 13,9 ms. Medido en la máquina de desarrollo, no en el hardware de referencia.

### 13.4 ter Primera medida en España completa (octubre de 2026)

Consultas de referencia generadas con `geko-eval generate` a partir del canónico de España: `countries/es/golden/spain-generated.jsonl` (semilla 42) y `spain-holdout.jsonl` (semilla 7), unas 900 cada una. Además, `spain-focus.jsonl`: las direcciones sin municipio, con el foco que mandaría una app.

**Servicio:** el bundle carga en 0,97 s y ocupa 932 MB residentes (371 MB anónimos y 560 MB mapeados); tras 1.800 consultas, 1,18 GB.

**Acierto en top-1:**

| Categoría | Baleares | España, primera medida | España, tras corregir el motor |
|---|---|---|---|
| Dirección completa | 98 % | 86–87 % | **92–96 %** |
| Dirección con errata | 95–98 % | 84–85 % | **90–95 %** |
| Dirección sin municipio, con foco | 98 % | 89 % | **94 %** |
| Calle | 88–95 % | 84–88 % | 87–90 % |
| Municipio, CP | 100 % | 100 % | 100 % |
| POI | 96–97 % | 91–96 % | 91–96 % |
| Dirección sin municipio ni foco | 77–78 % | 45–50 % | 45–50 % |
| Autocompletado de municipio | 84–89 % | 40–44 % | 40–44 % |

Los dos fallos del motor solo aparecen a escala de país:

- **Calles recorridas antes del filtro de municipio:** el motor recorría como mucho 400 calles con el nombre buscado, por orden de importancia, y contaba también las que el filtro descartaba. Las 400 primeras "Calle Real" de España nunca llegaban a la de Calzada del Coto, y respondía con el municipio. Ahora solo cuentan las que pasan el filtro, con una cota de 100.000 visitadas.
- **El foco no elegía candidatas:** sin municipio pero con foco, las calles cercanas al usuario no estaban entre las 400 más importantes y el foco no llegaba a actuar. Ahora, pasadas esas 400, también entran las que están a menos de 50 km del foco.

Las categorías sin municipio ni foco miden sobre todo la ambigüedad de los homónimos de todo el país, que ningún motor puede resolver sin contexto.

### 13.5 Suites de conformidad

Una por puerto (sección 2.1, regla 5), en `crates/geko-conformance`. Cada provider la ejecuta desde su propio test de integración, `tests/conformance.rs`.

- **Fixture canónico:** se genera con la fusión real (`fusion::fuse_staged` y `merge::merge`) a partir de registros sintéticos de Algaida: provincia, municipio, dos calles de OSM, portales de CartoCiudad y de OSM, una farmacia y una localidad. Así las suites de los índices prueban siempre el contrato que escribe `geko build`.
- **`GeocodeIndexBuilder` y `GeocodeEngine`:**
  - El manifiesto coincide con el formato del builder.
  - Una dirección, una calle, un municipio, una localidad, un POI y un CP quedan primeros, con su tipo y su posición. La misma dirección escrita como en otra fuente da el mismo portal.
  - El autocompletado encuentra por prefijo.
  - Los resultados respetan `size` y salen ordenados por puntuación.
  - Cada id tiene su lugar y su id público resuelve al mismo id.
  - El buffer se vacía en cada consulta, y `min_score` permite parar antes.
  - Un id desconocido no da ningún lugar.
- **`ReverseIndex`:**
  - El portal bajo el punto sale primero, y los resultados van ordenados por distancia y dentro de `k`.
  - El filtro por tipos se respeta.
  - `admin_at` devuelve la cadena de niveles, de mayor a menor, dentro del municipio, y nada fuera de todo límite.
- **`TextNormalizer`:**
  - `normalize` es idempotente y no distingue mayúsculas.
  - Las variantes de un nombre incluyen su forma normalizada, y ningún token está vacío ni tiene espacios.
  - La compatibilidad de tipos de vía es reflexiva y simétrica.
  - Ninguna entrada hostil provoca un pánico: Unicode, controles, textos muy largos.
- **`AddressParser`:**
  - Como mucho 8 interpretaciones, ordenadas, con probabilidades entre 0 y 1 que suman como mucho 1, y todas del país del parser.
  - Las consultas reales tienen alguna interpretación, y una consulta vacía no tiene ninguna.
- **`SourceProvider`:**
  - Declara licencia, módulos que la usan y cobertura.
  - Sin red, `fetch` falla sin escribir fuera de `raw`.
  - Con el fixture de la fuente, el staging escribe registros y es determinista: dos veces dan los mismos bytes, como exige §8.3.
  - Las entradas que declara existen dentro de `raw`.
  - Sin descargas, el staging falla con un error, no con un pánico.
- **`Downloader`**, contra un servidor HTTP local:
  - Escribe el contenido con su sha256 y su tamaño.
  - Una suma de control errónea o un 404 son un error y dejan intacta la copia anterior.
  - Una descarga fallida no crea nada.

### 13.6 Calidad de la fusión

Por país, en cada build y en CI:

| Métrica | Cómo se mide | Objetivo inicial |
|---|---|---|
| Tasa de duplicados de portales | Pares del canónico con el mismo vial normalizado, número y sufijo, a menos de 50 m | < 0,5 % |
| Tasa de duplicados de viales | Viales del canónico con el mismo nombre y tipo normalizados, en el mismo municipio, cuyas geometrías se tocan o están a menos de 25 m | < 0,5 % |
| Tasa de fusiones erróneas | Revisión manual de una muestra aleatoria de grupos con registros de varias fuentes (100 por tipo y país en cada cambio de reglas) | < 1 % |
| Duplicados en respuestas | Consultas de referencia cuyo top-10 contiene dos resultados que son la misma entidad | 0 |
| Actuaciones de la red de seguridad | `geko_merged_duplicates_total` en `/metrics` (6.5.6) | Tendencia a 0 |
| Conflictos entre fuentes | Atributos con valores distintos según la fuente, por atributo | Se informa; sin umbral |

Si las tasas de duplicados superan su objetivo, el build falla. Los ejemplos de cada métrica se guardan junto al bundle para revisarlos.

## 14. Estructura del repositorio

```
geko/
  Cargo.toml                 # workspace
  geko.toml                  # configuración de ejemplo del host, con todas las opciones
  openapi.yaml               # rutas del host
  Dockerfile, docker/        # imagen ghcr.io/candymaps/geko, sin packs
  ci/                        # test.sh (comprobaciones), golden-snapshot.sh (datos del golden), packs.sh (publicar packs), oras.sh
  schemas/                   # esquemas de staging (staging/*.md) y del canónico (canonical.md), versionados
  crates/
    geko-module-sdk/         # contrato de los módulos (Module), tipos que comparten con el host, bundle, etapas
    geko-core/               # dominio, traits, lógica pura (sin dependencias de tecnología)
    geko-conformance/        # suites de tests genéricas por puerto
    geko-fetch/              # Downloader HTTP, caché raw, cliente de registros OCI
    geko-build/              # generación: staging, fusión (Rust), publicación, incrementalidad, purge
    geko-geom/               # geometría común: anillos, punto en polígono, WKB, distancias
    geko-country-pack/       # lee, verifica y descarga los packs de país; geko-pack-sign
    geko-server/             # servicio común: transporte, montaje de los módulos, bundles, health, metrics
    geko-module-tiles/       # módulo tiles, feature `tiles` (esqueleto)
    geko-eval/               # consultas de referencia, comparación con Pelias, comprobaciones
    geko-bench/              # instrucciones de las piezas calientes (callgrind)
    geko-app/                # el host: binario, CLI, configuración, módulos enlazados, publicación del bundle
  modules/
    geko-geocoder/           # módulo geocoder (crate geko-module-geocoder, feature `geocoder`): fuentes, generación,
                             # servicio, órdenes; README.md, geko-geocoder.toml (su configuración de ejemplo) y openapi.yaml
  providers/                 # genéricos, válidos para cualquier país
    source-osm/  source-overture/  source-ogc-features/
    text-rules/              # normalizador y analizador de reglas, que leen los datos del pack
    geocode-fst/
    reverse-rstar/
  countries/                 # un pack por país (sección 2.4)
    es/  lu/
  tools/confidence/          # ajuste del modelo de `confidence`
```

Previstos: `providers/source-wikidata`, `tiles-planetiler` (proceso Java) y `tiles-pmtiles`, `planetiler/` (profile Shortbread) y `tiles-assets/` (estilo, fuentes y sprites).

## 15. Hoja de ruta

Lo que queda por hacer, con su estado, está en la sección 17. Estado a 2026-10-20: fase 1 terminada; de las demás, lo marcado como hecho, aplazado o descartado.

1. **PoC (Islas Baleares; primero se hizo con la provincia de Barcelona)**:
   1. Esqueleto: `geko-core`, `geko-server`, módulos y `geko-app` con activación de módulos. *(hecho)*
   2. Ajustes de `geko-core` para varios países y rendimiento: `CountryCode`, país en `PublicId`, `Coord` en punto fijo, ids (segmento, `LocalId`), `coverage()`, puerto `Downloader`. *(hecho)*
   3. Configuración (`[[scope]]`, `[data]`, `[sources.*]`), cálculo de la cadena de dependencias y `geko plan`. *(hecho)*
   4. `geko-fetch`, con tests contra un servidor HTTP falso, y comando `geko fetch`. *(hecho)*
   5. `source-osm`: descarga desde Geofabrik con md5, staging en tres pasadas, recorte por región y Parquet directo (`schemas/staging/osm.md`); comando `geko build`. *(hecho)*
   6. CartoCiudad: OGC API Features del IDEE por código postal (`schemas/staging/cartociudad.md`), y las provincias de España. Hoy, una declaración del pack (`countries/es/sources/cartociudad.toml`) para `source-ogc-features`, y `countries/es/data/regions.toml`. *(hecho; el modo manual con el GeoPackage del CNIG queda pendiente)*
   7. Fusión (Rust): asignación administrativa por coordenadas, normalización para comparar y diccionario de tipos de vía (hoy, las reglas genéricas de `text-rules` con los datos de cada pack), resolución de entidades y registro maestro (sección 6.5), atributos inferidos (zonas de CP), ids, importancia, métricas de duplicados (13.6); canónico en `schemas/canonical.md`. *(hecho, incremental por hash: §8.3)*
   8. Analizador de consultas (dirección, lugar con «dentro de», CP y coordenadas decimales, GMS y UTM), y comando de depuración `geko parse`. *(hecho; hoy genérico, `text-rules`, con los datos de cada pack; Plus Codes completos desde 2026-10-17; los cortos, con lugar de referencia, no)*
   9. `geocode-fst` (segmento, parte caliente y fría, cascada, relajación, parada temprana, autocompletado precalculado, red de seguridad) y `reverse-rstar` con `admin_at`; comandos `geko query` y `geko reverse`. *(hecho; el autocompletado precalculado por celda H3 y `admin_cells` se descartaron tras medirlos: un prefijo corto ya tarda menos de 0,11 ms)*
   10. Bundle versionado (publicación con enlaces duros, `current`, sumas de control, poda), carga y recarga en caliente en `geko serve`; rutas `/v1/search`, `/v1/autocomplete`, `/v1/reverse` y `/v1/place`. *(hecho, con generación incremental por hash (§8.3), `geko up` y `/metrics`)*
   11. `geko-eval` (generación de consultas de referencia y evaluación por HTTP) y comparación con Pelias en Baleares (§13.4 bis); afinado del ranking con esas métricas. *(hecho, con prueba de carga; el p99 se resolvió con el presupuesto de tiempo por consulta y el pool acotado: §11)*
2. **España completa y más datos**: España completa *(hecho)*; IGN, INE, NGBE, Catastro y catastros forales, callejeros autonómicos y Wikidata, todos con descarga automática *(Catastro, aplazado; INE e IGN, no compensan por ahora; Nomenclátor y callejeros, sin estudiar: `countries/es/README.md`; la importancia sale de los nombres de OSM sin descargar Wikidata)*; puntos kilométricos *(hecho)*, referencia catastral y capa `parcels` *(aplazadas con el Catastro)*; publicación, descarga solo de lo cambiado y cambio atómico, generación y servicio en máquinas distintas *(hecho: `[serve] bundles_url`)*. Comparación con Pelias *(hecho: `ci/test.sh --scope pelias`)*.
3. **Teselas y API completa**: *(teselas aplazadas por decisión, 2026-10-18: el módulo `tiles` se queda en su esqueleto)* `tiles-planetiler` con descarga automática del jar y de los datos auxiliares, esquema Shortbread y profile propio; `tiles-pmtiles` con estilo, glyphs y sprites; publicación estática como alternativa; `/v1/search/structured`, `/v1/batch`, modos compatibles con Pelias y Photon *(hecho)*; POIs de Overture *(hecho)* y registros oficiales; caché de respuestas *(hecho, apagada por defecto)*.
4. **Optimización guiada por métricas**: JSON preserializado *(hecho)*, `rkyv`, `mimalloc` *(hecho)*, SIMD, PGO, índice fonético, ajustes de ranking e importancia con las consultas de referencia *(en curso: AGENTS.md, «Optimizations»)*.
5. **Operación y más países**: diffs diarios de OSM, métricas por fase *(hecho: `/metrics` y `build_report.json`)*, registro de consultas lentas *(hecho)*, regeneración programada, `BundleStore` con S3, reparto de países entre nodos *(`[serve] countries` hecho; falta el enrutador)*; pack de un segundo país *(hecho: Luxemburgo)*; packs de país como datos firmados, descargados de ghcr.io, y publicación automática de packs e imagen *(hecho, 2026-10-20: §2.4)*.

## 16. Stack

- Rust (edición 2024) para núcleo, descarga, generación y servicio.
- DuckDB (CLI) para inspeccionar y validar los Parquet de staging y canónico.
- Java 21+ solo para Planetiler, invocado como proceso externo en el nodo de generación.
- Crates candidatos (siempre dentro de un provider o de su módulo): `fst`, `roaring`, `rstar`, `h3o`, `rkyv`, `memmap2`, `arrow`/`parquet`, `osmpbf`, `ureq` (rustls, síncrono), `sha2`, `md-5`, `pmtiles`, `axum`, `arc-swap`, `mimalloc`, `multiversion`, `serde`, `tracing`, `ed25519-dalek` (firma de los packs). `criterion` se descartó (§13.3). Herramientas: `oras` para publicar los packs y las instantáneas del golden.

## 17. Pendiente

Lo que queda por hacer del motor genérico. Lo propio de un país está en el README de su pack (`countries/<país>/README.md`, «Pendiente y decisiones»). Al cerrar un punto, bórralo de aquí; si cambia la especificación, actualiza la sección que corresponda de este documento, y si enseña algo (una optimización que funcionó o no), apúntalo en AGENTS.md.

Los puntos cerrados hasta el 2026-10-20, con su historia y sus medidas, están en `git show 8679454:docs/BACKLOG.md` (y `BACKLOG_ES.md`, `BACKLOG_CI.md`). Los «BACKLOG <n>» que citan AGENTS.md, este documento y el código se refieren a esa versión.

Esfuerzo: **S** = horas, **M** = una o dos jornadas, **L** = varias jornadas. Estado: ⬜ pendiente, ⏸️ aplazado (con el motivo y cuándo retomarlo). Todo debe resolverse de forma genérica, válida para cualquier país y fuente: los ejemplos españoles de un punto solo ilustran el fallo.

| # | Punto | Motivo | Esfuerzo | Estado |
|---|---|---|---|---|
| 1 | Consultas reales de usuarios | El ranking se validó con consultas generadas (semillas 42 y 7) y pulsaciones simuladas; faltan las de usuarios reales | S | ⬜ Cuando haya registro de consultas de un uso real |
| 8 | Latencia medida desde otra máquina | PROJECT.md §13 pide medir el cliente fuera del servidor. El autocompletado está en unos 5,6 ms de p99 frente al objetivo de 5 (presupuesto de 5 ms, aceptado como decisión de producto) | S | ⬜ Necesita el hardware de referencia o, al menos, otra máquina en la red |
| 12c | Nombres en lenguas ajenas al país | Los `name:*` de OSM en cualquier lengua (`name:la`, `name:en`…) se indexan igual que los del país y meten ruido al autocompletar: «Nova» da Vitoria-Gasteiz («Nova Victoria», en latín) | M | ⏸️ Aplazado (2026-10-17): poco valor medido. Casi todos los fallos son prefijos cortos que coinciden con un nombre secundario legítimo de una entidad importante («Villanueva» da Vilanova i la Geltrú), y penalizarlos empeoraría esos casos. Exigiría índices de palabras separados para nombres principales y secundarios |
| 13 | POIs famosos que aún fallan | «alhambra» (a 0,036 del municipio homónimo), «estación de atocha» (el nombre de la estación no lleva «estación»), «catedral de santiago» (gana la de Bilbao, que se llama así); «aeropuerto de palma» da La Palma, que es ambiguo | M | ⬜ No se arregla subiendo el peso de la fama (probado, AGENTS.md). Los hunde el texto, no la importancia |
| 14 | Interpolación de números de portal que faltan | Hoy se devuelve el número más cercano del mismo lado (a 10 como mucho, `approximate`) | M | ⏸️ Aplazado (2026-10-17). Un portal interpolado obliga a llevar número y posición propios en `Hit` (`geko-core`, todos los motores y la suite de conformidad) y a decidir su id público y qué devuelve `/place`. Antes, añadir a los conjuntos casos de números que faltan, para medir el beneficio |
| 18 | Fixture `.osm.pbf` para la conformidad de `source-osm` | Su suite solo prueba los metadatos y el error sin descargas, no el staging | S | ⬜ |
| 24 | Comparar con Pelias en la misma máquina | La latencia medida contra Pelias incluye red: geko responde en 0,3–0,5 ms de mediana en el servidor; Pelias, unos 60 ms con la red | M | ⬜ Necesita un Pelias local (Elasticsearch, varios GB de RAM) en una máquina con memoria libre |
| 26 | Licencias antes de abrir el servicio | El método de la base derivada (ODbL 4.6) está hecho y servido (`/v1/attribution`) | S | ⬜ Fuera del código: que lo revise un abogado y fijar `[serve] attribution_contact` |
| 35 | Tiempos de España con los packs leídos al arrancar | Una tanda de cada, una tras otra, dio fusión 287 y 332 s, índice 197 y 276 s e inversa 16 y 49 s, en etapas sin código cambiado (`main` frente a la rama de los packs, 2026-10-20): ruido de la máquina, sin confirmar | S | ⬜ Medir con A/B intercalado y la máquina en reposo (sin Docker ni navegador), unas 2 h |
| 37 | Teselas | Módulo `tiles` en su esqueleto: `tiles-planetiler` (descarga del jar y de los datos auxiliares, esquema Shortbread, profile propio), `tiles-pmtiles` con estilo, glyphs y sprites, y publicación estática (fase 3) | L | ⏸️ Aplazado por decisión (2026-10-18): primero el geocoder |
| 38 | POIs de registros oficiales | Hoy los POIs salen de OSM y Overture (fase 3) | M | ⬜ Cada registro, una declaración de un pack para un provider genérico |
| 39 | Wikidata | Importancia por enlaces y nombres multilingües (§6.1). Hoy la importancia sale de los nombres multilingües de OSM | M | ⬜ Solo si los conjuntos muestran fallos de importancia que OSM no resuelve |
| 40 | Índice fonético | Último paso de la cascada (§9.4, §9.5): claves fonéticas del idioma, dadas por el pack | M | ⬜ Medir antes cuántas consultas fallan por ortografía que no arreglan las erratas |
| 41 | Compilación optimizada | `rkyv`, SIMD, PGO entrenado con las consultas de referencia, binarios `x86-64-v3`/`x86-64-v2` o `multiversion` (§11, fase 4) | M | ⬜ Medir con instrucciones (`geko-bench`) y con el hardware de referencia |
| 42 | Diffs diarios de OSM y regeneración programada | Hoy cada build descarga los extractos enteros y se programa fuera de geko (§12, fase 5) | L | ⬜ |
| 43 | `BundleStore` con S3 y enrutador entre nodos | Hoy los bundles se sirven de disco o por HTTP (`bundles_url`), y `[serve] countries` reparte países sin un enrutador delante (fase 5) | M | ⬜ Cuando un nodo no baste |
| 44 | Opciones de configuración previstas | Elegir provider por configuración, capas opcionales (`[serve.layers]`), pesos del ranking (`[serve.ranking]`) (§4.1) | M | ⬜ Cada una, cuando haga falta: el provider, con su segunda implementación |
| 45 | Extracto del continente | Si los extractos de Geofabrik del ámbito pesan más que el del continente, bajar el continente (§7.4) | S | ⬜ |
| 46 | Módulos descargables | Cada módulo, un paquete firmado que el host baja y carga al arrancar (como los packs), y el geocoder, en su propio repositorio (`candymaps/geko-geocoder`), con los packs y los idiomas | L | ⬜ Plan en [MODULES_PLAN.md](MODULES_PLAN.md). Paso 1 hecho (2026-10-21): el geocoder, en `modules/geko-geocoder/`, detrás del contrato de `geko-module-sdk`, enlazado en el binario |
## 18. Rutas

- `geko` lee siempre `/etc/geko/geko.toml` y guarda siempre sus datos en `/var/lib/geko`: descargas (`raw/`), ficheros de trabajo (`staging/`, `relocated/`, `canonical/`, `segments/`) y bundles publicados (`bundles/`). Ni el `geko.toml` ni la línea de órdenes cambian estas rutas.
- Para generar en una máquina y servir en otra, la que genera ofrece su `/var/lib/geko/bundles` por HTTP (cualquier servidor web o almacenamiento de objetos) y la que sirve lo indica en `[serve] bundles_url`: en cada recarga baja la versión nueva a su propio `/var/lib/geko/bundles`, solo con los artefactos que cambiaron.
- Los packs de país se leen al arrancar de `/var/lib/geko/packs/<país>/`, solo los de los países que se piden. `geko plan`, `fetch`, `build` y `up` bajan los del ámbito que falten o hayan cambiado de `ghcr.io/candymaps/geko/countries/<país>` (`[packs] registry`), con la etiqueta del formato que entiende el binario (`format-2`), y solo los usan si llevan la firma de los packs publicados. Cada pack lleva su versión (`version` en `pack.toml`, publicada también como `pack-<n>`), que muestran `geko plan` y `geko build`. Un pack copiado a mano se usa tal cual y nunca se reemplaza; para probar uno sin publicar, `GEKO_PACKS=$PWD/countries` usa los del repositorio. Cada bundle lleva el pack con que se generó, y `geko serve` lee ese: la máquina que solo sirve no necesita la carpeta de los packs. La imagen de Docker no lleva ninguno: baja los de su ámbito al volumen.
- Las variables de entorno `GEKO_CONFIG` y `GEKO_DATA` cambian las dos rutas, y `GEKO_PACKS` la de los packs. Son para las comprobaciones (`ci/test.sh`) y la CI, que generan varios países a la vez y no escriben en `/etc` ni en `/var/lib`.
- Al cambiar el ámbito, lo que ya no está en él se queda en `/var/lib/geko`, por si vuelve. Se borra con `geko purge`:
  - `geko purge data`: las descargas y los ficheros de trabajo de los países, las regiones y las fuentes que la configuración ya no pide. `geko build --purge` y `geko up --purge` hacen lo mismo antes de generar.
  - `geko purge packages`: los paquetes que bajaron los módulos (los packs de país del geocoder) de los países fuera del ámbito; con `--all`, todos (se vuelven a bajar al generar). Los copiados a mano se quedan.
  - `geko purge`, sin más: todo lo anterior.
  - Las versiones antiguas de los bundles las retira `[build] keep`: por defecto solo se conserva la que se sirve; con 2 se puede volver a la anterior. Un servicio que aún tenga cargada la versión retirada sigue funcionando hasta cargar la nueva.

## 19. Estado

Fase 1 terminada (las fases, en la hoja de ruta, §15). Resultados en las Islas Baleares (§13.4 bis), con consultas de referencia que no se usaron para afinar el ranking (semilla 7):

| | geko | Pelias (OSM) |
|---|---|---|
| Acierto en top-1, comparación justa de motor | **95,9 %** | 63,7 % |
| Acierto en top-1, producto completo (con CartoCiudad) | **93,5 %** | 38,2 % |
| Latencia por petición HTTP (mediana) | **0,1–3 ms** (local) | 60–90 ms (en la nube, con red) |
| Caudal (concurrencia 16, máquina de desarrollo) | búsqueda 3.372 QPS, autocompletado 7.409 QPS, inversa 67.718 QPS | — |

Hecho tras la fase 1 (el detalle, en `git show 8679454:docs/BACKLOG.md`; lo pendiente, en la sección 17):

- **Generación:** incremental, que solo rehace las etapas cuyo código o entradas cambian (Baleares sin cambios: de 81 a 0,5 s).
- **Puertos:** cada uno tiene su suite de conformidad.
- **CI:** en GitHub Actions, en cada push a `main`: comprueba lo que cambió con `ci/test.sh` y, si pasa, lo publica.
- **España completa:** ya se genera y se sirve (§13.4 ter):

| España | |
|---|---|
| Datos | 16,1 M de portales, 1,27 M de calles, 685.000 lugares, 880.000 POIs |
| Descarga (OSM + CartoCiudad), solo la primera vez | 36 min |
| Generación | 16 min sin descargas, con Overture (2026-10-20; 6,6 min sin Overture el 2026-10-16), 2,9 GB de pico en el índice |
| Segmento | índice de 1,8 GB e inversa de 687 MB (mapeada) |
| Servicio | carga en 0,97 s, 932 MB residentes |
| Acierto en top-1: dirección completa / con errata / sin municipio, con foco | 92–96 % / 90–95 % / 94 % |

- **Packs de país como datos:** leídos al arrancar, firmados y publicados en ghcr.io; `geko` baja los de su ámbito, y la imagen de Docker sirve para cualquier combinación de países sin llevar ninguno (§2.4).
- **Segundo país:** Luxemburgo, con su pack (`countries/lu/`) y su golden, sin tocar el motor salvo arreglos genéricos ([countries/README.md](../countries/README.md)).
- **Host y módulos** (2026-10-21, [MODULES_PLAN.md](MODULES_PLAN.md), paso 1): el host (`geko-app`, `geko-server`) solo conoce los módulos por el contrato de `geko-module-sdk`. El geocoder vive en `modules/geko-geocoder/`, con su configuración (`geko-geocoder.toml`) y sus rutas bajo `/geocoder/`. Se quitaron los modos compatibles con Pelias y Photon.
- **Servicio:** página de demostración en `/geocoder/`, `geko up` (generar y servir), `lang`, CORS, claves de API y límite de peticiones; red de seguridad contra duplicados con su contador en `/metrics`.
- Las últimas medidas completas, del 2026-10-18, en [docs/2026-10-18_ANALYSIS_RESULTS.md](2026-10-18_ANALYSIS_RESULTS.md).

## 20. Comprobaciones y evaluación

Todas viven en `ci/test.sh`, igual en local que en la CI (GitHub solo lo llama), que solo ejecuta pruebas: `--variant` dice qué se prueba y `--scope`, sobre qué datos. Los datos del golden los prepara aparte `ci/golden-snapshot.sh`. Cada prueba se usa donde tiene sentido:

- **Antes de cada push, en local**: `make test`, que lanza solo `.githooks/pre-push` (se activa una vez por clon con `git config core.hooksPath .githooks`; `git push --no-verify` lo salta). Es todo lo determinista, con el golden de todos los países.
- **En cada push a `main`, en la CI** (`ci.yml`): lo determinista de lo que cambió. El código, si cambió el motor; los contratos, si cambió el motor o algún pack; y el golden de cada país, de todos si cambió el motor o solo de los packs cambiados. Si todo pasa, publica los packs cambiados y, si el push toca lo que entra en ella, la imagen, con la versión del `Cargo.toml`. Las demás ramas no tienen CI.
- **Solo en local y a demanda** (o en un runner propio con el hardware de referencia de §1.1): España completa (`make test-regression`) y toda latencia, caudal o memoria, los datos frescos (`GEKO_GOLDEN_FRESH=1`) y la comparación con Pelias.

```bash
ci/test.sh [--variant <variantes>] [--scope <ámbitos>] [--include-performance]
```

| `--variant` (por defecto, todas) | Qué prueba |
|---|---|
| `code` | clippy (avisos como errores), reglas de dependencias, licencias y avisos de seguridad (cargo-deny) y cada combinación de features |
| `contracts` | tests unitarios y suites de conformidad de cada puerto |
| `results` | aciertos consulta a consulta y en cada pulsación, calibración de `confidence` y duplicados |
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

### 20.1 Instantáneas del golden

**Para qué sirven.** El golden son las consultas de referencia de cada pack (`countries/<país>/golden/`): miles de direcciones, calles, lugares y POIs con su resultado esperado. Cada cambio se compara con su línea base consulta a consulta, con tolerancia 0. Eso solo vale si los datos no cambian entre una medida y otra: con datos frescos, OSM cambia a diario y no se sabría si una consulta perdida viene del código o de los datos. Por eso el golden no descarga, sino que usa una **instantánea**: las descargas de su ámbito (Baleares, Luxemburgo) congeladas en un archivo, sobre el que se midió la línea base. Con ella, solo un cambio del código puede cambiar los resultados. Renovarla solo hace falta para que el golden mida con datos actuales, cuando las fuentes se han alejado bastante.

**Dónde están.** La instantánea de cada país es la que nombra `countries/<país>/golden/snapshot`, con su sha256, y se guarda como paquete privado en `ghcr.io/candymaps/geko/countries/<país>/golden:<instantánea>`. La CI la lee con su propio token y corre el golden en cada push a `main` que puede cambiar sus resultados: el de todos los países si cambia el motor o `ci/`, solo el de uno si solo cambia su pack. En local, si no está en disco, se baja de ahí: hace falta una vez `oras login --registry-config ~/.config/oras/config.json ghcr.io -u <usuario>` con un token con `read:packages`.

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

4. Revisa la tabla de datos del informe (`entities …`: línea base y ahora), con entidades, duplicados e integridad, antes de aceptarla (AGENTS.md).
5. Commit de `golden/snapshot` junto con `golden/baseline`. La CI pasará el golden sobre la instantánea nueva en el push a `main`, y para eso tiene que estar publicada: una hecha con `golden-snapshot-local` se publica antes con `ci/golden-snapshot.sh publish --scope <país>`.

`ci/test.sh --scope spain` compara el árbol de trabajo con el último commit subido (`GEKO_BASE` para otro), cuyos binarios compila en un worktree (`/var/lib/geko/ab/base`). Lo que no depende de lo ocupada que esté la máquina (aciertos, datos, tamaños, memoria) lo compara con la línea base de ese commit, que mide sola una vez por commit y por datos y guarda junto a ellos (`/var/lib/geko/check-baseline/`). Las instrucciones de cada ruta las cuenta con valgrind en los dos binarios, cada uno sobre su bundle. Lo que sí depende de la máquina (latencia, caudal, acierto con carga), solo con `--include-performance`, lo mide en los dos servidores a la vez, en cinco tajadas por turnos de cada combinación de ruta y concurrencia, y compara la mediana de las tajadas: así una máquina más o menos cargada pesa igual en los dos, y una racha en una tajada no mueve el resultado. Con 8 núcleos o más, los servidores van fijados a 4 núcleos, como el hardware de referencia, y el cliente a los demás.

Para añadir un país, la guía está en [countries/README.md](../countries/README.md).

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

## 21. Desarrollo y uso local

### 21.1 Con Docker

La imagen, `ghcr.io/candymaps/geko`, lleva el binario y una configuración de ejemplo (`docker/geko.toml`), y ningún pack de país: al generar, `geko` baja los de su ámbito al volumen. Sirve igual para un país que para varios. Se monta la propia en `/etc/geko/geko.toml` (con `accept_licenses = true`; escucha siempre en `0.0.0.0:8080`) y un volumen en `/var/lib/geko`, donde quedan las descargas y los bundles:

```bash
docker build -t geko .
docker run -p 8080:8080 -v geko-data:/var/lib/geko -v ./geko.toml:/etc/geko/geko.toml geko
```

La orden por defecto es `up`: empieza a servir enseguida y, mientras, descarga y genera en segundo plano lo que pide la configuración. Hasta que se publica el primer bundle responde 503; Luxemburgo tarda unos 2 minutos desde un volumen vacío, y España completa, una media hora la primera vez. Después sigue sirviendo y, cada `[data] max_age_days` (30 días), comprueba las fuentes y genera de nuevo lo que cambió, sin cron y sin reiniciar: sirve el bundle de antes hasta que el nuevo está publicado, y lo recoge sin cortes. Si una generación falla, se queda el que sirve y lo vuelve a intentar al cabo de una hora. Un pack de país nuevo se usa al reiniciar el contenedor. Con `serve` en vez de `up`, solo sirve lo que ya hay en `/var/lib/geko` o, con `[serve] bundles_url`, lo que publica otra máquina. La imagen ocupa 162 MB.

### 21.2 Atajos: `make`

En Linux y macOS, `make <objetivo>` en la raíz del repositorio. En Windows, `make.cmd`, que lo ejecuta dentro de WSL: `make <objetivo>` en cmd y `.\make <objetivo>` en PowerShell. `make help` los lista:

- `make serve`: sirve el bundle publicado en el puerto 8080 de todas las interfaces (`LISTEN=127.0.0.1:8080` lo limita a la propia máquina): `http://localhost:8080`, con la página de prueba en `/` y la API en `/docs`. `make up` descarga y genera antes lo que pide la configuración.
- `make query Q="gran via 1, madrid"`, `make parse Q="…"` y `make reverse LAT=40.4168 LON=-3.7038`.
- `make test`, `make test-golden-es`, `make test-golden-lu` y `make test-regression`: las comprobaciones de `ci/test.sh`.
- `make image`: la imagen de Docker con el binario de la última compilación, sin compilar dentro de Docker. Se niega si el binario pide una glibc más nueva que la de la imagen (2.36), porque no arrancaría.
- `make dev`: compila el código tal como está en la rama, cambios sin commit incluidos, construye la imagen `geko:dev` y vuelve a crear el contenedor `geko-dev`. Este sirve en `http://localhost:8080` (`DEV_PORT=` lo cambia): la demo en `/` y la API en `/docs`. Usa la configuración de prueba `docker/dev.toml` (Baleares, con `keep_downloads = true`; `DEV_CONFIG=` usa otra) y los packs del repositorio, sin publicar. Sus datos van al volumen `geko-dev-data`, que se conserva entre ejecuciones; `docker volume rm geko-dev-data`, con el contenedor parado, lo borra todo. Desde WSL necesita la integración de Docker Desktop con la distribución (Settings → Resources → WSL integration).
  - Volver a lanzarlo con el código cambiado recompila solo lo que cambió y sustituye el contenedor por uno con la imagen nueva, unos segundos sin servicio. El contenedor nuevo sirve enseguida el bundle que ya había en el volumen, salvo que el cambio altere su formato: entonces responde 503 hasta publicar el nuevo.
  - En segundo plano rehace solo las etapas cuyo código o entradas cambiaron, sin volver a bajar nada, y recoge el bundle nuevo sin reiniciar. Un cambio de ranking o de búsqueda no rehace ningún dato; uno en el staging de una fuente rehace ese staging y lo que depende de él. Los cambios de los packs y de `docker/dev.toml` también entran, porque van montados.
  - Solo toca el contenedor `geko-dev`. El de «Instalación y uso» del README (`geko`) usa el mismo puerto: para tener los dos a la vez, `make dev DEV_PORT=8081`.
- `make release-packs`: firma los packs de país con `~/.config/geko/pack-signing.key` y sube los que cambiaron a `ghcr.io/candymaps/geko/countries/<país>` (`ci/packs.sh publish`, con `oras`, o con su imagen si no está instalado).
- **Publicación automática:** el trabajo `publish` de `ci.yml`, tras pasar las comprobaciones en `main`, publica los packs que cambian y, si el push toca algo que entra en ella (no solo documentación o packs), la imagen, etiquetados `<versión>-<commit>` con la versión del `Cargo.toml`. Para sacar una versión basta subirla en `[workspace.package]`: el workflow añade `<versión>` a packs e imagen, `latest` a la imagen y crea la etiqueta de git `v<versión>` en el commit subido, con su release de GitHub. Necesita el secreto `GEKO_PACK_SIGNING_KEY`.
- `make release`: compila, hace la imagen y la sube a `ghcr.io/candymaps/geko` con tres etiquetas: la versión y el commit (`0.1.0-6fb024c`), la versión y `latest`. Solo con todo en commit, para que la imagen sea la de ese commit, y tras `docker login ghcr.io` con un token con permiso `write:packages`.

Compilan `geko` en `--release` y lo instalan antes de usarlo (`make install`, con `sudo`): el binario en `/usr/local/bin/geko`, el `geko.toml` del repositorio en `/etc/geko/geko.toml` si aún no hay ninguno, y `/var/lib/geko` a nombre del usuario. El `/etc/geko/geko.toml` es el que se edita (ámbito, `accept_licenses = true`…); el del repositorio se queda con `accept_licenses = false`. El puerto se cambia con `LISTEN=…`.

### 21.3 Prueba local (Islas Baleares), desde PowerShell

Primero, la ruta del repositorio en WSL, por ejemplo `$GEKO_REPO = '/mnt/d/geko'` para `D:\geko`. Desde la línea de órdenes:

```powershell
wsl -e bash -lc "cd '$GEKO_REPO' && ~/.cache/geko-target/release/geko geocoder query 'carrer de sant miquel 30, palma'"
wsl -e bash -lc "cd '$GEKO_REPO' && ~/.cache/geko-target/release/geko geocoder reverse 39.5675 2.6482"
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
