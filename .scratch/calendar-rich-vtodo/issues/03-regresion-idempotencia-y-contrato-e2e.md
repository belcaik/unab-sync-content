## Parent

Spec #11 — VTODO de deadlines con representación humana y semántica temporal.

## What to build

El operador puede confiar en que enriquecer el `VTODO` no rompió la identidad
de los objetos ya publicados ni el ciclo de sincronización, y tiene evidencia
de que la forma nueva atraviesa el pipeline real.

Dentro del repo: un cambio de ramo, título, `unlock_at` o `due_at` reescribe
**el mismo path y el mismo UID** en vez de huerfanizar el archivo anterior, y
correr dos veces con la misma entrada de Canvas produce plan vacío la segunda
vez. La introducción de `DESCRIPTION` y `DTSTART` cuesta exactamente una
reescritura por assignment afectado, y después converge. State keys,
reconciliación de borrados, `PRIORITY`, `STATUS:COMPLETED`, fallo parcial por
ramo y filtrado por `ignored_courses` siguen comportándose igual.

Fuera del repo, como evidencia y no como test propio: el fixture `.ics`
generado se parsea en `caldir@vtodo-support` y se comprueba que llega como
`VTODO` —nunca como `VEVENT`— y que `DTSTART`, `DUE`, `SUMMARY`, `DESCRIPTION`
y `URL` sobreviven al round trip local. Y los tests de `vassago`
(`test_merge_ucrawler.py`, `test_bridge_vtodo.py`, `test_bridge_windows.py`) se
corren contra la forma nueva para demostrar que se fusiona, se hashea y
converge sin perder estado de usuario ni levantar conflictos repetidos.

Ambos repos externos se usan en clones o worktrees temporales de solo lectura.
No se modifican ni reciben push. Si algo no se puede correr, se reporta el
comando exacto y el bloqueo exacto; no se oculta ningún skip ni se declara
verde un tramo que no se pudo comprobar.

Ver ID8 y la sección de Testing Decisions de la spec.

## Acceptance criteria

- [ ] Un cambio de ramo, título, `unlock_at` o `due_at` reescribe el mismo path y conserva el UID.
- [ ] Una segunda corrida con la misma entrada produce plan vacío (la garantía de idempotencia).
- [ ] Registrar el estado tras la primera corrida y volver a planificar produce cero writes.
- [ ] Los tests vigentes de prioridad, completado, borrado, fallo parcial y ventanas pasan sin cambios observables no solicitados.
- [ ] Un fixture canónico se parsea en `caldir@vtodo-support` como `VTODO` y conserva `DTSTART`, `DUE`, `SUMMARY`, `DESCRIPTION` y `URL` en el round trip local.
- [ ] Los tres archivos de test de `vassago` corren contra la forma nueva y pasan, con el comando exacto registrado.
- [ ] Ningún archivo de los repos externos queda modificado (`git status` limpio en ambos clones).
- [ ] Cualquier tramo no verificable queda reportado como bloqueo con su comando y su error exactos.

## Blocked by

- Ticket 02 (DTSTART y DUE). El fixture de compatibilidad solo tiene sentido sobre la forma final del `VTODO`.
