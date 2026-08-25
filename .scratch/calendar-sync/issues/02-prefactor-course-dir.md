# 02: Prefactor — un único cálculo de `course_dir`

**What to build:** una sola función que traduce un curso a su directorio en disco, usada por todos los flows que la necesitan.

Hoy ese cálculo existe dos veces, copiado entre el flow de contenido y el de anuncios. El flow de calendario sería la tercera copia. Extraerlo ahora hace que el trabajo siguiente sea aditivo en vez de propagar la duplicación.

Este ticket **preserva el comportamiento exactamente**: mismos directorios, mismos nombres, misma transliteración. Es un movimiento de código, no un cambio de conducta.

**Blocked by:** None (can start immediately).

**Status:** ready-for-agent

- [ ] Existe una función única que deriva el directorio de un curso, reusando la sanitización de rutas ya presente en el proyecto
- [ ] Tanto el flow de contenido como el de anuncios la usan; no queda ninguna copia del cálculo
- [ ] Los tests existentes pasan sin modificación
- [ ] Un test cubre la función extraída, incluyendo nombres de curso con acentos y con caracteres inválidos para el sistema de archivos
- [ ] Ejecutar el flow de contenido sobre un curso ya sincronizado no genera ningún directorio nuevo ni renombra los existentes
