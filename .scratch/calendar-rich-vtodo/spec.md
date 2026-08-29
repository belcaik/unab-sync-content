# Spec: VTODO de deadlines con representación humana y semántica temporal

Ámbito: `src/calendar.rs`, colección `deadlines`. Complementa (no reemplaza)
`docs/specs/calendar-sync-flow.md`, cuyas decisiones D3, D5, D9 y D10 se
mantienen salvo donde este documento las extiende explícitamente.

Investigación de respaldo: `.scratch/calendar-rich-vtodo/research/`
(01 RFC 5545, 02 Google Tasks API, 03 caldir@vtodo-support, 04 vassago).

## Problem Statement

El `VTODO` que `u_crawler` proyecta hoy por cada assignment con `due_at` es
correcto pero pobre, y eso se nota justo donde el usuario lo lee:

1. **`SUMMARY` es solo el nombre del assignment.** En un cliente que agrega
   todos los ramos en una sola lista —que es exactamente el destino del
   pipeline: Google Tasks vía `caldir`— "Sumativa 5" no dice de qué ramo es.
   El nombre del ramo existe en el `Course` que el planner ya recibe, pero no
   llega al componente.
2. **No hay `DESCRIPTION`.** El `VTODO` lleva `URL`, pero la investigación de
   `caldir` muestra que `URL` **no se envía nunca a Google Tasks**
   (`to_google.rs:17-45`; `providers.md:56` lo documenta junto a `PRIORITY` y
   `DTSTART`). El único campo de texto libre que sí llega es `notes`, que
   `caldir` alimenta desde `DESCRIPTION`. Hoy ese campo va vacío: el usuario
   ve una tarea sin enlace y sin fechas legibles.
3. **La apertura del assignment no está en el `VTODO`.** `unlock_at` solo
   existe como `DTSTART` del `VEVENT` de `windows`, una colección hermana a la
   que un cliente puede perfectamente no estar suscrito. Quien mira la tarea no
   tiene forma de saber desde cuándo está disponible.

## Solution

El `VTODO` de `deadlines` pasa a llevar:

- `SUMMARY` = `<nombre humano del ramo> - <título del assignment>`.
- `DESCRIPTION` de tres líneas lógicas: el mismo rótulo, una línea de fechas
  (`Disponible: … - Vence: …`) y el `html_url` del assignment cuando exista.
- `DTSTART` = `unlock_at`, **solo cuando `unlock_at` es estrictamente anterior
  a `due_at`**; `DUE` sigue siendo `due_at`.

Todo lo demás del componente —`UID`, nombre de archivo, directorios, state
keys, `DTSTAMP`, `PRIORITY`, `STATUS`, `URL`, reconciliación de borrados,
idempotencia— se conserva sin cambios. El `VEVENT` de `windows` no cambia ni
un byte.

### Lo que este cambio *no* logra, y por qué (hallazgo verificado)

La meta visual planteada —una tarea que empiece el día 9 y venza el día 16
dentro de **una sola Google Task**— **no es alcanzable** desde
`unab-sync-content` a través de la API pública y el adaptador actuales. Esto
no es una limitación de esta implementación; es una asimetría real entre tres
capas, y cada tramo está verificado contra su fuente primaria:

| Capa | Qué permite | Evidencia |
|---|---|---|
| RFC 5545 | `DTSTART` y `DUE` juntos en un `VTODO`, con tipos de valor iguales y `DUE` **estrictamente** posterior | §3.6.2 (`todoprop`), §3.8.2.3 |
| caldir@`vtodo-support` | parsea `DTSTART` a `Todo.start`, lo re-serializa byte a byte, y **nunca lo envía a Google** | `to_google.rs:17-45`, `policy.rs:480`, test `dtstart_survives_the_push_and_is_never_folded_into_due` |
| Google Tasks API v1 | **un** campo de fecha escribible, `due`, solo día; documentado como fecha *programada* y explícitamente *"It doesn't represent the deadline of the task"* | Discovery Document rev. `20260825` |
| Google Tasks UI | sí muestra "Start date and time" y "Deadline" como conceptos separados | `support.google.com/tasks/answer/9901136` |

La UI expone dos conceptos que la API pública no permite escribir por separado.
No existe `startDate`, `start`, `deadline`, `scheduledDate` ni `taskDate` en el
esquema. Por lo tanto se implementa el `VTODO` **canónico y correcto**, se
verifica su preservación hasta donde el pipeline llega, y el tramo final se
reporta como **limitación verificada**, nunca como test verde.

