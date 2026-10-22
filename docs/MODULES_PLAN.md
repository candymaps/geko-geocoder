# Módulos descargables: plan

Estado: paso 1 hecho (2026-10-21, commit b814bd0); paso 2 (adaptadores dinámicos) hecho (2026-10-22), con la medición A/B con España: se elige `DylibModule`, que falta poner por defecto; paso 3 (registro y versiones) hecho (2026-10-22); paso 4 (CI, imagen y Makefile) hecho (2026-10-22), salvo las pruebas cruzadas, que pasan al paso 7; paso 5 (`tiles`) hecho (2026-10-22); después, las caídas de `ProcessModule` y el paso 6. Punto 46 de PROJECT.md §17.

## Objetivo

Que cada módulo funcional (hoy `geocoder`; después `tiles` y otros) sea un **paquete que el host descarga y carga en tiempo de ejecución**, como hoy los packs de país, en vez de compilarse dentro del binario. A medio plazo, el geocoder vivirá en su propio repositorio, `candymaps/geko-geocoder`, y `candymaps/geko` (el host) lo bajará de sus packages cuando `geko.toml` lo active (`[modules] geocoder = true`).

## Reparto

| | Host (`candymaps/geko`) | Módulo (`candymaps/geko-geocoder`) |
|---|---|---|
| Código | CLI, configuración, ámbito (códigos ISO), `Downloader`, licencias aceptadas, publicación y recarga de bundles, HTTP común (`/health`, `/metrics`, claves de API, CORS, límites, atribución), registro de módulos y descarga de artefactos firmados | Fuentes (OSM, Overture, OGC), fusión, índice, inversa, motor de búsqueda, caché de respuestas, `/geocoder/v1/*`, demo (`/geocoder/`), órdenes de depuración |
| Puertos | `Module` (el contrato, en la SDK) | `SourceProvider`, `TextNormalizer`, `AddressParser`, `GeocodeIndexBuilder`, `GeocodeEngine`, `ReverseIndex`, con sus suites de conformidad |
| Tipos | Los de la SDK: país, región, ámbito, licencia, petición de descarga, artefacto, petición y respuesta HTTP, configuración opaca | `Hit`, `ParsedQuery`, `QueryContext`, `Coord`, `PublicId`… |
| Datos | Ninguno | Packs de país (`countries/`, con `golden/`) e idiomas (`languages/`: traducen tipos de vía en las consultas, nada que use otro módulo) |
| Documentación | PROJECT.md §2–§4, §7, §12, §18 | PROJECT.md §5–§9, §13; «Optimizaciones» de AGENTS.md; `FUSION_METHOD.md` |

Regla: **si un tipo, un dato o un puerto solo tiene sentido para un módulo, vive en el módulo.** El host no sabe qué es un pack: el módulo baja los paquetes de datos que necesita para el ámbito con el mecanismo genérico de artefactos firmados de la SDK, el mismo con que el host baja los módulos (decidido, 2026-10-21: así el host no necesita conocer su destino ni su formato).

## La SDK (el contrato)

La define y la publica el host, en su propio repositorio (`candymaps/geko`): el puerto pertenece a quien lo consume. Cada módulo depende de ella y la implementa. GitHub Packages no tiene registro de Cargo, así que se publica como etiqueta de git del repositorio del host (`sdk-<n>`), y el módulo la usa como dependencia de git:

```toml
geko-module-sdk = { git = "https://github.com/candymaps/geko", tag = "sdk-1" }
```

Si el repositorio del host es privado, la CI del módulo necesita un token de lectura. La SDK contiene solo:

- `describe`: nombre, versión, contrato, plataforma, capacidades opcionales, rutas HTTP que atiende (método, patrón, si van al pool de búsqueda y si son públicas sin clave de API), su página de inicio, su `openapi.yaml`, lo que añade a `/v1/attribution` y sus órdenes.
- `plan` y `fetch`: del ámbito y su apartado de configuración a los datos que hacen falta, descargados con el `Downloader` que ofrece el host (caché, `offline`, licencias, descargas en paralelo).
- `prepare`: lo que necesitan las órdenes que generan (el geocoder baja ahí los packs del ámbito).
- `build`: sus etapas por país, con sus sellos de §8.3 en su propio directorio de trabajo, y las partes del bundle que deja (directorios de origen y destino), sus licencias, sus metadatos (el geocoder, sus segmentos) y sus etapas para el informe. El host las publica como ficheros opacos (ruta, hash, tamaño), y el manifiesto (formato 2) guarda de cada módulo su nombre, versión, contrato y metadatos. Después, `published` (el geocoder borra las descargas) y `next_build` (cuándo volver a generar en `geko up`).
- `load(bundle)`, `handle(Request) -> Response` (síncrono: el host monta axum, aplica acceso y límites, y llama desde su pool), `metrics(&mut String)`, `shutdown`.
- `command(args)`: órdenes del módulo (`geko geocoder query …`), reenviadas tal cual.
- `purge`: sus datos que la configuración ya no pide (`geko purge data`) y sus paquetes de fuera del ámbito (`geko purge packages`; antes `geko purge country-packs`).
- Configuración: el host pasa al módulo el contenido de su fichero (`geko-geocoder.toml`, sección siguiente) sin interpretarlo; el módulo lo valida.

