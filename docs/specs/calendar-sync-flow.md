# Spec: flow de sincronización de calendario

**Estado:** acordado — T1 resuelto empíricamente (ver «Hallazgos del spike»)
**Destino:** spec acordado. La implementación es un esfuerzo aparte.

---

## Problem Statement

Hoy u_crawler baja el **contenido** de Canvas a disco (módulos, páginas, archivos, anuncios, grabaciones), pero no baja **cuándo** hay que hacer las cosas. Las fechas existen en Canvas y se descartan: `Assignment` solo guarda `id`, `name`, `description`, `updated_at`.

El resultado es que para planificar la semana hay que abrir Canvas ramo por ramo y leer cada tarea a mano. Dos preguntas concretas quedan sin responder desde el calendario:

1. **"¿Qué puedo adelantar?"** — una tarea que ya está disponible pero vence en tres semanas es trabajo que se puede hacer hoy. Canvas lo sabe (`unlock_at`), el calendario no.
2. **"¿Qué importa de verdad?"** — un quiz que no pesa en la nota final y un entregable que vale el 30% se ven idénticos en una lista de vencimientos.

Además, la ejecución es manual: u_crawler es un ejecutable que se corre a mano, no algo que pueda vivir en un cron dentro de un contenedor.

## Solution

Un **segundo flow, independiente del de contenido**, que corre diario por cron dentro de Docker y proyecta el estado de Canvas a archivos ICS en el árbol de caldir. Desde ahí `caldir push` los sube a Radicale por CalDAV, y cualquier cliente (Thunderbird u otro) los consume.

Para cada assignment se emiten dos componentes distintos, porque responden preguntas distintas:

- **Deadline → `VTODO`.** Es una tarea con `DUE`, `PRIORITY` y `STATUS`: tildeable, priorizable, y con estado de completitud real.
- **Ventana de disponibilidad → `VEVENT`.** Un intervalo de `unlock_at` a `due_at` que muestra *cuándo se puede trabajar* en algo, no solo cuándo vence.

Los calendarios se parten por **ramo × semántica**, de modo que se pueda apagar el ruido de las ventanas y quedarse solo con vencimientos, o silenciar un ramo entero sin tocar los demás.

La prioridad se deriva del impacto en la nota final: lo que pesa va a `PRIORITY:1` (la máxima en iCalendar), lo que no pesa cae más abajo.

## User Stories

1. Como estudiante, quiero ver los vencimientos de todos mis ramos en mi calendario, para no tener que abrir Canvas ramo por ramo.
2. Como estudiante, quiero que cada entregable aparezca como una tarea tildeable y no como un evento, para poder marcar lo que ya hice.
3. Como estudiante, quiero ver desde qué fecha está disponible cada tarea, para poder adelantar trabajo cuando tengo tiempo libre.
4. Como estudiante, quiero que las tareas que pesan en mi nota final se distingan visualmente de las que no, para priorizar bajo presión de tiempo.
5. Como estudiante, quiero que una tarea sin peso en la nota siga apareciendo, para no perderme entregas obligatorias que no suman puntos.
6. Como estudiante, quiero un calendario separado por ramo, para poder colorearlos distinto y apagar los que no estoy cursando.
7. Como estudiante, quiero un calendario separado para las ventanas de disponibilidad, para poder ocultarlas cuando solo me interesan los vencimientos.
8. Como estudiante, quiero que si el profesor cambia una fecha de entrega, mi calendario refleje la nueva fecha al día siguiente, para no trabajar contra información vieja.
9. Como estudiante, quiero que si el profesor cambia una fecha, **no** me quede además el evento viejo, para no ver dos fechas contradictorias.
10. Como estudiante, quiero que un entregable grupal que entregó un compañero aparezca como hecho, para no volver a trabajar en algo ya entregado.
11. Como estudiante, quiero que una tarea calificada aparezca como completada, para tener registro de lo hecho sin que desaparezca.
12. Como estudiante, quiero que una tarea que el profesor eliminó desaparezca de mi calendario, para que no se acumule basura con el tiempo.
13. Como estudiante, quiero que si marco algo como hecho manualmente y Canvas no tiene opinión al respecto, mi marca sobreviva a la siguiente corrida.
14. Como estudiante, quiero que si nada cambió en Canvas, la corrida diaria no toque ningún archivo, para que el historial de sync sea legible y no genere ruido.
15. Como estudiante, quiero que los ramos que ignoro en el config no generen calendarios, para reusar la configuración que ya tengo.
16. Como estudiante, quiero que las fechas se muestren en mi hora local aunque el archivo esté en UTC, para no calcular diferencias horarias mentalmente.
17. Como operador del homeserver, quiero que el flow corra por cron sin intervención, para no acordarme de ejecutarlo.
18. Como operador, quiero que el flow escriba en un volumen montado, para que otros contenedores lo consuman.
19. Como operador, quiero que el flow tenga un `--dry-run` que no escriba nada, para verificar qué haría antes de dejarlo suelto.
20. Como operador, quiero que el flow falle con un código de salida distinguible, para que el cron pueda alertarme.
21. Como operador, quiero que un ramo que falla no impida sincronizar los demás, para no perder todo el calendario por un error puntual.
22. Como operador, quiero que el flow no necesite un navegador headless, para que la imagen Docker sea chica y el build no dependa de `chromiumoxide`.
23. Como operador, quiero que las credenciales no queden en la imagen, para poder publicarla o compartir el compose sin filtrar mi token.
24. Como desarrollador, quiero que la lógica de decisión sea una función pura, para poder testear la heurística sin red ni disco.
25. Como desarrollador, quiero que el flow reuse `HttpCtx` y `list_paginated`, para no romper los invariantes de `AGENTS.md`.
26. Como desarrollador, quiero que el layout de salida contemple un tercer tipo de calendario (clases recurrentes), para agregarlo después sin migrar los archivos existentes.

