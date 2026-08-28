## Parent

Spec #11 — VTODO de deadlines con representación humana y semántica temporal.

## What to build

La tarea representa correctamente apertura y vencimiento: cuando Canvas dice
que el assignment se abre antes de vencer, el `VTODO` lleva `DTSTART` con esa
apertura además del `DUE` de siempre. Un cliente CalDAV que respete `DTSTART`
sitúa la tarea en la fecha de apertura y la vence en la de entrega.

La regla es estricta y viene del RFC, no del gusto: RFC 5545 §3.8.2.3 exige que
`DUE` sea *estrictamente* posterior a `DTSTART`. Por lo tanto `unlock_at`
ausente, igual a `due_at`, o posterior a `due_at`, todos producen un `VTODO`
**sin** `DTSTART` — nunca un componente que viole el MUST, y nunca una fecha
inventada para rellenar. Es el mismo predicado `unlock < due` que el `VEVENT`
de ventana ya aplica, así que ambas colecciones quedan coherentes.

Cuando sí se emite, `DTSTART` usa exactamente la misma forma que `DUE`:
`DATE-TIME` UTC con sufijo `Z`, lo que satisface de paso la exigencia del RFC
de que ambos tipos de valor coincidan.

Un detalle deliberado: si `unlock_at` existe pero no es utilizable como
`DTSTART`, la línea `Disponible:` de la descripción **igual reporta el
`unlock_at` real**. La propiedad se omite porque el RFC lo obliga; el texto no
miente sobre lo que Canvas dijo. `sin fecha de apertura` queda reservado para
la ausencia verdadera.

No se usa `DTEND` (nunca es válido en un `VTODO`), no se intercambian `DTSTART`
y `DUE`, no se pone `unlock_at` en `DUE`, y no se inventan `deadline` ni
`X-GOOGLE-*`.

Ver ID4, ID5 y ID9 de la spec.

## Acceptance criteria

- [ ] `unlock_at < due_at` produce `DTSTART=unlock_at` y `DUE=due_at`, ambos `DATE-TIME` UTC.
- [ ] `unlock_at` ausente produce `DUE` y ningún `DTSTART`.
- [ ] `unlock_at == due_at` produce `DUE` y ningún `DTSTART`, y el componente sigue siendo válido.
- [ ] `unlock_at > due_at` se comporta igual que el caso de igualdad.
- [ ] En los dos casos incoherentes sigue sin emitirse el `VEVENT` de ventana, como hoy.
- [ ] Con `unlock_at` presente pero inutilizable, la descripción muestra el `unlock_at` real y no `sin fecha de apertura`.
- [ ] Sin `due_at` no se genera ni `VTODO` ni `VEVENT`, como hoy.
- [ ] El `VEVENT` de `windows` sigue produciendo los mismos bytes.
- [ ] `cargo fmt`, `cargo clippy -D warnings` y `cargo test` verdes, con y sin default features.

## Blocked by

- Ticket 01 (rótulo humano y descripción). Edita el mismo bloque de render del `VTODO`, así que no puede correr en paralelo.