`router()` desaparece del puerto: un `Router` de axum no cruza ni un proceso ni una `.so`.

**Rutas bajo el nombre del módulo.** El módulo declara sus rutas relativas (`v1/search`) y el host las monta siempre bajo `/<módulo>/`: dos módulos no pueden pisarse, y uno nuevo no toca las rutas de los demás. El geocoder queda en `/geocoder/v1/search`, `/geocoder/v1/search/structured`, `/geocoder/v1/autocomplete`, `/geocoder/v1/reverse`, `/geocoder/v1/place/{id}` y `/geocoder/v1/batch`, con su demo en `/geocoder/`; las teselas, en `/tiles/…`. El host se queda sin prefijo con lo común: `/health`, `/metrics`, `/v1/attribution` (licencias de todo el bundle, más lo que añade cada módulo: `{bundle, contact, sources, geocoder: {derivative_database: {license, notice, method}}}`), `/openapi.yaml` (solo las rutas del host) y `/docs` (Swagger con la del host y la de cada módulo). El método de la base derivada es del geocoder: `/geocoder/v1/attribution/method`. `/` lleva a la demo del primer módulo activo que tenga una. Se actualizan `openapi.yaml`, la demo, `geko-eval` (la URL de las consultas), `ci/test.sh` y la documentación de la API (PROJECT.md §10).

**Sin modos compatibles.** Se eliminan los modos Pelias (`/pelias/v1/*`) y Photon (`/photon/*`): `pelias.rs` y `photon.rs` del geocoder, su comprobación en `ci/test.sh` (variante `results`), su parte de `openapi.yaml` y de la documentación (PROJECT.md §10, §15; AGENTS.md). La comparación con Pelias de §13.2 (`--scope pelias`) se queda: consulta un Pelias de verdad con el cliente de `geko-eval`, no el modo de geko.

Un cambio del módulo (ranking, formato del índice, fuente nueva, ruta nueva, opción nueva, métrica, orden de depuración, datos del pack) **no toca el host**. Solo lo toca una capacidad nueva del contrato (streaming, WebSockets, tareas de fondo, un servicio nuevo del host) o un cambio incompatible de la SDK. Los cambios del contrato son aditivos siempre que se pueda: campos opcionales y capacidades que se consultan en `describe` antes de usarlas.

## Configuración

Cada módulo tiene su propio fichero, `/etc/geko/geko-<módulo>.toml` (`geko-geocoder.toml`), junto al `geko.toml`; con `GEKO_CONFIG`, se busca en la carpeta de ese fichero. `geko.toml` se queda solo con lo del host: qué módulos se activan y de dónde se bajan, el ámbito, las descargas, los bundles y el HTTP común.

- El host lee el fichero del módulo solo si el módulo está activo (un módulo desactivado no abre ficheros) y se lo pasa sin interpretarlo. Si no existe, el módulo usa sus valores por defecto. Lo valida el módulo, con errores que nombran el fichero y la clave.
- El ejemplo con todas las opciones y sus valores por defecto vive con el módulo (`modules/geko-geocoder/geko-geocoder.toml`, y después en su repositorio), y lo documenta él. `make install` lo copia a `/etc/geko/` si no hay ninguno, como hace con `geko.toml`.
- Una opción nueva del geocoder no toca ni el host ni `geko.toml`.
- Las opciones que afectan a la generación (las de las fuentes) entran en los sellos de las etapas del módulo, como hoy.

