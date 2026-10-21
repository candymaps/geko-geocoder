# Packs de país

Todo lo propio de un país vive en su pack, `countries/<país>/` (PROJECT.md §2.4): sus datos de texto, sus regiones, lo que necesita saber de él cada fuente y sus consultas de referencia. El motor y los providers de `providers/` son genéricos y no se tocan al añadir un país.

Un pack es **datos, no código**: `geko` lo lee al arrancar de la carpeta de los packs (`/var/lib/geko/packs/<país>/`, o la de `GEKO_PACKS`), y solo los de los países del ámbito. `ci/packs.sh` copia ahí lo que se lee (`pack.toml`, `data/`, `sources/*.toml`); `make install` lo hace solo. Las comprobaciones (`ci/test.sh`) leen los packs tal como están en el repositorio.

Los packs se publican firmados en `ghcr.io/candymaps/geko/countries/<país>` (`make release-packs`, o el trabajo `publish` de `ci.yml` en cada cambio de un pack en `main`), y `geko` baja los del ámbito al generar. Un pack nuevo se publica igual: el workflow lo encuentra por su `pack.toml`. La primera vez, su paquete de ghcr.io nace privado: hay que hacerlo público en GitHub («Package settings»), o nadie más podrá bajarlo.

| Pack | Lenguas | Fuentes | Particiones | Golden |
|---|---|---|---|---|
| [`es`](es/README.md) | es, ca, gl, eu | OSM + CartoCiudad + Overture | Provincias | Baleares (`ES-PM`) y España completa (`spain-*`) |
| [`lu`](lu/README.md) | lb, fr, de | OSM + Overture | El país entero | Luxemburgo entero |

Luxemburgo se añadió (2026-10-17) como segundo país del golden: un país pequeño, solo con fuentes genéricas y con otras lenguas, otra estructura administrativa y otra forma de escribir las direcciones. Así se comprueba en cada cambio que el motor no depende de España. Esta guía recoge lo que hizo falta, incluidos los tropiezos, para que el siguiente país salga a la primera.

## Pasos

Ejemplo con Luxemburgo (`lu`, `LU`). Sustituye el código por el del país nuevo (ISO 3166-1, en minúsculas para las carpetas y los crates, y en mayúsculas en los datos).

### 1. Los ficheros del pack

```
countries/lu/
├── pack.toml             # país, versión, lenguas y formato del CP
├── data/
│   ├── street-types.toml # tipos de vía en cada lengua
│   ├── words.toml        # conectores, marcas de número, pisos y puertas, «sin número», sufijos
│   ├── equivalences.txt  # pares de palabras entre lenguas (puede ir solo con comentarios)
│   ├── admin-levels.toml # el admin_level de OSM de cada tipo de unidad administrativa
│   ├── regions.toml      # opcional: particiones y regiones que las agrupan
│   └── region-boxes.tsv  # opcional, generado: una caja que contiene cada partición
├── sources/              # opcional: lo que una fuente necesita saber del país (`osm.toml`…)
├── Cargo.toml            # crate country-pack-lu, solo para los tests
├── src/lib.rs            # pack(), normalizer() y parser() del pack del repositorio, para los tests
├── tests/                # pack.rs (niveles), parser.rs (direcciones del país)
└── golden/               # consultas de referencia, línea base e instantánea de los datos
```

- El crate `country-pack-lu` nunca entra en el binario: solo sirve a los tests del pack y del motor. `src/lib.rs` es una copia del de `countries/lu/src/lib.rs` cambiando el código del país.
- Añade el crate a `[workspace.dependencies]` del `Cargo.toml` raíz: `country-pack-lu = { path = "countries/lu" }`. `members` ya incluye `countries/*`.

### 2. Los datos de texto