Consecuencia de diseño directa: como `URL` tampoco llega a Google, **repetir el
enlace dentro de `DESCRIPTION` es deliberado**, no redundante. Es la única vía
por la que el enlace llega a las notas de la tarea.

## User Stories

1. Como estudiante con varios ramos en una sola lista de tareas, quiero que el
   título de la tarea diga el ramo además del assignment, para saber de qué
   curso es sin abrirla.
2. Como estudiante, quiero que el nombre del ramo sea el nombre humano que veo
   en Canvas, no el directorio saneado ni el código del curso, para que se lea
   como lo que es.
3. Como estudiante, quiero ver en la tarea desde cuándo está disponible el
   assignment, para planificar cuándo empezarlo.
4. Como estudiante, quiero ver la fecha de vencimiento en texto legible dentro
   de la tarea, además de en el campo de fecha, porque el campo de fecha pierde
   la hora al llegar a Google.
5. Como estudiante, quiero el enlace al assignment dentro de las notas de la
   tarea, porque el campo `URL` del `VTODO` no llega a Google Tasks.
6. Como estudiante cuyo assignment no tiene fecha de apertura, quiero que la
   tarea lo diga explícitamente ("sin fecha de apertura") en vez de inventar
   una fecha o dejar un hueco.
7. Como estudiante, quiero que un assignment sin `unlock_at` no gane un
   `DTSTART` inventado, para que la tarea no afirme algo que Canvas no dijo.
8. Como usuario de un cliente CalDAV que sí respeta `DTSTART`, quiero que la
   tarea con apertura conocida arranque en esa fecha y venza en la de entrega.
9. Como operador del pipeline, quiero que el componente generado sea
   RFC 5545 válido incluso cuando Canvas entrega datos incoherentes
   (`unlock_at >= due_at`), para que ningún parser aguas abajo se rompa.
10. Como operador, quiero que un `unlock_at` igual al `due_at` se trate igual
    que uno posterior, porque el RFC exige `DUE` *estrictamente* posterior.
11. Como operador, quiero que los datos incoherentes de Canvas no se corrijan
    en silencio: si `unlock_at` existe pero no es utilizable como `DTSTART`,
    la descripción igual debe reportar el `unlock_at` real.
12. Como estudiante de un ramo con tildes, comas o punto y coma en el nombre,
    quiero que la tarea muestre el texto íntegro y sin corromper.
13. Como operador, quiero que las líneas largas se plieguen conforme al RFC,
    para que un `DESCRIPTION` largo no genere un archivo no conforme.
14. Como operador, quiero que un retorno de carro en un dato de Canvas no
    parta la propiedad en dos al des-plegarse aguas abajo.
15. Como operador, quiero que este cambio produzca exactamente una reescritura
    por assignment afectado y que la corrida siguiente devuelva plan vacío.
16. Como operador, quiero que el `UID` y la ruta del archivo no cambien por
    este enriquecimiento, para no huerfanizar objetos ya publicados en CalDAV.
17. Como operador, quiero que las state keys y la reconciliación de borrados
    sigan funcionando igual.
18. Como operador, quiero que el `VEVENT` de `windows` siga generando los
    mismos bytes, para no tocar una colección que no está en el alcance.
19. Como operador, quiero que `PRIORITY`, `STATUS:COMPLETED`, el manejo de
    fallos parciales y el filtrado por `ignored_courses` sigan intactos.
20. Como operador, quiero que el fixture resultante se parsee como `VTODO`
    —nunca como `VEVENT`— en `caldir@vtodo-support`.
21. Como operador, quiero saber, con evidencia, qué campo llega a Google y cuál
    no, para no prometer un comportamiento que la API no ofrece.
22. Como mantenedor de `vassago`, quiero que la forma nueva se fusione, se
    hashee y converja sin perder estado de usuario ni generar conflictos
    repetidos.

## Implementation Decisions

### ID1 — El rótulo es un formatter puro y compartido

Una sola función pura produce el texto `<ramo> - <assignment>`, y tanto
`SUMMARY` como la primera línea de `DESCRIPTION` la usan. No hay dos formatos
que puedan divergir.

El nombre del ramo sale de `Course.name` (el nombre humano). **No** de
`fsutil::course_dir` (saneado y transliterado a ASCII) ni de `course_code`.