| Opción de hoy (`geko.toml`) | Pasa a |
|---|---|
| `[modules] geocoder`, `tiles` | Se quedan en `geko.toml`, con `[modules.<módulo>] version` y `registry` (nuevos) |
| `[[scope]]` (`country`, `regions`) | Se queda en `geko.toml`: es el ámbito de todos los módulos. El host solo comprueba los códigos ISO; que una región exista en el pack lo comprueba el geocoder |
| `[data]` (todas) | Se quedan: las usa el `Downloader` del host |
| `[sources.osm]`, `[sources.overture]`, `[sources.cartociudad]` y las demás fuentes | `geko-geocoder.toml`, `[sources.*]` |
| `[packs] registry` | `geko-geocoder.toml`, `[packs] registry` (los packs son del geocoder) |
| `[build] keep` | Se queda: versiones de los bundles |
| `[serve] listen`, `reload_secs`, `bundles_url`, `cors_origins`, `api_keys`, `rate_limit_per_s`, `rate_limit_burst`, `attribution_contact` | Se quedan: HTTP común y recarga de bundles |
| `[serve] countries`, `preload`, `search_budget_ms`, `autocomplete_budget_ms`, `slow_query_ms`, `same_entity_m`, `batch_max`, `cache_entries` | `geko-geocoder.toml`, `[serve]` (segmentos, índice y API del geocoder) |

```toml
# /etc/geko/geko.toml (host)
[modules]
geocoder = true
tiles    = false

# [modules.geocoder]
# version  = "1.4.2"                          # opcional: por defecto, el último compatible
# registry = "ghcr.io/candymaps/geko-geocoder"

[[scope]]
country = "ES"
regions = ["ES-PM"]

[data]       # refresh, max_age_days, keep_downloads, offline, accept_licenses, parallel_downloads
[build]      # keep
[serve]      # listen, reload_secs, bundles_url, cors_origins, api_keys, rate_limit_*, attribution_contact
```

```toml
# /etc/geko/geko-geocoder.toml (módulo geocoder)
[packs]
registry = "ghcr.io/candymaps/geko-geocoder/countries"

[sources.osm]
mirror = "auto"

[sources.overture]           # enabled, base, release, min_confidence
[sources.cartociudad]        # cualquier clave de countries/es/sources/cartociudad.toml

[serve]
countries              = []
preload                = true
search_budget_ms       = 15
autocomplete_budget_ms = 5
slow_query_ms          = 50
same_entity_m          = 100
batch_max              = 100
cache_entries          = 0
```

Una opción que el host ya no conoce y sigue en `geko.toml` (p. ej. `[serve] search_budget_ms` tras actualizar) da un error al arrancar que dice a qué fichero se ha movido, en vez de ignorarse (decidido, 2026-10-21): ignorarla cambiaría en silencio el comportamiento tras actualizar (un presupuesto de tiempo que vuelve a su valor por defecto), y `geko.toml` ya rechaza las claves que no conoce (`deny_unknown_fields`).

## Adaptadores

El host solo maneja `Arc<dyn Module>`; el adaptador decide cómo se ejecuta el paquete. El manifiesto del paquete dice cuál usar (`kind = "process" | "dylib"`).

| Adaptador | Qué es | Para qué |
|---|---|---|
| `StaticModule` | Enlazado en el binario, como hoy | Monorrepo (paso 1), tests, golden con valgrind |
| `ProcessModule` | El host lanza el ejecutable del paquete y le habla por un socket Unix con mensajes versionados | Aislado (un fallo no tira el host), sin acoplamiento de compilador, el build devuelve la memoria al acabar, cambio de versión en caliente (arrancar el nuevo, cambiar, parar el viejo). Cuesta un salto por petición |
| `DylibModule` | `dlopen` de la `.so` del paquete, con una frontera `extern "C"` mínima que pasa los mismos mensajes serializados que `ProcessModule` (decidido, 2026-10-21, en vez de `abi_stable`: un solo protocolo para los dos, y el saludo con el contrato es el primer mensaje) | Mismo proceso, sin salto. Cada función exportada envuelta en `catch_unwind` (un pánico que cruza la frontera aborta el proceso, y choca con `panic = "unwind"`); cada lado libera su memoria (el host usa mimalloc). Una `.so` no se descarga con seguridad: cambiar de versión exige reiniciar |

Se implementan los dos dinámicos y se miden A/B con España completa (latencia, caudal con 16 a la vez, instrucciones). Si el salto del proceso queda dentro del ruido, `ProcessModule`; si se nota en la mediana de 0,1–0,5 ms, `DylibModule`.

## Versiones y compatibilidad

