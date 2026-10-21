# Resultados de las mediciones

## Cambios medidos el 2026-10-18

Sobre la medida del 2026-10-17 (abajo), con `ci/check.sh spain`, los golden de Baleares y de Luxemburgo y `ci/check.sh pelias`:

| Qué | Antes | Ahora | Punto |
|---|---|---|---|
| Inversa con la capa `street`: la calle del portal, primera | España 70 %, Baleares 69 %, Luxemburgo 94 % | 98 %, 100 %, 100 % | BACKLOG 33 |
| Respuestas con un resultado duplicado | Baleares 7, Luxemburgo 104 | 0 y 0 (red de seguridad de §6.5.6; sin ella, Luxemburgo 24, por la fusión de las localidades en su municipio) | 29 |
| Pulsaciones de España, top-1 / top-5 | 28,0 % / 36,7 % | 32,2 % / 38,2 %, sin empeorar ninguna categoría | 29 (`ltok`) y la localidad a medio teclear |
| Error de calibración del primer resultado (ECE) | Baleares 0,135, Luxemburgo 0,173, España 0,160 | 0,042, 0,083 y 0,079 | 16b |
| Etapa de la inversa de España | 92 s y 5,6–6,0 GB de pico | 9,8 s y 1,7 GB, salida idéntica | 32 |
| Cambiar una dependencia | Rehacía todas las etapas de España (30 min) | Solo las de los crates que la usan | 31 |
| Bundle de España | 2.598 MB | 2.666 MB: calle de cada portal (64 MB) y nombres por lengua para `lang` (4 MB); parte caliente igual, 565 MB | 33, 15 |

**Frente a Pelias** (Baleares, entidades que Pelias también tiene, repetido el 2026-10-18): top-1 96,3 % frente a 61,2 % y 96,8 % frente a 63,6 %; mejor en todas las categorías de los dos conjuntos. La más ajustada sigue siendo «dirección sin municipio», 89 frente a 88 % y 85 frente a 84 %.

**Latencia:** los objetivos no cambian respecto al 2026-10-17. En esta máquina, las pruebas de carga con 16 a la vez han variado hasta un 40 % entre tandas con el mismo binario (inversa: 18.300, 24.700 y 31.200 QPS), así que solo valen comparadas A/B en la misma tanda (AGENTS.md).

**Nuevo en el servicio:** página de demostración en `/`, `geko up`, `lang`, CORS, claves de API y límite de peticiones por cliente (apagados por defecto).

## Medida del 2026-10-17 (`ci/check.sh spain`)

Desde el 2026-10-17, estas cifras las mide `ci/check.sh spain` y quedan como línea base junto a los datos (`/var/lib/geko/check-baseline/`). Cada cambio posterior se compara con ellas (BACKLOG_CI.md). Bundle de España completa `v2026-10-17T111346`, generado con todas las etapas forzadas en el equipo de 12 núcleos (WSL Debian, cliente y servidor en la misma máquina). Las latencias son las que ve el cliente, que incluyen HTTP.

**Objetivos de PROJECT.md §1.1:**

| Métrica | Objetivo | 2026-10-17 | ¿Cumplido? |
|---|---|---|---|
| Búsqueda, p99 | < 15 ms | 4,7 ms (1 a la vez) · 6,3 ms (4) · 14,1 ms (16) | ✅ |
| Autocompletado, p99 | < 5 ms | 5,6 ms (1) · 6,9 ms (4) · 15,0 ms (16) | ❌ Lo fija el presupuesto: decisión pendiente (abajo) |
| Inversa, p99 | < 5 ms | 0,35 ms (1) · 0,48 ms (4) · 1,45 ms (16, 34.874 QPS) | ✅ |
| Caudal de autocompletado manteniendo su p99 | ≥ 2.000 QPS | 2.217 QPS con 4 a la vez, pero con p99 de 6,9 ms | ⚠️ |
| Parte caliente del segmento de España | ≤ 4 GB | 565 MB (bundle entero: 2,6 GB) | ✅ |
| Calidad frente a Pelias (Baleares, comparación justa) | ≥ +10 puntos y nunca peor | 96,3 % frente a 61,2 % y 96,8 % frente a 63,6 % en top-1; mejor en todas las categorías | ✅ |

**Acierto en España completa** (top-1 / top-5): generadas 89,7 % / 94,3 % (el 2026-10-16, 86,6 %); validación 90,2 % / 94,2 %; con foco 94,0 % / 95,5 %; 35.397 pulsaciones de autocompletado 28,0 % / 36,5 %. Con 16 autocompletados a la vez cargando el servidor, las generadas aciertan lo mismo que en reposo (89,7 %), con p99 de 13,3 ms.