- **`pack.toml`**: `country`, `version`, `languages` (ISO 639-1, en el orden en que se prefieren) y `[postcode]`:
  - `version`: entero, desde 1. Súbelo a mano en cada cambio del pack (`pack.toml`, `data/` o `sources/`). Se publica como la etiqueta `pack-<n>`, y `geko plan`, `geko build` y `build_report.json` lo muestran. La publicación se niega (y no sube ningún pack) si un pack cambió con una versión ya publicada.
  - `digits`: cifras del CP.
  - `prefixes`: primeras cifras válidas, si las hay (en España, la provincia). Vacío acepta cualquiera.
  - `country_prefixes`: letras con las que se escribe el CP delante, en mayúsculas y de la más larga a la más corta. En Luxemburgo, `["L-", "L"]` para «L-1930». Sin esto, «L-1930» no se reconoce como CP.
- **`data/admin-levels.toml`**: los cinco campos (`country`, `region`, `province`, `municipality`, `neighbourhood`) son **obligatorios**, aunque el país no tenga ese nivel: se ponen vacíos (`region = []`). Los niveles salen de la [tabla de OSM por país](https://wiki.openstreetmap.org/wiki/Tag:boundary=administrative#Country_specific_values). Deja fuera los que nadie usa para buscar una dirección (comarcas en España, cantones en Luxemburgo).
- **`data/street-types.toml`**: un `[[types]]` por tipo de vía, con un `code` común a todas las lenguas, todas sus formas y abreviaturas en `words` (sin tildes y en minúsculas), una palabra por lengua en `display`, `generic = true` si nunca es un nombre por sí solo y `area = true` si nombra una zona (plaza, parque, polígono). Los tipos que van detrás del nombre («Garer Strooss», en luxemburgués) no se reconocen como tipo: no pasa nada, el nombre se indexa entero.
- **`data/words.toml`**: `connectors` (artículos y preposiciones que se ignoran al comparar nombres), `number_markers` («nº», «no»), `unit_words` (piso, puerta, «étage», «Stock»), `unnumbered` («s/n») y `housenumber_suffixes` («BIS», «TER»).
- **`data/equivalences.txt`**: casi todas las equivalencias entre lenguas se aprenden solas de los nombres `name:*` de OSM. El fichero solo añade o descarta pares; puede quedar con solo los comentarios.

### 3. Regiones y fuentes

- **`data/regions.toml`** (opcional): las regiones del país, con su código ISO 3166-2, su nombre, la región que las contiene (`parent`) y los atributos que pida alguna fuente (`attributes`, que heredan las regiones que contiene). Las que no contienen ninguna otra son las **particiones**: el país se genera por ellas, una a una, y el ámbito puede nombrar una partición o una región que las agrupe. Sin el fichero, el país se genera entero, en una partición. España lista sus provincias, con su `postcode_prefix`, y las comunidades autónomas; Luxemburgo no lo necesita.
- **`data/region-boxes.tsv`** (opcional, generado): una caja que contiene cada partición, de su límite administrativo y con 1 km de margen. Con ella, una fuente que descarga por zonas (Overture) baja solo lo que tocan las particiones del ámbito: para Baleares, 11 MB en vez de los 379 de España. Se genera tras un `geko build` del país entero con `geko-eval region-boxes --canonical <datos>/canonical/<PAÍS> --pack countries/<país>`, que exige un límite con el código ISO 3166-2 de cada partición. Sin el fichero, se usa la caja del país. Los límites casi no cambian: basta con volver a generarlo si cambia `regions.toml`.
- **`sources/<fuente>.toml`** (opcional): lo que una fuente genérica necesita saber del país o, con `kind`, una fuente nacional. Una fuente nacional se declara para el provider genérico de su tipo, sin código: `kind = "ogc-api-features"` para un servicio OGC API Features (`countries/es/sources/cartociudad.toml` es el ejemplo completo: servicio, licencia, recorrido de cada partición y columnas del staging, con las del esquema común de direcciones). El `[sources.<fuente>]` del `geko.toml` cambia cualquier campo de la declaración o la desactiva. Si una fuente no cabe en ningún tipo, se añade el tipo al motor, en `providers/`, para que sirva a cualquier país.
- Para una fuente genérica, por ejemplo, `sources/osm.toml` da los extractos de Geofabrik cuando sus regiones no llevan código ISO en el índice (España) o el país se reparte entre extractos (Canarias está en África): `country = [...]` y `[regions]` con el extracto de cada región. Un país cuyo índice ya trae los códigos no lo necesita.
- Nada de esto se registra en el motor: `geko` encuentra el pack por el código del país del ámbito.

### 4. Probar el pack

```bash
cargo test -p country-pack-lu
```

Escribe en `tests/parser.rs` direcciones tal como se escriben en el país. En Luxemburgo eso destapó que el analizador no entendía el número delante y separado por una coma («12, rue de la Gare»); se arregló en el analizador genérico (`providers/text-rules`), no en el pack, porque así se escribe en media Europa y en América. Comprueba siempre que los tests de los demás packs siguen pasando y que el golden de Baleares no pierde ninguna consulta.

### 5. Generar y comprobar los datos

```bash
# Configuración del país, solo con OSM (o con sus fuentes nacionales, si las tiene)
mkdir -p /var/lib/geko/golden/lu
cat > /var/lib/geko/golden/lu/geko.toml <<EOF
[modules]
geocoder = true
tiles    = false

[[scope]]
country = "LU"

[data]
accept_licenses = true
EOF
GEKO_CONFIG=/var/lib/geko/golden/lu/geko.toml GEKO_DATA=/var/lib/geko/golden/lu geko build
```

OSM encuentra solo el extracto del país en el índice de Geofabrik. Revisa la salida: cifras razonables de calles, portales, lugares y CP, y la tasa de duplicados.

### 6. Las consultas de referencia

```bash
E=geko-eval; C=/var/lib/geko/golden/lu/canonical/LU; G=countries/lu/golden
$E generate --canonical $C --seed 42 --out $G/generated.jsonl
$E generate --canonical $C --seed 7  --out $G/holdout.jsonl
$E reverse  --canonical $C --out $G/reverse.jsonl
$E english  --canonical $C --words $G/english.toml --out $G/english.jsonl
printf '# Query sets that the golden of ci/test.sh runs against the baseline.\ngenerated holdout reverse english\n' > $G/sets
```

- `golden/english.toml` da la palabra inglesa de los tipos de vía del pack (`RUE = "Street"`), para las consultas en inglés. Los tipos que no la tienen se quedan fuera de ellas.

- `geko-eval generate` toma las reglas del pack del país del canónico (`canonical/<país>`): añade el país a la función `normalizer` de `crates/geko-eval/src/golden.rs`. Escribe cada dirección con el número detrás (`address_full`) y delante (`address_number_first`), y con el tipo de vía en otra lengua del país (`address_type_variant`).
- `golden/sets` lista los conjuntos que comprueba el golden (`ci/test.sh --scope lu`), además de las pulsaciones de `generated`. Sin él, el golden no sabe qué ejecutar.
- `golden/snapshot-max-age-days` (opcional, 60 si falta): los días tras los que el golden avisa de que conviene renovar la instantánea, según lo deprisa que cambian las fuentes del país.

### 7. Congelar los datos y medir la línea base

```bash
ci/golden-snapshot.sh create --scope lu                    # instantánea de raw/ y golden/snapshot
GEKO_GOLDEN_UPDATE=1 ci/test.sh --scope lu                # línea base en golden/baseline/
ci/test.sh --scope lu                                     # debe pasar sin cambios
```

- `--scope lu` acepta un país nuevo en cuanto tiene `countries/lu/golden/`, y la CI lo pasa sola: el golden corre para cada pack con `golden/`.

- Fuera de España, el golden genera el país entero (sin `GEKO_GOLDEN_REGIONS`) y usa `/var/lib/geko/golden/<país>` como carpeta de datos (la de Baleares es `golden/es`).
- La primera vez, la línea base incluye `data.json` (recuentos e integridad del canónico), `build.json` (tamaño del bundle) y `keystrokes.json` (pulsaciones). Las instrucciones no tienen línea base: se comparan con las del último commit subido, contadas en la misma pasada.
- La segunda pasada debe salir idéntica: si no, algo no es determinista.
- Publica el archivo de la instantánea (`/var/lib/geko/golden/lu/snapshots/*.tar.gz`) donde apunta `GEKO_GOLDEN_SNAPSHOT_URL`.

### 8. La CI

No hace falta tocarla: el trabajo `golden` de `.github/workflows/ci.yml` pasa el de cada pack con `golden/` cuando cambia el motor, y el de un pack cuando cambia ese pack. Antes del primer push a `main` con el país, publica su instantánea (`ci/golden-snapshot.sh publish --scope <país>`): sin ella, su golden falla.

### 9. La documentación

- Una fila en la tabla del principio de este README, con el enlace al README del pack.
- El `README.md` del pack, **siempre**, tenga o no fuentes propias, con el formato de abajo.
- Si hubo que tocar el motor, su punto en `docs/PROJECT.md`, sección 17 «Pendiente».

#### El README del pack

Todos los packs tienen el mismo README, con estas partes y en este orden ([`es`](es/README.md) y [`lu`](lu/README.md) son el modelo):

1. **Título:** `# Pack de <país> (`<código>`)`.
2. **Entrada:** «Todo lo propio de <país> (PROJECT.md §2.4). El motor y los providers de `providers/` son genéricos; este pack aporta lo que solo tiene sentido aquí.», y una frase más si el pack tiene un papel especial (por ejemplo, ser país del golden).
3. **Tabla `Contenido | Qué es`:** una fila por carpeta o grupo de ficheros del pack (`pack.toml` y `data/`, `src/` y `tests/`, cada fuente en `sources/<fuente>.toml`, `golden/`, `golden/baseline/`), diciendo qué datos trae cada uno.
4. **Lista de rasgos del país:** como mínimo, `**Lenguas:**`; además, las particiones y los niveles administrativos cuando no son los habituales.
5. **`## Fuentes`:** «Con `country = "<CÓDIGO>"` en el ámbito, <país> usa siempre **<fuentes>**, sin escribir nada más en `geko.toml`:», y una viñeta por fuente que se usa por defecto: OSM, cada fuente genérica (Overture…) y cada fuente nacional. Cada fuente nacional lleva su tabla `Campo | Por defecto | Qué hace` con todos los campos de su `[sources.<id>]`, `enabled` incluido, y un ejemplo en TOML. Después, las fuentes nacionales previstas o «no tiene todavía ninguna fuente nacional».
6. **Cierre:** dónde está lo pendiente del pack (su `docs/BACKLOG_<PAÍS>.md`, si lo tiene) y el enlace a esta guía.

Al añadir o quitar una fuente, un nivel o un conjunto del golden, actualiza el README del pack y la fila de la tabla de arriba en el mismo cambio.

## Lo que salió al añadir Luxemburgo

| Problema | Arreglo |
|---|---|
| `admin-levels.toml` sin `region` ni `province`: el pack no carga («missing field `region`») | Poner los cinco campos, vacíos si el país no tiene ese nivel |
| «L-1930» no se reconocía como CP | Campo `country_prefixes` en `[postcode]` (genérico, en `text-rules`) |
| «12, rue de la Gare» daba una calle vacía | El analizador genérico toma como calle la parte siguiente cuando el número va solo antes de la coma |
| `geko-eval generate` generaba una consulta `address_spanish_type` y no probaba el número delante | Generador genérico, con el pack del país (BACKLOG 30) |
| La línea base no se escribía la primera vez | `ci/test.sh` crea ya la carpeta `golden/baseline/` |
| El golden no sabía qué conjuntos ejecutar ni qué regiones generar | `golden/sets` en el pack, y el país entero por defecto fuera de España |
| `confidence` peor calibrada que en España (ECE 0,197 frente a 0,136) | Se ajustó solo con España: BACKLOG 16b |
| Añadir el crate cambia `Cargo.lock`, y eso rehacía todas las etapas de cualquier país ya generado | Cada etapa cuenta ya solo las entradas del lock que usa (BACKLOG 31) |