- **El host fija el contrato, no la versión del módulo.** Cada release del host declara los contratos que entiende (p. ej. 2–3). Cada publicación del módulo lleva las etiquetas de su versión (`1.4.2`), de su commit y de su contrato (`contract-3`), que apunta al último módulo de ese contrato. Con el módulo activo, el host baja el último del contrato más alto que entiende: un arreglo del geocoder llega sin release del host.
- **Versión fijada, opcional:** `[modules.geocoder] version = "1.4.2"` (producción, volver atrás). También `[modules.geocoder] registry`, por defecto `ghcr.io/candymaps/geko-geocoder`.
- **El bundle fija la versión con la que se sirve.** El manifiesto guarda la versión exacta del módulo que lo generó (como hoy lleva el pack). `geko serve` usa esa y la baja si falta (es código, no fuentes: no rompe «serve nunca descarga fuentes»). En `geko up`, el build usa el último compatible (o el fijado) y, al pasar el bundle nuevo a `current`, el servicio cambia al módulo de ese bundle en la misma recarga. La versión fijada vale para generar; si difiere de la del bundle servido, aviso en el log.
- **Versión incompatible con el host:**
  1. Antes de descargar el binario, el host lee solo los metadatos de esa versión (anotaciones del manifiesto OCI o un `module.json` pequeño): contrato, plataforma, host mínimo. Si no encaja, error en `plan`, `fetch`, `build`, `up` y `serve`, antes de tocar fuentes, con las versiones compatibles y la salida («quita `version` para usar la última compatible»). Nunca se elige otra en silencio.
  2. Tras descargar, se repite la comprobación con el manifiesto firmado.
  3. Al cargar, saludo del adaptador: primer mensaje con el contrato, en los dos adaptadores dinámicos. Cubre también los módulos locales (`GEKO_MODULES`).
  - Si el host deja de entender el contrato del bundle en servicio: no carga el nuevo y sigue con el que tiene (error en el log y en `/health`); si no hay ninguno cargado, `serve` no arranca y dice que hay que regenerar o volver a un host compatible. En `geko up`, una versión fijada incompatible hace fallar el build y se sigue sirviendo el bundle actual.
- **Publicación del host:** en la CI del host, el trabajo `publish` de `ci.yml` comprueba en el registro, antes de publicar nada, que hay algún módulo publicado para cada contrato que declara el host. Si falta, no publica la imagen, ni crea la etiqueta `v<versión>`, ni la release de GitHub. Para no bloquearse al estrenar un contrato, el módulo se publica antes: su CI prueba con el host compilado desde la etiqueta de la SDK (`sdk-<n>`), no con una imagen publicada. Dejar de soportar un contrato es versión mayor del host, anunciada.
- **Errores claros** para: ningún módulo del contrato, versión fijada inexistente, firma inválida, `offline` sin el módulo. Con el registro inalcanzable, la copia en disco.
- `geko plan` muestra la versión de cada módulo, su contrato y si es fijada o la última compatible.

## Descarga y verificación

Lo genérico de `geko-country-pack` (`registry.rs`, `signature.rs`) pasa a un crate de artefactos firmados (`geko-artifact`, en el paso 3), que usan el host y, por la SDK, los módulos: OCI con etiquetas, firma ed25519 contra `TRUSTED_KEYS` y hash de cada fichero, copia en disco si el registro falla, `offline`, copias a mano sin `.digest`, directorio local sin descarga y `purge`. Sirve para:

- **Módulos**, por plataforma (`linux-x86_64`), en `/var/lib/geko/modules/<módulo>/<versión>/`; `GEKO_MODULES` usa los locales y nunca descarga. Solo se ejecuta código firmado, salvo el de ese directorio local.
- **Paquetes de datos de un módulo** (los packs de país del geocoder), que baja el propio módulo dentro de su directorio. El formato de los packs (`format-2`) pasa a ser cosa del módulo.

`geko purge modules` borra los módulos que ya no pide la configuración ni ningún bundle conservado, y `geko purge packages` los paquetes de datos de fuera del ámbito.

**Imagen de Docker del host sin módulos.** La imagen `ghcr.io/candymaps/geko` es solo el host; los módulos no son imágenes de Docker, sino artefactos OCI (`ghcr.io/candymaps/geko-geocoder`), como los packs. Al arrancar, baja al volumen (`/var/lib/geko/modules/`) los que estén activos en `geko.toml`, como hoy los packs del ámbito; uno desactivado no se baja. En los arranques siguientes usa la copia del volumen, y con el registro inalcanzable arranca con ella. Sin copia y sin red, o con `offline` y sin el módulo, no arranca y lo dice. La imagen no cambia al publicar un módulo: solo con el código del host.

**Idiomas.** Siguen compilados dentro del binario del geocoder (`include_str!` en `text-rules`), así que se descargan con él. Pasarlos a un paquete de datos, como los packs, solo compensa si hay que actualizar un idioma sin sacar una versión del geocoder.

## Pasos

Cada paso deja el golden de `es` y `lu` igual (tolerancia 0) y el segmento idéntico byte a byte.