## Implementation Decisions

### D1 — Fuente de datos: REST, con el feed ICS como complemento

La API REST es la fuente primaria. El feed ICS de Canvas **no expone `unlock_at` ni `points_possible`**, con lo cual ni la ventana de disponibilidad ni la heurística de prioridad son derivables de él. El feed queda reservado para los calendar events / clases síncronas, que la REST expone peor.

Endpoints nuevos (todos por `CanvasClient::list_paginated`, y todos a agregar a la tabla "Canvas API Contract (v1)" de `AGENTS.md` **en el mismo commit**):

- `GET /api/v1/courses/{id}/assignments?per_page=100` — ya existe, pero hay que **ensanchar el struct**.
- `GET /api/v1/courses/{id}/students/submissions?student_ids[]=self&per_page=100` — nuevo. Un solo bulk call por curso.

**Nota de encaje:** `list_paginated` recibe un `&str` que se une a `base`; no hay query-builder en `CanvasClient` (a diferencia de `ZoomClient`). Los params repetidos tipo `student_ids[]` hay que pre-encodearlos en el path.

### D2 — Ensanchar `Assignment`

Se agregan campos, todos `Option`: `due_at`, `unlock_at`, `lock_at`, `points_possible`, `omit_from_final_grade`, `html_url`, `assignment_group_id`, `submission_types`, `published`.

Es un cambio **aditivo y seguro**: serde ya ignora campos desconocidos y el resto del código accede por nombre.

**Prerrequisito:** `chrono` está declarado con `default-features = false, features = ["clock"]` — **sin `serde`**. Hay que habilitarlo antes de poder deserializar `DateTime<Utc>` directamente. Alternativa: mantener los campos como `String` y parsear en el borde. Se prefiere habilitar `serde` y tipar las fechas de verdad, porque el resto del código ya sufre de comparar timestamps como strings (`status` ordena lexicográficamente).

### D3 — Modelo ICS: VTODO + VEVENT

Por cada assignment con `due_at`:

