# Método de fusión de geko

Este documento describe cómo geko construye su base de datos de geocodificación a partir de OpenStreetMap y de otras fuentes. Es la descripción del método que ofrece la cláusula 4.6 de la Open Database License (ODbL) para la base de datos derivada que sirve geko. Quien reciba resultados de un servicio geko puede pedir, además, el código necesario para reproducirla, en la dirección de contacto de `/v1/attribution`.

## 1. Fuentes

Cada bundle lista sus fuentes, licencias y atribuciones en `/v1/attribution`. Todas son públicas:

- **OpenStreetMap** (ODbL 1.0, © OpenStreetMap contributors): extractos regionales de Geofabrik en formato PBF, sin cambios. Aporta calles con su geometría, portales, lugares, puntos de interés y límites administrativos.
- **CartoCiudad** (CC BY 4.0, © Instituto Geográfico Nacional), solo en España: portales con número y código postal, descargados de la OGC API Features del IDEE (`api-features.idee.es`, colección `address`) por código postal.
- **Overture Maps Places** (CDLA-Permissive-2.0, © Overture Maps Foundation), en todos los países: lugares abiertos con confianza de al menos 0,8, de las versiones públicas de Overture (`overturemaps-us-west-2.s3.amazonaws.com`). Se descargan solo las partes de sus ficheros que tocan el país, y su categoría se traduce a la etiqueta de OSM más parecida.

Las fuentes no se modifican: se leen, se normalizan para compararlas y se funden en una entidad por cada objeto del mundo real.

## 2. Partición

Cada país se divide en particiones, sus provincias o el país entero si no tiene subdivisiones declaradas. Cada partición recorta cada fuente por su límite oficial de OSM. Un registro que cae fuera de la partición de su fuente se envía a la que lo contiene.

## 3. Normalización para comparar

Cada nombre se compara en una forma normalizada:

- minúsculas, sin tildes ni signos;
- sin artículos ni preposiciones de enlace;
- con el tipo de vía separado del nombre y llevado a un código común a todas las lenguas del país (en España, «calle», «carrer», «rúa» y «kalea» son `CL`).
- con los códigos de vía como una sola palabra: «A-7» y «A7» son lo mismo.

Los números de portal se separan en número, sufijo y rango. Los números ficticios conocidos de una fuente se tratan como «sin número».

## 4. Jerarquía administrativa

El municipio y el resto de la jerarquía de cada registro se asignan **por sus coordenadas**, con los límites administrativos de OSM, nunca por el nombre de municipio que traiga una fuente.

## 5. Reglas de fusión por tipo de entidad

| Entidad | Se funden en una si… |
|---|---|
| Tramos de una calle (dentro de una fuente) | tienen el mismo nombre normalizado y el mismo tipo de vía, en el mismo municipio y la misma localidad, y están conectados o a menos de 500 m |
| Calle de dos fuentes | mismo municipio, nombre normalizado igual o muy parecido y tipo de vía compatible. Un portal se une a la calle de su nombre a menos de 300 m de su línea (150 m si el nombre solo se parece). Los portales sin calle con geometría se agrupan en calles por nombre a menos de 400 m |
| Portal | misma calle ya fundida, mismo número y sufijo, y a menos de 75 m (30 m sin número) |
| Lugar (ciudad, villa, pueblo) | si tiene el mismo nombre que el municipio que lo contiene, se funde en ese municipio. Lo mismo para una unidad administrativa de nivel de barrio con el nombre de su municipio |
| Plaza o paraje con nombre de calle | se funde con la calle del mismo nombre a menos de 150 m |
| Punto de interés | misma familia de categoría y mismo nombre a menos de 30 m (200 m para estaciones de transporte, cuyas partes se unen a la estación) |
| Punto de interés de otra fuente | se funde con el de OSM a menos de 75 m cuyo nombre es igual, o que lo contiene como palabras completas («Carrefour Express» y «Carrefour Express Calle Oms 35») y es de la misma clase de sitio: la misma categoría, o dos categorías que las dos fuentes dan al menos 3 veces en la partición a POIs vecinos con el mismo nombre (así se aprende que el `shop=supermarket` de una es el `shop=convenience` de otra). Sin esto, «Fátima», una parada de taxis, se fundía con «Parroquia de Nuestra Señora de Fátima». OSM conserva su posición y su categoría, y el otro nombre se suma como variante. Si el nombre se escribe de otra forma («Restaurant Ca'n Salvador» y «Can Salvador»), solo se funde con más pruebas a la vez: el mismo teléfono o la misma web (de los que no tienen más de 4 POIs de la partición: una cadena, o un hotel y su restaurante, comparten los suyos), una palabra en común que no esté en más de 20 POIs de la partición (no «asador» ni «viajes»), o un nombre dentro del otro sin espacios, y la misma clave de categoría. Sin pruebas así, se queda como POI aparte: un nombre parecido o el mismo portal no bastan, porque en un mismo edificio hay muchos negocios. Si no hay ninguno, es un POI nuevo |
| Límite administrativo | se toma de OSM tal cual |

Un registro que no se empareja con ninguno es una entidad propia: así se añaden los portales que solo están en CartoCiudad.

## 6. Atributos de cada entidad

- **Nombres:** se unen los de todas las fuentes, incluidos los nombres en otras lenguas (`name:*`), como variantes de la entidad.
- **Posición y número de un portal:** los de CartoCiudad si los tiene; si no, los de OSM.
- **Geometría de calles y puntos de interés:** la de OSM.
- **Código postal de un portal:** el de su fuente si lo trae. Si no, se **infiere** del de los portales cercanos de la misma calle y del mismo lado; y, si no hay, del código mayoritario de su celda H3 (resolución 9). Los códigos inferidos van marcados como tales.
- **Dirección de un punto de interés que no la trae:** la del portal más cercano, a menos de 25 m y en el mismo municipio, marcada como inferida.
- **Importancia:** sale de la población, del número de portales de cada calle y del número de nombres en otras lenguas, con escalas fijas.

Cada entidad guarda las fuentes y los identificadores de origen de los que procede.

## 7. Identificadores

Cada entidad recibe un identificador público estable, derivado de su país, su tipo, su municipio y su nombre normalizado, o del identificador de OSM de su origen.

## 8. Reproducibilidad

La generación es determinista: con los mismos datos de entrada y la misma versión del software se obtiene la misma base de datos, byte a byte. `/v1/attribution` indica la versión del bundle servido, y su manifiesto lista las fuentes empleadas.
