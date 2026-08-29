## Parent

Spec #11 — VTODO de deadlines con representación humana y semántica temporal.

## What to build

Un estudiante que mira su lista de tareas ve, por cada assignment con fecha de
entrega, una tarea titulada `<nombre humano del ramo> - <título del assignment>`
y, dentro de ella, una descripción de tres líneas: el mismo rótulo, la línea
`Disponible: … - Vence: …` y el enlace al assignment.

El nombre del ramo es el nombre humano de `Course`, nunca el directorio saneado
ni el código del curso. Un único formatter puro produce el rótulo y lo comparten
`SUMMARY` y la primera línea de `DESCRIPTION`, de modo que no puedan divergir.

Las fechas del texto van en RFC 3339 UTC con segundos (`2026-09-09T14:00:00Z`),
sin introducir configuración nueva de zona horaria. Cuando el assignment no
tiene `unlock_at`, la línea dice literalmente `sin fecha de apertura`. Cuando
Canvas no entrega `html_url`, la tercera línea no se emite en absoluto — ni
vacía ni de relleno.

El texto resultante debe sobrevivir intacto a los parsers de aguas abajo: se
escapan `\`, `;`, `,` y los saltos de línea como manda RFC 5545 §3.3.11, los
retornos de carro se normalizan antes de escapar (un CR suelto parte la
propiedad al des-plegarse en vassago), y las líneas se pliegan a 75 octetos con
CRLF más espacio sin partir jamás un carácter UTF-8 a la mitad.

Solo cambia el `VTODO` de `deadlines`. El `VEVENT` de `windows` debe seguir
emitiendo exactamente los mismos bytes, y `escape_text` debe seguir devolviendo
exactamente lo mismo para cualquier entrada — la normalización de CR vive en el
camino de texto nuevo, no dentro de ella.

Ver ID1, ID2, ID3, ID6, ID7 y ID9 de la spec.

## Acceptance criteria

- [ ] Un assignment con ramo, título y URL produce `SUMMARY:<ramo> - <título>`.
- [ ] `DESCRIPTION` lleva las tres líneas lógicas separadas por el escape `\n`.
- [ ] Sin `unlock_at`, la línea de fechas dice `sin fecha de apertura` y no una fecha inventada.
- [ ] Sin `html_url`, no aparece tercera línea, ni vacía ni falsa.
- [ ] Título o ramo vacío no produce un guion decorativo suelto al inicio ni al final.
- [ ] Ramo/título con coma, punto y coma, backslash, salto de línea, CR y Unicode producen un `.ics` parseable cuyo texto se recupera sin corrupción.
- [ ] Ninguna línea del `VTODO` supera 75 octetos; las plegadas continúan con un espacio y no parten un carácter UTF-8.
- [ ] Un test pinnea los bytes del `VEVENT` de `windows` y demuestra que no cambiaron.
- [ ] Las expectativas de los tests son literales escritas a mano, no recalculadas con el formatter bajo prueba.
- [ ] `cargo fmt`, `cargo clippy -D warnings` y `cargo test` verdes, con y sin default features.

## Blocked by

None (can start immediately).