- Un **`VTODO`** con:
  - **`SUMMARY` = `<nombre humano del ramo> - <título del assignment>`.** El ramo sale de `Course.name` —el nombre humano tal como se ve en Canvas—, **no** del directorio saneado de `fsutil::course_dir` ni del código de curso. Se unen con `" - "` **solo las partes no vacías**, así que un assignment sin título produce el ramo a secas y nunca queda un guion suelto al principio ni al final. El rótulo existe porque el destino real del pipeline agrega todos los ramos en una sola lista de tareas, donde "Sumativa 5" no dice de qué curso es.
  - **`DESCRIPTION` de tres líneas lógicas**, separadas por el escape `\n` de RFC 5545 §3.3.11:
    1. el mismo rótulo de `SUMMARY`;
    2. `Disponible: <unlock_at | "sin fecha de apertura"> - Vence: <due_at>`, con las fechas en RFC 3339 UTC (`2026-09-09T14:00:00Z`), coherente con D9;
    3. el `html_url` del assignment. La línea se **omite entera** si Canvas no lo entrega: no se emite una línea vacía ni texto de relleno.

    El texto es deliberadamente plano, corto y estable: sin HTML, sin retornos de carro, sin espacios finales. El HTML de `assignment.description` **no** entra acá (ver D13).
  - **`DTSTART` = `unlock_at`, solo cuando `unlock_at` es estrictamente anterior a `due_at`.** RFC 5545 §3.8.2.3 exige que el valor de `DUE` sea *"later in time"* que el de `DTSTART`, así que `unlock_at == due_at` viola el MUST igual que `unlock_at > due_at`. Sin `unlock_at`, o con un `unlock_at` no utilizable, el `VTODO` va sin `DTSTART` y sigue siendo válido — pero la línea 2 de `DESCRIPTION` **igual reporta el `unlock_at` real** que Canvas dio: la propiedad temporal se omite porque el RFC lo exige, el texto no miente. `"sin fecha de apertura"` queda reservado para la ausencia verdadera. Es el mismo predicado (`unlock < due`) que decide si se emite el `VEVENT`, así que ambas colecciones quedan coherentes.
  - `DUE` = `due_at`, `PRIORITY` según D6, `URL` = `html_url`, `STATUS` según D5.
- Un **`VEVENT`** con `DTSTART` = `unlock_at` y `DTEND` = `due_at`, **solo si `unlock_at` existe y es anterior a `due_at`**. Sin `unlock_at` no hay ventana que representar y no se emite nada.

Un assignment sin `due_at` no genera componentes: no hay nada que ubicar en el tiempo.

`DTEND` **no** se usa en el `VTODO`: RFC 5545 §3.8.2.2 lo limita a `VEVENT`/`VFREEBUSY` y no aparece en el ABNF `todoprop` (§3.6.2). Tampoco se intercambian `DTSTART` y `DUE`, ni se inventan propiedades `deadline` o `X-GOOGLE-*`.

**El enlace aparece dos veces a propósito.** `URL` y la línea 3 de `DESCRIPTION` llevan el mismo `html_url` porque `caldir` **nunca** envía `URL` a Google Tasks, mientras que `DESCRIPTION` sí llega, como el campo `notes` de la tarea. Duplicarlo no es redundancia: es la única vía por la que el enlace llega a las notas. Por el mismo motivo la hora de entrega se escribe en el texto además de en `DUE` — el campo de fecha la pierde al llegar a Google. Ver D13 y «Límite verificado: Google Tasks» en Further Notes.

Solo el `VTODO` se pliega según RFC 5545 §3.1 (líneas de ≤75 octetos, continuación con CRLF + espacio, sin partir un carácter UTF-8). El `VEVENT` de `windows` conserva sus bytes exactos: no está en el alcance de este enriquecimiento.

El registro de decisiones numeradas `ID1`–`ID9` del enriquecimiento del `VTODO` —las que citan los comentarios `spec IDn` de `src/calendar.rs`— vive con las notas de trabajo de la feature, en `.scratch/calendar-rich-vtodo/spec.md`. **Este documento es el contrato**; ese archivo es el registro de diseño de respaldo, igual que `research/`.

### D4 — Layout: ramo × semántica, en el árbol de caldir

u_crawler escribe **directo** en el árbol de caldir, y es **dueño exclusivo** de los directorios que crea. Ningún otro proceso escribe ahí; se documenta como invariante en `AGENTS.md`.

Un directorio por (curso, semántica). El nombre del directorio se deriva del curso con `fsutil::sanitize_component`, que ya garantiza determinismo y transliteración ASCII.

El layout deja lugar para una tercera semántica (clases recurrentes) sin migrar nada.

### D5 — Canvas es la fuente de verdad

La regla, en orden:

| Situación | Acción |
|---|---|
| Canvas cambió (fecha, título, peso) | Se reescribe el componente, pisando |
| Canvas dice entregado/calificado, el archivo no | Se reescribe con `STATUS:COMPLETED` |
| Canvas y el archivo coinciden | **No se toca el archivo** |
| El assignment ya no está en Canvas | Se borra el archivo |

La comparación **no** lee el archivo de salida: se hace contra `state.json`, con un namespace nuevo (`calendar:{assignment_id}`), igual que hoy se saltea contenido sin cambios por SHA-1 (`syncer.rs:288`). Barato y consistente con lo existente.

**Consecuencia deseable, no accidental:** como solo se escribe cuando el *proyectado* cambió, un tilde manual sobre algo de lo que Canvas no tiene opinión (una lectura sin entrega) sobrevive. Se pierde solo si el profesor edita la tarea. Esto sale del principio, no de un caso especial.

### D6 — Heurística de prioridad

`PRIORITY:1` (máxima) ⇔ `points_possible > 0` **y** `omit_from_final_grade != true`.

Todo lo demás cae a un bucket inferior, cuyo mapeo completo a la escala 1–9 queda pendiente (T3).

**Corrección respecto al pedido original:** en iCalendar `PRIORITY:0` significa *sin definir*, no *máxima*. La escala es 1–9 con **1 = más alta**. Emitir `0` haría que el cliente muestre "sin prioridad", exactamente lo opuesto a lo buscado.

Ponderar por peso del grupo de assignments (`/assignment_groups` + `course.apply_assignment_group_weights`) sería más fiel, pero se dejó fuera: agrega un endpoint y lógica, y la regla simple ya separa el caso que importa.

**Rechazado explícitamente:** incluir proximidad del deadline en la prioridad. Haría que la prioridad cambie sola cada día, lo que obliga a reescribir componentes sin que Canvas haya cambiado — contradice D5 y destruye la propiedad de "no tocar nada si nada cambió".

### D7 — Detección de "hecho"

Se considera hecho si `submitted_at` está presente **o** `workflow_state == "graded"`.

Canvas propaga la entrega de un compañero al registro propio en entregas grupales, así que el caso del entregable grupal queda cubierto **sin lógica especial**.

### D8 — Transporte: u_crawler no habla CalDAV

u_crawler escribe archivos y muere. `caldir push` es el cliente CalDAV contra Radicale, y sube deletions, lo que hace que el borrado de D5 funcione end-to-end sin que u_crawler sepa nada de la red.

### D9 — Zona horaria: UTC

Canvas devuelve ISO-8601 en UTC. Se emite `DUE`/`DTSTART`/`DTEND` con sufijo `Z` y el cliente convierte a hora local al mostrar. Sin `VTIMEZONE`, sin tablas de zonas, sin bugs de horario de verano.

### D10 — Seam único: un planner puro

Este es el punto central del diseño para testeabilidad.

El flow se parte en dos, y **solo la primera mitad se testea**:

1. **Planner (puro).** Recibe: el curso, sus assignments, sus submissions, y el `State` previo. Devuelve un **plan**: qué archivos escribir con qué contenido, y qué archivos borrar. Cero I/O — sin red, sin disco, sin reloj (el "ahora" se inyecta).
2. **Ejecutor (I/O).** Toma el plan y lo aplica con `fsutil::atomic_write` / `atomic_rename`, y actualiza `state.json`.

Toda la lógica interesante —prioridad, ventanas, detección de hecho, reconciliación, renombres, borrados— vive del lado puro. El ejecutor es lo bastante trivial como para no necesitar mocks, lo cual importa porque **el repo no tiene ninguna infraestructura de mocking HTTP** y este spec no la introduce.

Esto respeta la instrucción de `AGENTS.md`: "prefer pure testable functions".

### D11 — CLI y config

Un subcomando nuevo, hermano de `sync` / `announcements` / `recordings`, con la firma establecida en el repo: `run_*(filter_course_id: Option<u64>, dry_run: bool) -> anyhow::Result<()>`, más una variante en `Commands` y un arm que devuelve `ExitCode::from(12)` en error.

Reusa `canvas.ignored_courses` (user story 15). Las keys de config nuevas deben ser **leídas por el código** — `AGENTS.md` prohíbe config inerte — y agregadas a `assets/config.toml` en el mismo commit.