1. **Separar el geocoder dentro del mismo binario (`StaticModule`).**
   - `crates/geko-module-sdk` con el contrato de arriba.
   - Al geocoder: la fusión de `geko-build`, las etapas de índice e inversa de `geko-app/src/plan.rs`, las fábricas de `geko-app/src/sources.rs`, `GeocoderConfig` (`geko-app/src/modules.rs`), `parse`, `query` y `reverse` de `geko-app/src/main.rs`, la creación del normalizador y el analizador (hoy en `geko-country-pack`), los providers, sus puertos, sus suites de conformidad y los tipos de `geko-core` que solo usa él.
   - Ponerlo en `modules/geko-geocoder/` (hecho: dentro del workspace principal hasta extraer el repositorio en el paso 7), dependiendo del host solo por la SDK, con una regla de dependencias en `ci/test.sh --variant code` que lo compruebe. Con él, `countries/` y `languages/`.
   - El host deja de leer los packs: solo baja los paquetes que el módulo declara.
   - Rutas bajo `/geocoder/` y eliminación de los modos Pelias y Photon. Es el único cambio de este paso que cambia lo que se ve desde fuera; el golden debe dar los mismos aciertos con las rutas nuevas.
   - Configuración: las opciones del geocoder salen de `geko.toml` (y de `geko-app/src/config.rs`) a `geko-geocoder.toml`, leído por el módulo; el error para las opciones movidas; `make install`, `ci/test.sh` y la imagen de Docker copian también `geko-geocoder.toml`.
2. **Adaptadores dinámicos.** `ProcessModule` y `DylibModule`; el módulo se compila como ejecutable y como `.so` en el mismo repositorio, y el host lo carga de `GEKO_MODULES`. Medición A/B (valgrind tendrá que seguir al hijo, `--trace-children=yes`). Se elige adaptador.
   - Hecho, `ProcessModule` (2026-10-22). El protocolo está en la SDK (`geko_module_sdk::process`): tramas con la longitud de la cabecera y de la carga (`u32`), la cabecera en JSON y la carga en bruto (los cuerpos de peticiones y respuestas); la primera trama es el saludo del módulo con su contrato, y la primera llamada, `Open` con el `Context`, que responde con `describe`. El módulo atiende peticiones y métricas en 2 hilos por núcleo, y el resto (builds…) cada una en su hilo; un pánico es un error de esa llamada (500 en una petición). El lado del host está en `geko-app` (`process.rs`): lanza el ejecutable en su propio grupo de procesos (un Ctrl-C solo llega al host, que lo apaga tras las peticiones en curso), le pasa el socket en `GEKO_MODULE_SOCKET` y, si el proceso muere, las llamadas en curso fallan y la siguiente lo relanza con el último bundle cargado.
   - Hecho, `DylibModule` (2026-10-22). La `.so` exporta con `geko_module_sdk::export_module!` cinco funciones `extern "C"` (`geko_module_hello`, `_open`, `_call`, `_free`, `_close`) que pasan las mismas tramas que el socket (`geko_module_sdk::wire`); cada función envuelve su trabajo en `catch_unwind`, y cada lado libera su memoria (`geko_module_free`). El host la abre con `libloading` y no la descarga nunca. El geocoder se compila como `.so` en un crate aparte, `modules/geko-geocoder/dylib` (`libgeko_geocoder.so`, con su mimalloc y su `tracing`), para que enlazarlo en el host no compile también la `cdylib`. Lo común a los dos adaptadores (de cada método de `Module`, un mensaje) está en `geko-app/src/remote.rs`.
   - `GEKO_MODULES`: un directorio con uno por módulo (`<módulo>/module.toml`, con `kind = "process" | "dylib"` y `entry`, el ejecutable o la biblioteca), que sustituye al enlazado y nunca se descarga. `make modules [MODULE_KIND=dylib]` deja el geocoder en `$(CARGO_TARGET_DIR)/modules/geocoder/`.
   - Golden de Baleares (`results`) con los dos: los mismos aciertos y la misma calibración que enlazado.
   - Medición A/B con España (2026-10-22, `GEKO_BASE=f8a3c98 GEKO_SPAIN_ADAPTERS=1 make test-regression`, sin valgrind): el canonical y el bundle, idénticos byte a byte a los de antes de los módulos; los aciertos bajo carga, iguales (una consulta de 896 menos, por el presupuesto). El geocoder enlazado frente al mismo como proceso y como `dylib`, sobre el mismo bundle y en turnos, con 16 a la vez:

     | Ruta | `ProcessModule` | `DylibModule` |
     |---|---|---|
     | Búsqueda | −19 % de caudal, p99 22,3 frente a 18,7 ms | +1 % |
     | Autocompletado | −20 %, p99 19,6 frente a 16,0 ms | −2 % |
     | Inversa | −55 %, mediana 1,82 frente a 0,79 ms | −12 %, p99 1,87 frente a 1,48 ms (ruido: el mismo binario enlazado dio de 16.100 a 21.600 QPS en la misma tanda) |
     | Apertura | 2,9 s | 0,5 s |

     Con una petición a la vez, el proceso añade unos 0,1–0,2 ms a la mediana (inversa, de 0,14 a 0,25 ms); la `dylib`, nada medible. Lo del proceso no es ruido: pierde en todas las rutas y concurrencias, siempre en el mismo sentido, y en la inversa más que la variación entre tandas (±20 %). El salto del proceso pesa más que la consulta en las baratas: más de lo estimado en «Efecto esperado». Se elige `DylibModule` por defecto; `ProcessModule` queda para quien quiera el aislamiento o el cambio de versión en caliente. Pendiente: que `module.toml`, `make modules` y la imagen de desarrollo usen `dylib`.
