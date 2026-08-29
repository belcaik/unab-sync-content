## Parent

Spec #11 — VTODO de deadlines con representación humana y semántica temporal.

## What to build

Quien lea la documentación del proyecto entiende, con evidencia y sin
optimismo, hasta dónde llega cada campo del `VTODO` y por qué la meta de ver
"inicio el 9 y deadline el 16" dentro de una sola Google Task no es alcanzable
por la API pública actual.

La documentación debe dejar claras las cuatro capas y dónde se corta la cadena:
RFC 5545 permite `DTSTART` y `DUE` juntos si `DUE` es estrictamente posterior;
`caldir@vtodo-support` parsea y preserva `DTSTART` localmente pero
deliberadamente nunca lo envía a Google, igual que `URL` y `PRIORITY`; la API
pública de Google Tasks v1 expone un solo campo de fecha escribible, `due`, de
solo día y documentado explícitamente como fecha *programada* y no como
deadline; y la UI de Google sí muestra "Start date and time" y "Deadline" como
conceptos separados que la API no permite escribir.

De ahí se sigue la consecuencia práctica que la documentación debe explicar:
repetir el enlace dentro de `DESCRIPTION` no es redundancia, es la única vía
por la que llega a las notas de la tarea, porque `URL` no viaja. Y que la hora
de entrega sobrevive en el texto de la descripción precisamente porque el campo
de fecha la pierde.

Cada afirmación va con su fuente. La limitación se reporta como limitación
verificada, nunca como algo pendiente de arreglar en este repo ni como un test
que pasa.

Este ticket toca solo documentación (`docs/`, `AGENTS.md`), no `src/`, así que
puede correr en paralelo con el ticket 01.

## Acceptance criteria

- [ ] La documentación del flujo de calendario describe `SUMMARY`, `DESCRIPTION`, `DTSTART` y `DUE` tal como quedan.
- [ ] Existe una matriz `Canvas -> VTODO -> caldir -> vassago -> Google` por campo, con la fidelidad de cada tramo.
- [ ] Se declara explícitamente que inicio y deadline separados no son escribibles por la API pública de Google Tasks, citando la fuente.
- [ ] Se distingue API pública, UI y API interna/no documentada; no se sugiere usar ninguna API no documentada.
- [ ] Se explica por qué el enlace se repite en `DESCRIPTION` además de `URL`.
- [ ] `AGENTS.md` queda consistente con el comportamiento nuevo (la regla del repo es cambiarlo en el mismo commit que la conducta).
- [ ] No se afirma en ninguna parte que el resultado visual "inicio 9 / deadline 16" funcione en Google Calendar.

## Blocked by

None (can start immediately). Solo toca documentación.