**Inversa** (nueva, 300 puntos a 2 m de portales reales): el portal sale primero en el 91 % de los casos y el municipio es correcto en el 100 %. La calle del portal sale primera solo en el 70 %: el portal de CartoCiudad suele estar a decenas de metros del eje de su calle, y otra calle queda más cerca.

**Generación** (todas las etapas, segundos / pico de memoria): staging 420,6 s / 1,4 GB; reparto 2,2 s / 1,1 GB; fusión 155,9 s / 2,4 GB; índice 75,5 s / 2,9 GB; **inversa 90,4 s / 5,6 GB**; publicación 2,1 s. El pico del build ya no es el índice sino el índice inverso.

**Servidor:** sirve el bundle a los 0,8 s de arrancar; la primera consulta tarda 8,3 ms; tras la carga ocupa 1,85 GB residentes.

**Datos:** ningún id público repetido, ningún portal sin calle, ningún CP ni municipio inexistente. Sin municipio quedan 203 portales, 111 POIs y 113 calles, en el mar o en puertos, fuera de todo límite. Hay 84 pares de calles duplicadas (0,036 %).

## Medida del 2026-10-16

Estado medido el 2026-10-16, tras el commit `6562074` (presupuesto de tiempo por consulta), con el bundle de España completa `v2026-10-15T221559`.

**Condiciones de la medida.** Máquina de 16 núcleos con WSL Debian, cliente y servidor en la misma máquina, binario `--release`. Los objetivos de PROJECT.md §1.1 se definen para un servidor Linux de 4 vCPU y 8 GB, con el cliente en otra máquina (§13). Por tanto, las latencias son orientativas hasta repetirlas en esas condiciones. Además, el rendimiento de la máquina varía entre tandas: los cambios se miden A/B en la misma tanda.

Herramientas:
- `geko-eval load`: carga con las consultas de referencia de España.
- `geko-eval keystrokes`: 17.568 pulsaciones reales de autocompletado.
- `geko-eval check`: acierto y latencia por categoría.
- `ci/check.sh golden`: consultas de referencia de Baleares, con tolerancia 0.

### Objetivos medibles (PROJECT.md §1.1)

| Métrica | Objetivo | Al empezar el punto 8 | Ahora (España completa, 2026-10-16, equipo de 12 núcleos) | ¿Cumplido? |
|---|---|---|---|---|
| Búsqueda, p99 en servidor | < 15 ms | 47 ms (1 consulta a la vez) · 139 ms (16 a la vez) | **4,0 ms** (1 a la vez) · 13,9 ms (16 a la vez, medido en el cliente) | ✅ En las dos, en este equipo |
| Autocompletado, p99 en servidor | < 5 ms | 34 ms (pulsaciones reales) | **5,6 ms** (1 a la vez) · 6,8 ms (4 a la vez) · 16,5 ms (16 a la vez), los dos últimos medidos en el cliente | ❌ Casi: falta 0,6 ms |
| Inversa, p99 en servidor | < 5 ms | 21 ms con 48 búsquedas a la vez | **0,33 ms** en reposo · 0,41 ms (4 a la vez) · 1,5 ms (16 a la vez, 29.752 QPS), medidos en el cliente | ✅ Con mucho margen |
| Caudal de autocompletado manteniendo su p99 | ≥ 2.000 QPS | — | 2.591 QPS con 4 a la vez, pero con un p99 de 6,8 ms | ⚠️ El caudal sí; el p99 < 5 ms, no |
| RAM de la parte caliente del segmento de España | ≤ 4 GB | — | **539 MB** (el servicio entero ocupa 932 MB) | ✅ Con mucho margen |
| Calidad frente a Pelias | ≥ Pelias + 10 puntos, y nunca peor en ninguna categoría | Baleares, fase 1: 95,9 % frente a 63,7 % (comparación justa) | Baleares: 95,9–96,3 % frente a 61,0–63,7 % (comparación justa) | ✅ +32,6 puntos, y mejor en todas las categorías |

Caudal de la búsqueda con 16 consultas a la vez: 7.075 QPS (venía de 1.142 QPS). Las cifras son orientativas hasta repetirlas en el hardware de referencia (punto 5 de abajo).

### Calidad (top-1, consultas de referencia)