Une con `" - "` **solo las partes no vacías**. Un assignment sin nombre produce
el rótulo del ramo a secas, no `"Ramo - "`; un ramo sin nombre produce el
título a secas, no `" - Título"`. Así nunca aparece un guion decorativo al
comienzo ni al final.

### ID2 — `DESCRIPTION` de tres líneas, corto y estable por diseño

Líneas lógicas, separadas por el escape `\n` de RFC 5545 §3.3.11:

1. el rótulo de ID1;
2. `Disponible: <unlock_at | "sin fecha de apertura"> - Vence: <due_at>`;
3. el `html_url` del assignment, si Canvas lo entrega.

La línea 3 se **omite entera** cuando no hay `html_url` — no se emite una línea
vacía ni un texto de relleno. `DESCRIPTION` en su conjunto solo se omite si
todas sus partes faltan, situación que no se da para un assignment con ramo.

**Por qué corto importa.** La investigación de `vassago` muestra que
`DESCRIPTION` está en `COMMON_FIELDS` (bidireccional) y es una de las cinco
entradas de `shared_signature` (`bridge-vtodo.py:39-45`, `:193-214`). Si Google
normaliza el texto de `notes`, ambos lados quedan "cambiados" con firmas
distintas y el bridge levanta `CONFLICT … both sides changed shared fields`
(rc 2), que bloquea la fase 2 completa cada 15 minutos hasta intervención
humana. La mitigación que se adopta es no darle a Google nada que normalizar:
texto plano, sin HTML, sin CR, sin espacios al final, tres líneas cortas muy
por debajo del tope de 8192 caracteres de `notes`.

**Decisión explícita derivada de eso:** `assignment.description` (el HTML de
Canvas) **no** entra en el `DESCRIPTION`. Es exactamente el "blob grande
derivado de HTML" que la investigación identifica como el disparador del
conflicto. El campo existe en el struct y sigue sin usarse en `calendar.rs`.

### ID3 — Formato de fecha del texto: RFC 3339 en UTC

La línea 2 usa RFC 3339 con segundos y sufijo `Z` (`2026-09-09T14:00:00Z`).
Explícito, estable, independiente del locale y del huso de la máquina que
corre el sync. Preserva el instante.

El proyecto no tiene zona horaria de presentación configurada y este cambio
**no introduce una**: sería configuración nueva y global para una sola feature,
y `AGENTS.md` prohíbe configuración inerte. Es coherente con la decisión D9 del
spec vigente ("Zona horaria: UTC").

Esto además tiene valor propio: la investigación de `caldir` muestra que el
`DUE` que llega a Google se reduce a día **calculado en `chrono::Local`** de la
máquina que sincroniza (`create_event.rs:72`). La hora exacta de entrega, que
el campo de fecha pierde, sobrevive en el texto de las notas.

### ID4 — `DTSTART` solo cuando `unlock_at < due_at`, estricto

