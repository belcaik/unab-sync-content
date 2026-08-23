# 06: Idempotencia y cambio de fecha

**What to build:** que la corrida diaria sea silenciosa cuando no hay novedades, y limpia cuando el profesor mueve una fecha.

Dos comportamientos, una misma raíz. Si Canvas no cambió, ningún archivo se toca: eso mantiene el historial legible y —efecto lateral valioso— hace que una marca manual del usuario sobre algo de lo que Canvas no tiene opinión sobreviva a la corrida. Si Canvas cambió, el componente se reescribe pisando, porque Canvas es la fuente de verdad.

El caso filoso es el cambio de fecha: si el nombre de archivo depende de la fecha, mover un deadline crea un archivo nuevo y deja el viejo. Dos vencimientos contradictorios en el calendario es peor que ninguno. El ticket 01 determina si caldir reconcilia por identificador interno; este ticket implementa la limpieza que corresponda según ese hallazgo.

La comparación se hace contra el estado persistido, no leyendo los archivos de salida, siguiendo el mismo patrón con el que el flow de contenido evita retrabajo.

**Blocked by:** 05.

**Status:** ready-for-agent

- [ ] El estado persistido registra, por assignment, lo suficiente para detectar si el componente proyectado cambió
- [ ] Una segunda corrida sin cambios en Canvas deja todos los archivos con su fecha de modificación intacta
- [ ] Un cambio de fecha de entrega actualiza el componente
- [ ] Si el cambio de fecha implica un nombre de archivo distinto, el archivo anterior deja de existir
- [ ] Un cambio de título actualiza el componente conservando su identificador
- [ ] Los tests cubren: sin cambios produce plan vacío, cambio de fecha produce escritura más limpieza, cambio de título produce escritura sin limpieza
- [ ] El test de "plan vacío cuando nada cambió" existe y pasa: es la garantía de todo el comportamiento de este ticket