**Deuda a saldar de paso:** `course_dir` está duplicado en `syncer.rs:63-71` y `announcements.rs:112`. Una tercera copia es el momento de extraerlo.

**Atención:** `main.rs:126-144` carga el config **antes** de despachar cualquier comando, y si falta lo crea y sale con código 10. El subcomando nuevo hereda ese comportamiento, lo que importa para el arranque en Docker.

### D12 — Manejo de errores por ramo

Un ramo que falla no debe abortar el resto (user story 21). Esto **contradice el comportamiento actual de `sync`**, que usa `?` en `sync_module` y aborta la corrida entera ante un fallo de página — una rough edge ya documentada en `AGENTS.md:294-308`. El flow nuevo no la replica.

### D13 — Fidelidad de campos: Canvas → VTODO → caldir → vassago → Google

El `VTODO` se emite **canónico y correcto** según el RFC, y no se recorta para acomodar al eslabón más pobre de la cadena. Pero hay que saber, con evidencia, hasta dónde llega cada campo, para no prometer un comportamiento que la API de destino no ofrece.

**Orden real del pipeline en el despliegue.** u_crawler escribe `<caldir_root>/<ramo>/deadlines/assignment-<id>.ics`; `vassago` (`merge-ucrawler.py`) lo funde en el archivo canónico respaldado por CalDAV, y `bridge-vtodo.py` proyecta un espejo para Google Tasks; recién ahí `caldir sync` empuja a la API de Google Tasks. La columna "caldir" de la matriz describe el round trip local y lo que el proveedor pone en el cuerpo JSON de `tasks.insert` / `tasks.patch`.

| Campo Canvas | Propiedad `VTODO` | caldir (round trip local) | vassago (bucket) | Google Tasks |
|---|---|---|---|---|
| `Course.name` + `assignment.name` | `SUMMARY` | parseado a `Todo.summary`, re-serializado | `COMMON_FIELDS` — bidireccional | **llega**: `title`, verbatim, ≤1024 caracteres |
| rótulo + `unlock_at`/`due_at` legibles + `html_url` | `DESCRIPTION` | parseado a `Todo.description`, re-serializado; multilínea sobrevive como `\n` escapado + folding | `COMMON_FIELDS` — bidireccional, y **una de las cinco entradas de `shared_signature`** | **llega**: `notes`, verbatim, ≤8192 caracteres. El manejo de saltos de línea y la linkificación de URLs **no están documentados** por Google — no verificado |
| `assignment.html_url` | `URL` | parseado a `Todo.url`, re-serializado byte a byte | `RICH_FIELDS` — canónico → espejo Google, nunca de vuelta | **no llega**: `to_google.rs:17-45` no tiene campo para él, y el test `url_survives_the_push_and_is_never_appended_to_notes` fija que tampoco se anexa a `notes`. En la API pública `links[]` es *output only* |
| `assignment.unlock_at` (si `< due_at`) | `DTSTART` | parseado a `Todo.start`, re-serializado byte a byte | `RICH_FIELDS` — canónico → espejo Google; nunca normalizado, nunca leído de vuelta | **no llega**: no existe campo de inicio en el recurso `Task`. `dtstart_survives_the_push_and_is_never_folded_into_due` (`policy.rs:285`) fija que caldir tampoco lo dobla dentro de `due` |
| `assignment.due_at` | `DUE` (`DATE-TIME` UTC) | parseado a `EventTime::DateTimeUtc`, re-serializado byte a byte | `COMMON_FIELDS` — bidireccional, con normalización a solo fecha (`DUE-DATE:YYYYMMDD`) para comparar | **llega con pérdida**: `due: "YYYY-MM-DDT00:00:00.000Z"`. La hora se destruye y la **fecha se calcula en `chrono::Local` de la máquina que sincroniza** (`create_event.rs:72`); el eco de solo-fecha de Google se reescribe en el archivo local |
| `points_possible`, `omit_from_final_grade`, `submission_types` | `PRIORITY` | preservado solo localmente | `RICH_FIELDS` | **no llega**: la API no modela prioridad |
| submission (`submitted_at` / `graded`) | `STATUS` (`COMPLETED` o ausente) | `TodoStatus`; `COMPLETED`/`NEEDS-ACTION` fieles | `COMMON_FIELDS` — bidireccional; `merge-ucrawler.py` además protege el estado del usuario (`USER_STATE_FIELDS`) | **llega**: `status`, que solo tiene dos valores |