RFC 5545 §3.8.2.3: el valor de `DUE` *"MUST be later in time than the value of
the 'DTSTART' property"*. Es desigualdad estricta, así que `unlock_at ==
due_at` viola el MUST igual que `unlock_at > due_at`.

- `unlock_at` ausente → sin `DTSTART`.
- `unlock_at >= due_at` → sin `DTSTART`, `DUE` se conserva, el componente sigue
  siendo válido. Es el mismo predicado que el `VEVENT` ya aplica para decidir
  si emite ventana (`unlock < due`), así que ambas colecciones quedan
  coherentes: el caso incoherente produce `VTODO` sin `DTSTART` y ningún
  `VEVENT`.
- `unlock_at < due_at` → `DTSTART` como `DATE-TIME` UTC, mismo tipo de valor y
  misma forma `Z` que `DUE`, satisfaciendo de paso la exigencia de §3.8.2.3 de
  que ambos tipos coincidan.

`DTEND` no se usa: §3.8.2.2 lo limita a `VEVENT`/`VFREEBUSY` y no aparece en el
ABNF `todoprop`. No se intercambian `DTSTART` y `DUE`, no se pone `unlock_at`
en `DUE`, y no se inventan propiedades `deadline` ni `X-GOOGLE-*`.

### ID5 — La descripción reporta `unlock_at` aunque no haya `DTSTART`

Cuando `unlock_at` existe pero no es utilizable como `DTSTART`
(`unlock_at >= due_at`), la línea 2 **igual muestra el `unlock_at` real**. La
propiedad temporal se omite porque el RFC lo exige; el texto no miente sobre
lo que Canvas dijo. "Sin fecha de apertura" queda reservado para la ausencia
verdadera de `unlock_at`.

### ID6 — Escaping y normalización de fin de línea

Se respeta RFC 5545 §3.3.11 para valores `TEXT`: se escapan `\`, `;` y `,`, y
los saltos de línea se codifican como `\n`. Los dos puntos no se escapan.

Se agrega normalización de retorno de carro (`\r\n` y `\r` sueltos → `\n`)
**en el camino de texto del `VTODO`**, antes de escapar. Motivo verificado: el
`unfold` de `vassago` (`merge-ucrawler.py:21`) convierte un CR suelto en salto
de línea y parte la propiedad en dos. La normalización se hace en el formatter
nuevo y **no** dentro de `escape_text`, precisamente para que el `VEVENT`, que
comparte esa función, siga produciendo exactamente los mismos bytes.

### ID7 — Folding conforme al RFC, solo en el `VTODO`

RFC 5545 §3.1 pide líneas de máximo 75 octetos (sin contar el salto), plegadas
con CRLF más un espacio. El renderer actual no pliega; con un `DESCRIPTION` de
tres líneas eso deja de ser aceptable.

Se pliega **solo el `VTODO`**. El `VEVENT` queda literalmente intacto, por
contrato de alcance. El plegado nunca parte un carácter UTF-8 a la mitad (el
RFC llama "improperly folded" a hacerlo), lo que importa con nombres de ramo en
español.

Es seguro aguas abajo: ambos hashes de `vassago` se calculan sobre líneas
lógicas ya des-plegadas, y `caldir` des-pliega al parsear.

### ID8 — Identidad e idempotencia intactas

`UID` (`u_crawler-todo-{id}@u-crawler.local`), nombre de archivo
(`assignment-{id}.ics`), directorios `deadlines`/`windows` y state keys
(`calendar:{id}`, `calendar-window:{id}`) siguen derivando solo del id del
assignment. Un cambio de ramo, título, `unlock_at` o `due_at` reescribe **el
mismo path y el mismo UID**.

El hash de contenido (SHA-1 sobre el `.ics` renderado) cambia una vez al
introducir las propiedades nuevas → exactamente una reescritura por assignment
afectado. Como el render sigue siendo función pura de datos de Canvas —sin
reloj, `DTSTAMP` derivado de `updated_at`/`due_at` como hoy— la corrida
siguiente vuelve a producir el mismo hash y el plan queda vacío.

### ID9 — Lo que deliberadamente no se toca

- `render_vevent` y todo `windows`: mismos bytes.
- `escape_text`: misma función, misma salida.
- El escapado actual del valor `URL`. RFC 5545 §3.8.4.6 lo tipa como `URI`, que
  no lleva escapado con backslash, así que el código actual es técnicamente
  incorrecto para una URL con coma. No se cambia aquí: no forma parte del
  encargo, `URL` es compartida con el `VEVENT` congelado, ninguna URL de Canvas
  observada trae comas, y `caldir` la preserva tal cual y nunca la envía a
  Google. Queda anotado como deuda conocida.
- `assignment.description` sigue sin usarse (ver ID2).
- El requisito de `due_at`: sin `due_at` no hay `VTODO` ni `VEVENT`, igual que
  hoy.

## Testing Decisions

**Qué hace un buen test acá.** Igual que en el spec vigente: entrada de
assignments/submissions/estado previo, aserción sobre el **plan devuelto** y
sobre el `.ics` que contiene. Nunca sobre funciones privadas ni sobre cómo el
planner llegó ahí.

**Expectativas literales e independientes.** El texto esperado se escribe a
mano en el test, nunca recalculado con el mismo helper bajo prueba. Un test que
compone el esperado con el formatter que está probando pasa por construcción y
no puede discrepar del código.

**Seams (preacordados, no se renegocian).**

1. *Seam principal:* la función pública y pura `plan`, observada a través del
   contenido `.ics` de los `PlannedWrite`. Es el seam único que la decisión D10
   del spec vigente ya estableció; no se abre ninguno nuevo.
2. *Seam de regresión:* la misma `plan`, para `VTODO` y `VEVENT`. Sin mockear
   helpers privados.
3. *Seam de compatibilidad:* el fixture `.ics` resultante, parseado y
   proyectado por `caldir@vtodo-support`, y reconciliado por los tests de
   `vassago`, en clones temporales de solo lectura.

**Prior art:** `#[cfg(test)] mod tests` al pie del módulo, sobre funciones
puras — el estilo que ya usan `links.rs`, `fsutil.rs`, `state.rs` y el propio
`calendar.rs`.