| Categoría | España: generadas / validación | Baleares |
|---|---|---|
| Total | 86,6 % / 85,7 % | 94,5 % / 94,1 % |
| Dirección completa | 96 % / 97 % (venía del 87 %) | — |
| Dirección con tipo de vía en castellano | 99 % / 98 % (venía del 89 %) | — |
| Autocompletado de municipio | 50,8 % / 43,2 % (venía del 40–44 %) | — |
| Con foco | 94 % | — |
| Otras categorías de Baleares | — | 90,0 % y 98,0 % |

Ninguna categoría ha empeorado con los cambios de la sesión. El golden de Baleares pasa con tolerancia 0.

Fallos conocidos (BACKLOG.md, punto 12b):
- **Autocompletado de municipio:** muchos "Sant…" y "San…" de todo el país compiten entre sí.
- **Direcciones rurales** (Partida, Barrio, Lugar): el resultado queda a entre 57 y 537 m del esperado.

### Generación y datos de España

| Concepto | Resultado |
|---|---|
| Descarga | 36 min: OSM 2 min y CartoCiudad 34 min |
| Build | 6,6 min sin Overture (2026-10-16); 16 min con Overture (2026-10-20: staging 7,5 min, fusión 3,7, índice 4, inversa 0,5). Pico de RAM del builder del índice: **2,9 GB** (era 8,1 GB; 6g hecho) |
| Volumen | 16,1 M de portales y 1,27 M de calles |
| Segmento | 1,8 GB: 539 MB de parte caliente y 1,3 GB de parte fría; la inversa ocupa 687 MB mapeados |
| Arranque del servicio | 0,97 s |
| Generación incremental | ✅ Cada etapa se salta si no cambian ni su código ni sus entradas |

### Qué ha bajado la latencia (punto 8)

| Cambio | Efecto |
|---|---|
| Tipo de vía con letras intercambiadas, reconocido al analizar | Menos búsquedas de respaldo |
| Una errata no se expande a palabras presentes en más del 2 % del índice | Recorta la cola de la búsqueda |
| Erratas y relajación solo mientras no haya una respuesta completa | Búsqueda p99: 47 → 19 ms (1 a la vez) |
| Un prefijo de 1–2 letras junto a otras palabras se comprueba en los nombres | Autocompletado p99: 54 → 18 ms |
| Pool de CPU acotado a una búsqueda por núcleo | Inversa bajo carga: p50 de 7,2 a 1,7 ms y p99 de 21 a 6,7 ms |
| Prefijos de expansión enorme (más del 0,5 % del índice) comprobados en los nombres | Pulsaciones: menos cola con las palabras más comunes |
| Autómata de erratas `levenshtein_automata` en vez del de `fst` | Antes costaba unos 30 ms por palabra larga; pulsaciones p99: 34 → 8,0 ms |
| Presupuesto de tiempo: `[serve] search_budget_ms = 15`, `autocomplete_budget_ms = 5` | Búsqueda p99: 11,7 → 9,1 ms; pulsaciones p99: 8,0 → 7,5 ms |

## Qué falta para cumplir todos los objetivos

1. ~~**Repetir la comparación con Pelias.**~~ Hecho el 2026-10-16 (abajo).
2. **Bajar el autocompletado por debajo de 5 ms.** El 2026-10-16 bajó de 8,7 a 5,6 ms (p99 en el servidor, en otro equipo; BACKLOG 8), y el 2026-10-17 sigue en 5,6 ms (en el cliente). El resto lo fija el presupuesto de tiempo, no el coste de las consultas: las caras se cortan a los 5 ms. Bajar de 5 ms exige un presupuesto menor, que cuesta unos 0,7 puntos de top-5 en las pulsaciones por cada milisegundo (AGENTS.md, «Optimizations»). **Decisión pendiente (de producto):** qué presupuesto se fija, y comprobarlo en el hardware de referencia (punto 5).
3. **Mantener los p99 con 16 consultas a la vez.** La búsqueda ya lo cumple (14,1 ms el 2026-10-17); el autocompletado, no (15,0 ms). Con el servidor fijado a 4 núcleos, el p99 del autocompletado con 4 a la vez (unos 2.000–2.500 QPS) queda en 7–9 ms: el tiempo se va en la cola de CPU. **Pendiente** del mismo presupuesto y del hardware de referencia.
4. ~~**Medir la inversa en reposo.**~~ Hecho el 2026-10-16: p99 de 0,33 ms.
5. **Repetir las medidas en el hardware de referencia**, con el cliente en otra máquina. **Pendiente del hardware.** Ya está automatizado: `GEKO_REFERENCE_MACHINE=1 ci/check.sh spain` comprueba allí todos los objetivos de §1.1.
6. ~~**Bajar los 8,1 GB del build** (punto 6g).~~ Hecho: 2,9 GB en el índice. El 2026-10-17, la medida por etapas mostró que el pico del build está ahora en el índice inverso: 5,6 GB.