Consecuencias de diseño que salen directo de la matriz:

1. **El enlace se repite en `DESCRIPTION`** (D3). `URL` no viaja; `notes` sí. Es la única vía.
2. **La hora de entrega se escribe en el texto** (D3, línea 2). `DUE` llega a Google como día, calculado en el huso de la máquina que sincroniza. La hora exacta sobrevive en las notas o no sobrevive.
3. **El `DESCRIPTION` se mantiene corto, plano y estable.** Al ser bidireccional y alimentar `shared_signature`, si Google llegara a normalizar el texto de `notes` ambos lados quedarían "cambiados" con firmas distintas y `bridge-vtodo.py` levantaría un `CONFLICT` que bloquea la publicación hasta intervención humana. La mitigación adoptada es no darle a Google nada que normalizar. Por eso el HTML de `assignment.description` **no** entra en el `DESCRIPTION`: es exactamente el blob grande que dispararía ese conflicto.
4. **`DTSTART` produce un push loop benigno** en `bridge-vtodo.py` (está en `RICH_FIELDS`, canónico → Google), igual que ya ocurre con `PRIORITY` y `URL`. Sin conflicto.

## Testing Decisions

**Qué hace un buen test acá:** dado un conjunto de assignments y submissions de entrada más un estado previo, afirmar sobre el **plan devuelto** — qué se escribe, qué se borra, qué se deja intacto. Nunca afirmar sobre cómo el planner llegó ahí, ni sobre estructuras internas.

**Prior art en el repo:** el estilo dominante es `#[cfg(test)] mod tests` al pie del módulo, sobre funciones puras — ver `links.rs`, `fsutil.rs`, `state.rs`, `download.rs` (`state_key`, derivación de extensión), `zoom/app_conf.rs`. `canvas.rs` testea construcción de URLs y un loop de paginación **simulado que nunca toca la red**. Ese es el patrón a seguir.

Casos que deben estar cubiertos:

- Assignment sin `due_at` → no genera componentes.
- Assignment sin `unlock_at` → genera VTODO, no genera VEVENT.
- `unlock_at` posterior a `due_at` → no genera VEVENT (dato inconsistente de Canvas).
- `points_possible > 0` y no omitido → `PRIORITY:1`.
- `points_possible == 0` → prioridad del bucket inferior.
- `omit_from_final_grade == true` con puntos > 0 → **no** es prioridad máxima.
- Submission con `submitted_at` → `STATUS:COMPLETED`.
- Submission `graded` sin `submitted_at` → `STATUS:COMPLETED`.
- Sin cambios respecto al estado previo → plan vacío (**el test más importante**: es la garantía de user story 14).
- Cambio de `due_at` → un write y, si el nombre de archivo cambió, un delete del anterior.
- Assignment presente en el estado pero ausente de Canvas → delete.
- Curso en `ignored_courses` → no se planifica nada.

**Fuera del alcance de los tests:** el ejecutor de I/O y las llamadas HTTP. El repo no tiene mock server y este spec no lo agrega. `AGENTS.md:311-320` ya identifica ese test de integración como deuda pendiente — sigue siéndolo, y es un esfuerzo aparte.

## Out of Scope

- **La implementación.** Este documento es el destino; construirlo es un esfuerzo separado.
- **Clases síncronas recurrentes (RRULE).** Solo se diseña la costura: el layout contempla una tercera semántica desde el día uno, pero no se implementa. Son otra fuente de datos (`calendar_events`, no `assignments`) con su propio problema de RRULE y timezones.
- **Google Tasks como backend.** Confirmado que era analogía ("que se comporten como Google Tasks"), no un destino real. u_crawler no habla con Google.
- **Escribir una fecha de inicio y un deadline separados en Google Tasks.** La API pública no lo permite (ver «Límite verificado: Google Tasks» en Further Notes), y no se usa ninguna API interna ni no documentada para conseguirlo.
- **Meter el HTML de `assignment.description` en el `DESCRIPTION` del `VTODO`** (D13, punto 3).
- **Una zona horaria de presentación configurable.** Sería configuración nueva y global para una sola feature, y `AGENTS.md` prohíbe configuración inerte. D9 se mantiene.
- **Que u_crawler escriba por CalDAV.** caldir se encarga (D8).
- **Ponderación por peso de grupo de assignments** (D6).
- **Prioridad dinámica por proximidad del deadline** — rechazada por contradecir D5.
- **Mock server HTTP** para los tests de red.
- **Arreglar la rough edge de `sync`** que aborta ante un fallo de página. El flow nuevo no la hereda, pero el viejo no se toca.