3. **Registro, versiones y compatibilidad.** Crate de artefactos firmados; descarga con `[modules]`; etiquetas `contract-<n>`, versión y commit; `[modules.<m>] version` y `registry`; versión del módulo en el manifiesto del bundle y en los sellos de las etapas; comprobaciones de compatibilidad en los tres momentos y sus errores; `geko purge modules`; `geko plan`.
   - Hecho (2026-10-22). `crates/geko-artifact`: la firma (`signature`, con un `Kind` por clase de artefacto, cuyo dominio va en el mensaje firmado: un pack no pasa por módulo) y la descarga (`registry`: `.digest`, copia hecha a mano, copia en disco si el registro falla, `offline`, `purge`), sacadas de `geko-country-pack`, que las envuelve sin cambiar su API ni la firma de los packs publicados.
   - El host (`geko-app/src/store.rs`, `modules.rs`) abre cada módulo de `GEKO_MODULES`, o el enlazado si `[modules.<m>] version` no fija otra versión, o el descargado en `<data>/modules/<m>/<versión>/` desde `[modules.<m>] registry` (por defecto `ghcr.io/candymaps/geko-<m>`). Etiquetas: `contract-<n>-<plataforma>` (el último del contrato) y `<versión>-<plataforma>`. Antes de bajar nada lee las anotaciones del manifiesto (`geko.version`, `geko.contract`, `geko.platform`; `OciClient::annotations`) y rechaza otro contrato u otra plataforma; tras bajarlo, la firma (`Kind` `geko-module`) y el `module.toml` firmado (`name`, `version`, `contract`, `platform`); al abrirlo, el saludo. Una versión ya bajada no se vuelve a pedir (no cambia); la última del contrato, solo si cambió su digest; sin registro u `offline`, la más nueva de las del disco que sirvan.
   - El bundle fija la versión con la que se sirve: en cada carga, si una parte la generó otra versión del módulo, `BundleManager` (con `Reopen`) abre esa, le carga el bundle y la cambia en caliente (`Slot`); las peticiones en curso acaban en la anterior. Un módulo enlazado o de `GEKO_MODULES` sirve tal cual (aviso en el log), y uno `dylib` no cambia de versión sin reiniciar: error, se sigue con el bundle cargado y `/health` lo dice (`bundle_error`).
   - `geko plan` muestra de cada módulo su versión y de dónde sale; `geko purge modules` (y `geko purge`) borra las versiones bajadas que no usan ni los módulos activos ni los bundles conservados.
   - Los sellos de las etapas ya cambian con el código de cada crate (hash de sus fuentes y de `Cargo.lock`, `build.rs` del geocoder), que es más fino que la versión: no se añade.