## Cómo seguir

### 1. Comparación con Pelias

Es el objetivo principal del proyecto, y no se ha medido desde la fase 1. Además nos dirá dónde mejorar: si en alguna categoría estamos por debajo de Pelias, eso pasa por delante de cualquier optimización.

- **Pelias:** una instancia de referencia, con OSM de Baleares (PROJECT.md §13.2). No se monta en local.
- **Solo Baleares:** esa instancia tiene calles y portales solo de Baleares, así que se usan las consultas de referencia de Baleares (`countries/es/golden/`), no las `spain-*.jsonl`.
- **Mismas pruebas para los dos:** comparación justa (`*-osm.jsonl`) y de producto, top-1 y top-5 por categoría, con `geko-eval run` y `geko-eval compare`. Con pausa entre peticiones, porque es un servicio compartido.
- **Latencia:** la de Pelias incluye la red y no es comparable. Los p99 de geko se miden aparte contra los objetivos de §1.1.

**Resultado (2026-10-16, commit `0d1a23a`).** Baleares regenerada con el código actual. Pelias, con 50 ms de pausa entre peticiones y sin errores. Top-1 / top-5:

| Conjunto | geko | Pelias |
|---|---|---|
| Semilla 42, comparación justa | **95,9 % / 98,8 %** | 61,0 % / 65,5 % |
| Semilla 7, comparación justa | **96,3 % / 99,5 %** | 63,7 % / 67,4 % |
| Semilla 42, producto completo | **94,5 % / 97,9 %** | 37,8 % / 42,3 % |
| Semilla 7, producto completo | **94,1 % / 98,4 %** | 38,2 % / 41,4 % |
| Escritas a mano | **90,0 % / 93,3 %** | 70,0 % / 76,7 % |
| Sin municipio, con foco | **98,0 % / 99,0 %** | 41,0 % / 46,5 % |

geko gana en top-1 en todas las categorías de todos los conjuntos. Las más justas son dirección sin municipio (89 % frente a 88 % y 85 % frente a 84 %) y dirección completa en el conjunto de la semilla 7 (99 % frente a 95 %). Pelias ya no gana en ninguna, y el objetivo de §1.1 (≥ Pelias + 10 puntos) se cumple con +32,6 puntos. El golden de Baleares pasa sin regresiones.

Como Pelias solo tiene calles de Baleares, el resto de España no se puede comparar con esta instancia.

**Repetida el 2026-10-17** con `ci/check.sh pelias` (200 ms de pausa), que desde entonces la repite cada semana en la CI. En la comparación justa, geko acierta en top-1 el 96,3 % y el 96,8 %, frente al 61,2 % y el 63,6 % de Pelias, y gana en todas las categorías.

### 2. Después, la optimización genérica, por orden

| Orden | Punto | Por qué |
|---|---|---|
| 1 | ~~**6g:** memoria del builder del índice~~ | Hecho (2026-10-16): de 8,1 a 2,9 GB con España, segmento idéntico. |
| 2 | **Autocompletado:** de 5,6 a menos de 5 ms (venía de 7,5–8,7) | Es el único objetivo de latencia que falla con una consulta a la vez. |
| 3 | ~~**9:** construcción del índice en paralelo~~ | Hecho (2026-10-16): índice de España de 136 a 74–76 s, mismo pico y segmento idéntico. |
| 4 | ~~**28:** sacar del código los datos de cada país y llevarlos a su pack~~ | Hecho: paso 2 (2026-10-16), normalizador y analizador genéricos (`providers/text-rules`) con los datos de `countries/es/`; pasos 3 y 4 (2026-10-17, commits `f069053` y `5d09511`), niveles administrativos de OSM por pack y equivalencias entre lenguas aprendidas de OSM. El 2026-10-17 se añadió un segundo pack, Luxemburgo (`countries/lu`), sin tocar el motor salvo dos arreglos genéricos del analizador. |
| 6 | **Pico de memoria del índice inverso** | 5,6 GB con España (2026-10-17), el mayor del build: el doble que el índice. |
| 5 | ~~**6f:** asignar los registros a su región por coordenadas~~ | Hecho (2026-10-16): las direcciones de España sin municipio bajan de 8.962 a 203, que caen en el mar o en puertos, fuera de todo límite. La etapa tarda 4,2 s y el acierto no cambia en ninguna categoría. |
