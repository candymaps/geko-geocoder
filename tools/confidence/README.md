# Calibración de `confidence`

`confidence` de búsqueda y autocompletado es un modelo logístico (`text_confidence` en `geko-module-geocoder`) ajustado con estos dos scripts, de Python sin dependencias. El primer resultado tiene su propio modelo: es el que las aplicaciones muestran y usan, y ajustarlo aparte bajó mucho su error de calibración (BACKLOG 16b).

1. Con `geko serve` en marcha (`GEKO_URL`, por defecto `127.0.0.1:8080`) y el bundle de cada país, sin presupuesto de tiempo:
   `GEKO_URL=… python3 collect.py es-fit.jsonl ../../countries/es/golden/spain-generated.jsonl <pulsaciones.jsonl>` y
   `GEKO_URL=… python3 collect.py es-check.jsonl ../../countries/es/golden/spain-holdout.jsonl ../../countries/es/golden/spain-focus.jsonl`,
   y lo mismo con los conjuntos de los demás packs (`countries/lu/golden/generated.jsonl` con sus pulsaciones para ajustar, `holdout.jsonl` para comprobar).
   Las pulsaciones salen de `geko-eval keystrokes`; se usa una de cada cuatro.
2. `python3 fit.py es-fit.jsonl,lu-fit.jsonl es-check.jsonl,lu-check.jsonl weights.json`: ajusta los pesos por el método de Newton con los primeros ficheros, cada uno con el mismo peso total (un país con más consultas no fija el modelo él solo), e informa del error de calibración (ECE) y de Brier en cada fichero, para la `confidence` servida y para el modelo.
   - Modelo del primer resultado: los mismos ficheros filtrados a `"rank": 0` (`grep '"rank": 0,'`).
   - Modelo del resto: los ficheros enteros.
3. Copia los pesos de `weights.json` a `text_confidence`.

Los pesos dependen de los pesos del ranking (`QueryContext::weights`): si cambian, hay que reajustarlos.

Resultado de 2026-10-18 (modelo del primer resultado, con España y Luxemburgo): ECE del primer resultado en los conjuntos de control, España 0,143 → 0,057 y Luxemburgo 0,167 → 0,082.