**Casos obligatorios.**

- Assignment completo (ramo, título, `unlock_at`, `due_at`, URL) → un solo
  `VTODO` con `UID`/filename estables, `DTSTART=unlock_at`, `DUE=due_at`,
  `SUMMARY` compuesto, `DESCRIPTION` de tres líneas, y `URL`/`PRIORITY`/estado
  de completado preservados.
- Sin `unlock_at` → sin `DTSTART`, con `DUE`, descripción diciendo "sin fecha
  de apertura".
- `unlock_at == due_at` y `unlock_at > due_at` → sin `DTSTART`, `VTODO` válido,
  sin `VEVENT` de ventana.
- Sin `due_at` → ni `VTODO` ni `VEVENT`.
- Ramo y título con coma, punto y coma, backslash, salto de línea, CR y
  Unicode → `.ics` parseable y texto recuperable sin corrupción.
- Sin `html_url` → sin línea vacía ni falsa; el resto de la descripción sigue
  siendo útil.
- Cambio de ramo/título/`unlock_at`/`due_at` → reescribe el mismo path y UID;
  segunda corrida con la misma entrada → plan vacío.
- Los tests vigentes de prioridad, completado, borrado, fallo parcial y
  ventanas siguen pasando; los bytes del `VEVENT` se pinnean explícitamente.
- Líneas largas plegadas a ≤75 octetos sin partir un carácter UTF-8.

**Verificación externa (no son tests de este repo, son evidencia).**
El fixture se parsea con `caldir@vtodo-support` (`cargo test`, sin
credenciales; los tests live están gateados por `CALDIR_LIVE_GOOGLE`), y los
tres archivos de test de `vassago` se corren con
`python3 -m unittest discover -s tests` (stdlib, sin red). Cualquier bloqueo se
reporta con el comando exacto y el error exacto — nunca se oculta un skip.

## Out of Scope

- Escribir una fecha de inicio y un deadline separados en Google Tasks. La API
  pública no lo permite (ver la tabla de arriba). No se usa ninguna API interna
  ni no documentada.
- Cambiar `windows` / `render_vevent`.
- Meter el HTML de `assignment.description` en el `DESCRIPTION`.
- Arreglar el escapado del valor `URL` (deuda anotada en ID9).
- Cambiar `UID`s, nombres de archivo, directorios o state keys.
- Modificar `caldir` o `vassago`, o hacer push a esos repos.
- Escrituras contra una cuenta real de Google.
- Introducir una zona horaria de presentación configurable.
- Clases recurrentes (RRULE) y mock server HTTP: siguen fuera, como en el spec
  vigente.

## Further Notes

- **Riesgo residual conocido, no mitigable desde este repo.** `DESCRIPTION` es
  bidireccional en `vassago` y alimenta `shared_signature`. Si alguna vez
  Google normaliza el texto de `notes`, puede aparecer un `CONFLICT` pegajoso
  que bloquea la publicación. Este spec lo minimiza manteniendo el texto
  plano, corto y estable (ID2). La solución de fondo —mover `DESCRIPTION` de
  `COMMON_FIELDS` a `RICH_FIELDS`— vive en `vassago` y requiere un ADR allá.
- **Quirk preexistente, ajeno a este cambio.** `merge-ucrawler.py` descarta el
  `STATUS:COMPLETED` derivado de Canvas en cada actualización si el archivo
  canónico no tenía `STATUS`; solo llega en la creación inicial.
- **`DTSTART` genera un push loop benigno** en `bridge-vtodo.py` (está en
  `RICH_FIELDS`, canónico → Google), exactamente como ya ocurre hoy con
  `PRIORITY` y `URL`. rc 0, sin conflicto.
- **`DUE` pierde la hora al llegar a Google**, y `caldir` reescribe esa pérdida
  de vuelta en su propio archivo local. La hora real sobrevive en el texto de
  `DESCRIPTION` (ID3), que es parte del motivo de que esa línea exista.