4. **CI, imagen y Makefile.** `publish` publica el módulo cuando cambia su código, y no publica el host sin módulo para cada contrato. `ci/test.sh` y `pre-push` usan el módulo compilado en local. Pruebas cruzadas: la CI del módulo pasa el golden con la última imagen del host de su contrato; la del host, la conformidad del contrato contra el último módulo de cada contrato que soporta. `Dockerfile` sin módulos: los baja al arrancar. `publish` crea la etiqueta `sdk-<n>` cuando cambia la SDK.
   - Hecho (2026-10-22). `ci/modules.sh`: `build` compila cada carpeta de `modules/` con `module.toml` y completa este (`name`, `version`, `contract`, `platform`); `publish` lo firma con `geko-module-sign` (`geko-artifact`, la clave de los packs) y lo sube a `ghcr.io/candymaps/geko-<m>` con las etiquetas `<versión>-<plataforma>` y `contract-<n>-<plataforma>` y las anotaciones `geko.*`; `check` falla si no hay módulo publicado para el contrato del host.
   - La versión de un módulo publicado es `<versión>-<commit>` (o `<versión>` en una release), compilada en él con `GEKO_MODULE_VERSION` (`module_version!` de la SDK), y la del módulo enlazado se compila igual, de modo que el bundle dice la versión exacta que lo generó.
   - `ci.yml` (`publish`): compila los módulos en `rust:<toolchain>-bookworm`, por la glibc de la imagen; los publica cuando cambia lo que entra en la imagen; no publica imagen, etiqueta ni release sin `ci/modules.sh check`; y mueve `sdk-<contrato>` cuando cambia `crates/geko-module-sdk`.
   - `Dockerfile` y `make image`: geko sin módulos (`--no-default-features`, en `$CARGO_TARGET_DIR/host`); `make start-dev` monta los de `make modules` en `/modules` (`GEKO_MODULES`).
   - Desarrollo en local (2026-10-22): `make start-dev` es `develop/compose.yaml` (proyecto `geko-dev`, un contenedor), con el objetivo `local` del `Dockerfile`, que compila en Docker geko y los módulos activos en la configuración (`DEV_MODULES` los fuerza) y los lleva dentro (`GEKO_MODULES`), con la versión `<versión>-<commit>[-dirty]`. Con `GEKO_MODULES`, un módulo activo que no está ahí ni enlazado es un error, no una descarga. Pendiente para el paso 6: un registro local (`registry:2`) para probar descarga, firma y cambio de versión en caliente.
   - `ci/test.sh` y `pre-push` siguen con el módulo enlazado, que en este repositorio es el compilado en local; `GEKO_MODULES=… ci/test.sh` prueba el de proceso o biblioteca. Las pruebas cruzadas (golden del módulo contra la última imagen del host de su contrato, y conformidad del host contra el último módulo publicado) solo tienen sentido con dos repositorios: pasan al paso 7.
5. **`tiles` al mismo contrato**, como segunda implementación.
   - Hecho (2026-10-22). `crates/geko-module-tiles` pasa a `modules/geko-tiles`, como el geocoder: biblioteca (enlazada en el host con la *feature* `tiles`), binario `geko-tiles` (`ProcessModule`), `cdylib` aparte (`modules/geko-tiles/dylib`, `libgeko_tiles.so`) y `module.toml`, que `ci/modules.sh` publica igual. `make modules` compila los dos módulos. Probado como proceso y como biblioteca (`geko --modules tiles plan`); sigue siendo un esqueleto que responde 503.
   - Hecho (2026-10-22), los cuatro de abajo, en `process.rs` (`Limits`, con sus valores por defecto: espera 1 s doblando hasta 60, 5 caídas en 10 min paran 15 min, 2 caídas en curso ponen la petición en cuarentena 15 min, plazo de 30 s), con un test por cada uno. `Response` gana cabeceras opcionales (`Retry-After`) y `Module` gana `unavailable()`, del lado del host, que `/health` muestra en `modules_unavailable`.
   - **Después, antes del paso 6: caídas de `ProcessModule`.** Si el proceso del módulo muere (aborto, segfault, OOM; un pánico solo responde 500), hoy se relanza bajo el cerrojo de la conexión: todas las peticiones esperan a que el nuevo cargue el bundle (con España, segundos), y una consulta que lo tumba y un cliente repite lo deja casi parado. En `crates/geko-app/src/process.rs`, con tests rápidos:
     1. Relanzar y cargar el bundle en un hilo aparte; mientras, cada petición responde 503 con `Retry-After`, sin esperar en el cerrojo.
     2. Espera creciente entre relanzamientos (1, 2, 4… hasta 60 s); tras N caídas en poco tiempo, queda caído con 503, error en el log y en `/health`, en vez de relanzar sin fin.
     3. Cuarentena de la consulta que lo tumba: huella (ruta, parámetros, hash del cuerpo) de las peticiones en curso al morir; la que estaba en curso en dos caídas se rechaza sin reenviarla durante un tiempo, y su huella va al log.
     4. Plazo por llamada (503 si no responde en X s) y vigilante: si el proceso deja de contestar del todo, el host lo mata y aplica el 2.