## Further Notes

### Hallazgos del spike (T1) — resuelto

Verificado contra un Radicale real en el homeserver, 2026-08-23. **El riesgo que bloqueaba
este spec está cerrado: D3 se mantiene.**

| Pregunta | Hallazgo |
|---|---|
| ¿Un `VTODO` sobrevive `archivo → push → Radicale → pull → archivo`? | **Sí.** El formato funciona; los campos vuelven intactos. |
| ¿Qué nombre de archivo le da caldir a un `VTODO` sin `DTSTART`? | **Ninguno: respeta el que se le da.** caldir conservó el nombre elegido a mano. |
| ¿Borrar un archivo local propaga el borrado al servidor en `push`? | **Sí.** D8 confirmado: u_crawler borra archivos y nunca habla CalDAV. |
| ¿Qué hace caldir cuando cambia la fecha de un componente? | **No se probó.** Ver abajo por qué dejó de importar. |

**Consecuencia de diseño — el nombre de archivo lo elige u_crawler.** Como caldir respeta el
nombre dado, el archivo se nombra a partir del **UID**, que se deriva del id del assignment en
Canvas y por lo tanto es estable frente a cambios de fecha y de título. El nombre
`{ISO-datetime}__{slug}.ics` que caldir usa *cuando él crea el archivo* no aplica acá.

Esto **elimina** el caso filoso del ticket 06: si la ruta no depende de la fecha, mover un
deadline reescribe el mismo archivo y no puede quedar un duplicado viejo. La pregunta sin
responder de la tabla deja de estar en el camino crítico.

**Riesgo residual, a verificar en la puerta manual del ticket 05/06:** que caldir suba como
*update* un archivo modificado que conserva su nombre. Es el caso de sincronización más básico
que existe y casi con certeza funciona, pero no se observó directamente.

### Límite verificado: Google Tasks no permite inicio y deadline separados

Investigación de respaldo: `.scratch/calendar-rich-vtodo/research/` (01 RFC 5545, 02 Google Tasks API, 03 `caldir@vtodo-support`, 04 `vassago`), todas las fuentes consultadas el **2026-08-28**.

**La afirmación, sin adornos: una fecha de inicio y un deadline separados no son escribibles a través de la API pública de Google Tasks.** El recurso `Task` de la v1 expone **un solo campo de fecha escribible, `due`**, que registra **solo el día** y está documentado como la fecha *programada* de la tarea, con la frase explícita **«It doesn't represent the deadline of the task»** (Discovery Document en vivo `https://tasks.googleapis.com/$discovery/rest?version=v1`, revisión **`20260825`**, coincidente palabra por palabra con la página de referencia REST). En el esquema **no existen** `startDate`, `start`, `deadline`, `scheduledDate` ni `taskDate`: cero coincidencias sobre el conjunto completo de propiedades del recurso.

Esto **no es un bug de este repositorio ni algo pendiente de arreglar acá**, y no hay ningún test verde que lo cubra: es una asimetría real entre capas, y cada tramo está verificado contra su fuente primaria.

