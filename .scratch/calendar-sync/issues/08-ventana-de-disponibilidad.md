# 08: Ventana de disponibilidad

**What to build:** poder ver desde cuándo se puede trabajar en cada tarea, no solo cuándo vence.

Una tarea que ya está disponible pero vence en tres semanas es trabajo que se puede adelantar hoy. Canvas lo sabe; el calendario todavía no. Esta es una de las dos preguntas que motivan el feature entero.

La ventana se representa como un `VEVENT` que abarca desde la fecha de disponibilidad hasta el vencimiento, y vive en un **calendario separado** del de vencimientos. Esa separación es el punto: permite apagar el ruido de las ventanas y quedarse solo con los deadlines cuando la semana está apretada.

Acá aparece por primera vez la granularidad ramo × semántica del spec, y con ella el lugar donde una tercera semántica (clases recurrentes) encajaría más adelante sin migrar lo existente.

**Blocked by:** 05.

**Status:** ready-for-agent

- [ ] Un assignment con fecha de disponibilidad anterior a su vencimiento produce un `VEVENT` que abarca ese intervalo
- [ ] Un assignment sin fecha de disponibilidad produce su `VTODO` y ningún `VEVENT`
- [ ] Una fecha de disponibilidad posterior al vencimiento se trata como dato inconsistente y no produce `VEVENT`
- [ ] Los `VEVENT` se escriben en un directorio distinto del de los `VTODO` del mismo ramo
- [ ] El identificador del `VEVENT` es distinguible del `VTODO` del mismo assignment
- [ ] Los tests cubren los tres casos de fechas anteriores
- [ ] La estructura de directorios resultante queda documentada en el README
- [ ] Verificado a mano: el calendario de ventanas se puede ocultar en el cliente sin afectar al de vencimientos