6. **Estabilizar `contract-1`** con unas semanas de uso real y su suite de conformidad en el host.
7. **Extraer el repositorio** `candymaps/geko-geocoder` con `git filter-repo` (`modules/geko-geocoder/`, `countries/`, `languages/` y su documentación), con la SDK publicada por el host como dependencia de git con etiqueta (o un registro de Cargo; si es privado, la CI del módulo necesita un token). Con él, las pruebas cruzadas del paso 4.
8. **Documentación** en cada paso: AGENTS.md (la regla de las *features* de Cargo), PROJECT.md §2.3, §2.4, §3, §3.1, §4, §4.1, §12, §14, §18, §19, README, `geko.toml` y `geko-geocoder.toml`.

## Efecto esperado en lo que se mide

Acierto, duplicados, calibración, datos y tamaño del bundle: iguales (el mismo código genera y busca). Lo demás:

| | `StaticModule` | `DylibModule` | `ProcessModule` |
|---|---|---|---|
| Latencia | Igual | Casi igual: copiar la petición y la respuesta, en microsegundos | Un salto por socket Unix y una copia más de la respuesta: unos 10–30 µs por petición, a medir. Del 5 al 20 % en las medianas de 0,1–0,5 ms y en las respuestas de la caché; nada en el p99 (5 y 15 ms de presupuesto) |
| Caudal (16 a la vez) | Igual | Casi igual | Menor: el host gasta CPU en reenviar. Unos puntos en búsqueda y autocompletado; más en la inversa (67.000 QPS, la más barata) |
| Instrucciones (valgrind) | Igual | Algo más en la frontera | Más, por el reenvío; valgrind debe seguir al hijo. Sube una vez frente al commit anterior y se acepta |
| Memoria al servir | Igual | Igual | Unos MB más por el segundo proceso. El índice mapeado no se duplica (la caché de páginas es una) |
| Memoria al generar | Igual | Igual | Mejor en `geko up`: el pico del build (2,9 GB) se devuelve al acabar el proceso, en vez de quedarse en el asignador del que sirve |
| Arranque | Igual | Igual, más la primera descarga del módulo | Igual, más la primera descarga y unos ms de saludo |
| Disco | Igual | Unas decenas de MB por versión del módulo conservada | Igual que `DylibModule` |
| Robustez | Igual | Un fallo del módulo (UB, aborto) tira el host | Mejor: un fallo del módulo no tira el host, y el host lo relanza |
| Cambio de versión en caliente | — | No: hay que reiniciar | Sí, en la recarga del bundle |

Otros efectos, iguales en los tres:

- **Compilación:** el host compila mucho más rápido sin el geocoder. El LTO deja de cruzar entre host y módulo, sin coste esperado: los bucles calientes están dentro del módulo.
- **Asignador:** el módulo debe declarar mimalloc en su propio binario (o en su `.so`). Si no, pierde lo que mimalloc ganó.
- **Quitar los modos Pelias y Photon** no cambia el rendimiento de la API propia: menos código y menos comprobaciones en la CI.

La medición del paso 2 confirma o corrige estas estimaciones. Si `ProcessModule` pierde más que el ruido en las medianas o en el caudal, se usa `DylibModule`.

## Riesgos

- Latencia del salto del proceso (paso 2). Cruza una vez por petición, como pide §2.2.
- Cambios que tocan los dos lados, una vez separados los repositorios: publicar la SDK, subirla en el módulo, publicar el módulo, subir el contrato en el host. Se mitiga con un contrato pequeño y aditivo, y extrayendo el repositorio solo tras estabilizarlo.
- Pruebas repartidas entre dos repositorios: el `pre-push` (35–50 min) también.
- Ejecutar código descargado: firma obligatoria.
- Más piezas que publicar: imagen, módulos y packs, cada uno con su versión.

## Pendiente de decidir

- Un proceso de reserva del módulo ya cargado, que cubra al instante la caída del activo (ver «caídas de `ProcessModule`», tras el paso 5). Solo duplica la parte caliente del índice en RAM (la fría va mapeada y comparte la caché de páginas); se decide midiendo en el paso 6 su memoria con España y cuánto tarda en recargarse sin él.
- ⏸️ Aplazado (2026-10-21): si `tiles` necesita datos por país (sus propios paquetes o los del geocoder). Ahora solo se prepara el terreno para su módulo; se decide al crearlo.

Decidido (2026-10-21): la imagen del host no lleva módulos y baja los activos al arrancar; la SDK se publica en el repositorio del host; los idiomas y los packs se van con el geocoder (`tiles` no traduce); los idiomas siguen compilados dentro del binario del geocoder, no como paquete de datos; la SDK se usa como dependencia de git con etiqueta `sdk-<n>`, no desde un registro de Cargo.