| Capa | Qué permite | Evidencia |
|---|---|---|
| RFC 5545 | `DTSTART` y `DUE` juntos en un `VTODO`, con el mismo tipo de valor y `DUE` **estrictamente** posterior | §3.6.2 (`todoprop`), §3.8.2.3 |
| `caldir@vtodo-support` | parsea `DTSTART` a `Todo.start` y lo re-serializa byte a byte, pero **nunca lo envía a Google** | `to_google.rs:17-45`, `policy.rs:480`, test `dtstart_survives_the_push_and_is_never_folded_into_due` |
| `vassago` | `DTSTART` es `RICH_FIELDS`: se copia al espejo de Google, nunca se normaliza ni se lee de vuelta | `bridge-vtodo.py:39-63`, `:327-372` |
| API pública Google Tasks v1 | **un** campo de fecha escribible, `due`, solo día, documentado como fecha *programada* y explícitamente *"It doesn't represent the deadline of the task"* | Discovery Document rev. `20260825`; referencia REST, última actualización 2026-02-24 UTC |
| UI de Google Tasks / Calendar | sí muestra **"Start date and time"** y **"Deadline"** como campos separados | `support.google.com/tasks/answer/9901136`, `support.google.com/tasks/answer/7675838` |

Las tres capas, separadas con cuidado porque se confunden fácil:

- **(a) API pública documentada.** Un solo campo de fecha escribible, `due`, de granularidad de día, documentado como fecha *programada* y explícitamente *no* como deadline. Sin hora de inicio, sin duración, sin deadline. La hora se descarta al escribir y no se puede leer ni escribir por la API. Las release notes de la API no registran ningún campo de fecha agregado desde la GA de 2018.
- **(b) UI de usuario final.** Sí ofrece "Start date and time" (fecha + hora + duración) y "Deadline" (solo fecha) como dos campos independientes, según las páginas de ayuda citadas. El modelo de la UI es más rico que el de la API en tres cosas a la vez: hora de inicio, duración y deadline separado.
- **(c) API interna / no documentada.** Los clientes propios de Google evidentemente persisten inicio, duración y deadline, así que algo no público debe transportarlos. **Su existencia es inferible; no se documenta acá cómo usarla, no está cubierta por ningún contrato publicado ni por los scopes OAuth publicados, y no se recomienda ni se depende de ella.** Este proyecto usa exclusivamente la API pública, vía `caldir`.

**Lo que esto significa en concreto para el usuario.** El objetivo visual de ver, dentro de **una sola Google Task**, que algo "empieza el 9 y vence el 16" **no es alcanzable** desde este pipeline: la tarea aparece en un único día, en la fila de todo el día del `due` que se le escribió, y no hay forma de escribir el otro extremo por la API. La fecha de apertura sobrevive en dos lugares y solo en dos: en el `DTSTART` del `.ics`, que llega hasta el archivo local y hasta un cliente CalDAV que sí respete `DTSTART`, y como texto legible en las notas de la tarea. Eso es todo lo que hay, y está documentado así a propósito.

### Docker

`chromiumoxide` es dependencia **git pinneada** (el build necesita red + git), y musl + chromiumoxide + rustls es la esquina frágil de la matriz de `ci.yml`. Pero **este flow no necesita navegador headless en absoluto**.

Eso abre la posibilidad de un feature flag que excluya `chromiumoxide` del build de calendario, dando una imagen chica para el cron. **No está verificado** — es una asunción, no una decisión, y le corresponde su propio ticket.

Cualquier crate ICS que se agregue debe verificarse contra el target musl de `build-check` **antes de taggear**: `release.yml` no tiene guardia independiente.

### Referencias

- [caldir](https://github.com/t4t5/caldir) · [caldir.org](https://caldir.org/)
- [Radicale #101 — REPORT con VTODO devuelve VEVENT y viceversa](https://github.com/Kozea/Radicale/issues/101)
- [RFC 5545 — iCalendar](https://www.rfc-editor.org/rfc/rfc5545) (§3.1 folding, §3.3.11 `TEXT`, §3.6.2 `todoprop`, §3.8.2.2 `DTEND`, §3.8.2.3 `DUE`)
- [Google Tasks API v1 — recurso `tasks`](https://developers.google.com/workspace/tasks/reference/rest/v1/tasks) · Discovery Document en vivo `https://tasks.googleapis.com/$discovery/rest?version=v1` (rev. `20260825`)
- [Google Tasks API — release notes](https://developers.google.com/workspace/tasks/release-notes) (sin campos de fecha nuevos desde la GA de 2018)
- [Create & manage tasks in Google Calendar](https://support.google.com/tasks/answer/9901136) · [Add or edit a task](https://support.google.com/tasks/answer/7675838) — los campos "Start date and time" y "Deadline" de la UI
