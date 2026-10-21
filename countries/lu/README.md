# Pack de Luxemburgo (`lu`)

Todo lo propio de Luxemburgo (PROJECT.md §2.4). El motor y los providers de `providers/` son genéricos; este pack aporta lo que solo tiene sentido aquí. Es el segundo país del golden: pequeño, solo con fuentes genéricas y con otras lenguas, para comprobar en cada cambio que el motor no depende de España.

| Contenido | Qué es |
|---|---|
| `pack.toml`, `data/` | Datos de texto, leídos por el normalizador y el analizador genéricos (`providers/text-rules`): lenguas y formato de CP, con la «L-» delante (`pack.toml`), niveles administrativos de OSM (`data/admin-levels.toml`), 15 tipos de vía en luxemburgués, francés y alemán (`data/street-types.toml`), conectores, marcas de número, palabras de piso y puerta, y sufijos de portal (`data/words.toml`), y las equivalencias entre lenguas que no se aprenden de OSM (`data/equivalences.txt`) |
| `src/`, `tests/` | Crate `country-pack-lu`, solo para los tests: lee el pack del repositorio; sus tests comprueban los niveles administrativos y el análisis de direcciones de Luxemburgo («12, rue de la Gare», «L-1930») |
| `golden/` | Consultas de referencia del país entero (`generated`, `holdout`, `reverse`; la lista, en `golden/sets`) e instantánea de los datos (`golden/snapshot`) |
| `golden/baseline/` | Línea base que compara `ci/test.sh --scope lu` |

- **Lenguas:** luxemburgués, francés y alemán.
- **Particiones:** el país entero en una sola.
- **Niveles administrativos:** municipio (`admin_level` 8) y localidad o barrio (9 y 10). Ni región ni provincia: los cantones y los distritos no se usan para buscar una dirección.

## Fuentes

Con `country = "LU"` en el ámbito, Luxemburgo usa siempre **OSM + Overture**, sin escribir nada más en `geko.toml`:

- **OSM:** es la base de todos los países y no se puede desactivar (README del proyecto). Da las calles, los portales (unos 166.000), los lugares, los POIs y los CP.
- **Overture Maps:** POIs de todos los países, opcional con `[sources.overture]` (README del proyecto). El golden la deja fuera (`GEKO_GOLDEN_DISABLE`), porque su instantánea solo congela OSM.

Luxemburgo no tiene todavía ninguna fuente nacional. Prevista: las direcciones oficiales de la Administration du cadastre et de la topographie, publicadas en data.public.lu.

Otro país solo tiene que escribir sus propios `pack.toml` y `data/*.toml`: el motor no cambia (guía en [countries/README.md](../README.md)).
