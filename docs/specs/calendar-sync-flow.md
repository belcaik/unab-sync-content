# Spec: flow de sincronización de calendario

**Estado:** propuesto — bloqueado por T1 (round-trip de caldir)
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

- Un **`VTODO`** con `DUE` = `due_at`, `PRIORITY` según D6, `SUMMARY` = nombre, `URL` = `html_url`, `STATUS` según D5.
- Un **`VEVENT`** con `DTSTART` = `unlock_at` y `DTEND` = `due_at`, **solo si `unlock_at` existe y es anterior a `due_at`**. Sin `unlock_at` no hay ventana que representar y no se emite nada.

Un assignment sin `due_at` no genera componentes: no hay nada que ubicar en el tiempo.

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
- **Que u_crawler escriba por CalDAV.** caldir se encarga (D8).
- **Ponderación por peso de grupo de assignments** (D6).
- **Prioridad dinámica por proximidad del deadline** — rechazada por contradecir D5.
- **Mock server HTTP** para los tests de red.
- **Arreglar la rough edge de `sync`** que aborta ante un fallo de página. El flow nuevo no la hereda, pero el viejo no se toca.

## Further Notes

### Riesgo abierto que bloquea este spec

**caldir no documenta soporte de VTODO en ningún lado.** Ni el README, ni caldir.org, ni la búsqueda: todo habla de *events*. Y VTODO es el componente central de D3.

Tampoco documenta:
- Qué pasa cuando cambia la fecha de un evento. El nombre de archivo es `{ISO-datetime}__{slug}.ics`, **derivado del start** — un cambio de fecha cambia el nombre, con riesgo de duplicado en vez de update.
- Cómo maneja UIDs: si reconcilia por el UID interno del archivo o por el nombre.
- Qué `DTSTART` usaría para un VTODO, que no tiene `DTSTART` obligatorio.

Esto no se resuelve discutiendo. **T1** lo resuelve empíricamente: escribir un VTODO a mano en un caldir, hacer `push` a un Radicale de prueba, `pull`, y verificar que `UID`, `DUE`, `PRIORITY` y `STATUS` vuelven intactos. Si no vuelven, **D3 se revisa antes de escribir código**.

Del lado del cliente no hay riesgo: Thunderbird soporta VTODO bien vía CalDAV. El eslabón dudoso está en el medio.

### Docker

`chromiumoxide` es dependencia **git pinneada** (el build necesita red + git), y musl + chromiumoxide + rustls es la esquina frágil de la matriz de `ci.yml`. Pero **este flow no necesita navegador headless en absoluto**.

Eso abre la posibilidad de un feature flag que excluya `chromiumoxide` del build de calendario, dando una imagen chica para el cron. **No está verificado** — es una asunción, no una decisión, y le corresponde su propio ticket.

Cualquier crate ICS que se agregue debe verificarse contra el target musl de `build-check` **antes de taggear**: `release.yml` no tiene guardia independiente.

### Referencias

- [caldir](https://github.com/t4t5/caldir) · [caldir.org](https://caldir.org/)
- [Radicale #101 — REPORT con VTODO devuelve VEVENT y viceversa](https://github.com/Kozea/Radicale/issues/101)
