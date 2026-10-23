# Spec: layout v2 de la biblioteca + migración retroactiva

> Fuente de verdad para los tickets T0–T7 del tablero herdr. Si un ticket contradice esto, gana este archivo; si este archivo está mal, corrígelo en el mismo PR.

## Contexto

`u_crawler` descarga bien, pero el árbol que produce es difícil de usar para una persona y para un agente:

- Las carpetas de ramo repiten el período y el NRC dos veces: `202615_2690_MINERIA_DE_DATOS_ATDF105_202615_2690_EL_ON`. Pasa porque `fsutil::course_dir` concatena `name` y `course_code`, y en UNAB los dos ya traen esos datos.
- Las grabaciones viven en `Canvas/Zoom/<course_id>/`, con ids numéricos, y algunos archivos no nombran el ramo (`Sesion_sincronica_1.mp4`).
- `Modules/<module_id>_<Nombre>` ordena por id y no por la posición del módulo. Las tareas están mezcladas dentro de los módulos.

Hay 15 GB ya descargados (2.621 archivos, 135 mp4) que **ya no se pueden volver a descargar**. Por eso la migración solo puede renombrar o mover, nunca borrar ni pisar nada.

**Destino final:** imagen Docker con cron, escribiendo directo en el volumen del NAS, que es el vault real. El PC lo consume montando la unidad. La herramienta es open source, así que el migrador tiene que descubrir cualquier biblioteca con el layout viejo y no puede depender de un plan hecho a mano.

## Decisiones tomadas

| Tema | Decisión |
|---|---|
| Primer nivel | Ramos al mismo nivel, sin agrupar por período. El período queda en los metadatos. |
| Estilo | kebab-case ASCII en minúsculas para **todo**, incluidos los adjuntos. |
| Slug del ramo | Plantilla en config: `[layout] course_dir = "{code}-{name}"`, más las regex opcionales `code_pattern` y `name_strip_pattern`. El valor por defecto es genérico y la plantilla de UNAB va como ejemplo en `assets/config.toml`. El sufijo `-s<nrc>` solo se agrega cuando dos ramos quedarían con el mismo nombre (ATDF106 2520/2524). |
| Grabaciones | Dentro del ramo: `grabaciones/<YYYY-MM-DD>_<tema>[-parte-N].mp4`. Se quita el nombre del ramo del tema y se usa `clase` cuando no queda nada. |
| Interior del ramo | Organizado por tipo, con índice (ver árbol más abajo). |
| Tareas | Un archivo por tarea en `tareas/`, con frontmatter. El README del módulo las enlaza. |
| Fuente de metadatos | Por defecto offline: ids desde `announcements/index.json` (`courses/<id>`), nombres desde las carpetas, fechas desde los archivos. Si hay token y red, Canvas aporta el orden real de los módulos y el nombre oficial. |
| Colisiones | Se compara tamaño y BLAKE3. Si el contenido es idéntico, el origen se queda donde está y se reporta como duplicado. Si es distinto, se guarda como `nombre.conflicto-<hash8>.ext`. Nunca se pisa ni se borra. |
| Layout viejo en `sync` | **Migra siempre de forma automática**, con una advertencia `warn!` en el log. |
| Archivos ajenos | Se mueven junto con la carpeta que les corresponde en el layout nuevo. |
| `.md` existentes | Se reescriben para agregar frontmatter y enlaces locales, solo en los `.md` generados por u_crawler. El original se respalda en `.u_crawler/backup/<ts>/`. Los binarios nunca se abren para escribir. |
| Calendario | `caldir_root/<ramo>/` adopta el slug nuevo mediante rename. Los UID no cambian. |
| Extras para IA | Frontmatter YAML, enlaces Canvas → locales, `README.md` y `curso.json` por ramo, `INDEX.md` y `AGENTS.md` en la raíz. |
| Transcripción | **Fase 2**: `u_crawler transcribe` en el PC con la GPU (whisper `large-v3`, `es`), fuera del contenedor cron. |
| Orquestación | Tablero herdr (skill `herdr-projects`). |
| Release | `feat!:` con el footer `BREAKING CHANGE:` y un salto de versión mayor. |

## Layout v2

```
<download_root>/
  INDEX.md                 # ramos, período, próximas entregas (regenerado)
  AGENTS.md                # cómo navegar el árbol (estático, versión de layout)
  .u_crawler/
    library.json           # { "layout_version": 2 }
    migrations/<ts>.jsonl  # diario: una línea por operación, reanudable/deshacible
    backup/<ts>/...        # originales de .md reescritos
  atdf105-mineria-de-datos/
    README.md              # índice: módulos, tareas+fechas, anuncios, grabaciones
    curso.json             # canvas_id, course_code, name, término, sección, zoom ids
    modulos/
      01-informacion-general/
        README.md          # enlaza páginas y tareas del módulo
        01-informacion-general.md
        adjuntos/guia-1.pdf
    tareas/semana-10-formativa-6.md
    anuncios/2026-06-17_bienvenidos-al-curso.md
    anuncios/adjuntos/coder-s3.zip
    anuncios/index.json
    grabaciones/2026-06-18_sesion-sincronica-1.mp4
    .u_crawler/state.json  # mismo esquema por ids; se agregan las claves zoom:<file_id>
```

Los números de módulo salen de la `position` de Canvas cuando está disponible. Sin red, se usa el orden por `module_id`. Los adjuntos de los anuncios conservan el mismo espacio de nombres de estado `file:{id}`.

## Arquitectura (archivos clave)

- **Nuevo `src/layout.rs`**: funciones **puras** que convierten metadatos en rutas: `slugify` (kebab-case sobre `deunicode`, reutilizando `fsutil::ascii_skeleton`), `course_slug(template, patterns, course)`, `module_dir`, `page_file`, `assignment_file`, `announcement_file`, `recording_file(start, topic, part, course_name)`, y `disambiguate` para el sufijo `-s<nrc>`. Es la única fuente de verdad de las rutas. `fsutil::course_dir` y `sanitize_component` pasan a ser envoltorios o se eliminan.
- **Nuevo `src/migrate/`**:
  - `detect.rs`: reconoce el layout v1. Sus señales son la carpeta `Zoom/` con subcarpetas numéricas, `Modules/<id>_`, `NN-ASSIGN-` y la ausencia de `library.json`.
  - `plan.rs`: planificador **puro**. Recibe un inventario del árbol (más metadatos opcionales de Canvas) y devuelve un `Plan { moves, edits, duplicates, conflicts, unresolved }`.
  - `apply.rs`: ejecutor. Escribe primero la línea del diario y después ejecuta `rename(2)`. Solo trabaja dentro del mismo sistema de archivos y se detiene si `rename` devuelve `EXDEV`. Reanuda desde el diario y revierte con `--undo`. Para el BLAKE3 usa la crate `blake3`.
  - `rewrite.rs`: inserta el frontmatter y reescribe los enlaces de los `.md` generados. Escribe siempre con un rename atómico (`fsutil::atomic_write`), nunca en el mismo archivo, para que un clon con hardlinks (`cp -al`) no altere el original.
- **Escritores que cambian para usar `layout.rs`**:
  - `src/syncer.rs:246,342` (Modules/Attachments). Las tareas pasan a `tareas/`.
  - `src/announcements.rs:137-138`.
  - `src/zoom/headless.rs:361`: la salida pasa a `<ramo>/grabaciones/`. La deduplicación usa `state` (`zoom:<file_id>`) **y** el nombre calculado, porque el migrador no puede conocer los ids de grabación sin conexión.
  - `src/calendar.rs`, que usa `fsutil::course_dir`.
- **Nuevo `src/index.rs`**: renderizado puro de `README.md` por ramo y módulo, `curso.json`, `INDEX.md` y `AGENTS.md`. Lo llaman `sync` y `migrate`, este último sin red.
- **Nuevo `src/frontmatter.rs` + `src/links.rs`**: renderiza YAML (`ramo`, `tipo`, `canvas_id`, `url`, `publicado`, `vence`, `disponible`). Reescribe `canvas.../files/<id>` a la ruta relativa local usando el mapa `file:<id>` → ruta. Ya existe el regex de `/files/<id>` en `syncer.rs:458`, y hay que reutilizarlo.
- **CLI (`main.rs`)**: `migrate [--root P] [--dry-run] [--undo <ts>] [--canvas]`. `sync`, `announcements`, `calendar` y `zoom flow` llaman a `migrate::ensure_v2` al iniciar: detectan, planifican, aplican y emiten `warn!` con un resumen.
- **Config**: `[layout] course_dir`, `code_pattern` y `name_strip_pattern`, conectados en `config.rs` y documentados en `assets/config.toml` (regla del repo: ninguna clave inerte).
- **Docs**: actualizar `AGENTS.md` (layout de descargas y calendario, CLI, nueva sección "Migración") y el README con una sección de breaking change.

### Reglas del migrador (invariantes que se testean)

1. No existe ninguna operación de borrado. Las únicas son `rename`, `mkdir` y la escritura atómica de `.md` con respaldo previo. Las carpetas viejas vacías se dejan en su lugar y se reportan.
2. El plan es determinista: la misma entrada siempre produce el mismo plan, con todo ordenado.
3. Es idempotente: correrlo sobre una biblioteca v2 da un plan vacío.
4. Antes de cada operación se escribe y se sincroniza su línea en el diario. Un corte a mitad de camino se reanuda.
5. `Zoom/<id>` sin un ramo asociado se queda en su lugar, con un `warn!`. Un ramo sin `index.json`, como ATDF106-2520, se migra igual y queda con `canvas_id: null` hasta el próximo `sync`.
6. Todo `rename` es sobre el mismo sistema de archivos. Ante `EXDEV` la ejecución se detiene y no copia.

## Ejecución con agentes (tablero herdr)

Todos los tickets se publican en herdr con `herdr-projects`. Cada agente trabaja en su propio worktree y rama `feat/layout-v2-<ticket>`, y yo hago el merge hacia `feat/layout-v2`. El revisor también cumple el rol de tester: corre `cargo test`, clippy en las dos variantes de features y el skill `code-review` (estándares + spec).

| # | Ticket | Depende de | Coder | Reviewer/Tester |
|---|---|---|---|---|
| T0 | Spec `docs/specs/layout-v2.md` + `layout.rs` (funciones puras + tests de tabla con los 15 nombres reales) + `[layout]` en config | — | Claude | Codex |
| T1 | Fixture: generador sintético de un árbol v1 (`tests/fixtures/v1/`, sin datos reales) que reproduce secciones duplicadas, `_1`, grabaciones sin nombre de ramo, un ramo sin `index.json`, notas ajenas y un Zoom huérfano | T0 | Grok | Kimi |
| T2 | `migrate::detect` + `plan` (puros) + tests sobre la fixture | T0, T1 | Codex | Claude |
| T3 | `migrate::apply` (diario, reanudación, undo, BLAKE3, EXDEV) + tests de corte simulado | T2 (interfaz `Plan`) | Codex | Antigravity |
| T4 | `frontmatter.rs` + reescritura de enlaces (puros) + `migrate::rewrite` | T0 | Grok | Antigravity |
| T5 | `index.rs`: README por ramo y módulo, `curso.json`, `INDEX.md`, `AGENTS.md` | T0 | Kimi | Gemini |
| T6 | Escritores con el layout nuevo: syncer, announcements, zoom/headless (deduplicación `zoom:`), calendar | T0, T4 | Kimi | Codex |
| T7 | CLI `migrate` + `ensure_v2` en los comandos + docs (`AGENTS.md`, README, `assets/config.toml`) + `feat!` | T2–T6 | Claude | Gemini |

Paralelismo: cuando T0 termina, arrancan a la vez T1, T4 y T5. T2 empieza cuando T1 termina, y T3 y T6 pueden ir en paralelo con T2. T7 cierra.

**Fase 2 (otra tanda):** `u_crawler transcribe` con whisper.cpp o faster-whisper con CUDA. Genera `grabaciones/<mismo-nombre>.md` con frontmatter y timestamps. Es incremental y solo procesa los mp4 que todavía no tienen su `.md`.

## Verificación

1. `cargo fmt --all`, `cargo clippy --all-targets --all-features --locked -D warnings`, lo mismo con `--no-default-features`, y `cargo test` en las dos variantes.
2. Sobre la fixture: el plan coincide con un golden snapshot, una segunda corrida da un plan vacío, `--undo` devuelve el árbol byte por byte (comparando los hashes de un manifiesto) y un corte a mitad se reanuda.
3. **Sandbox nuevo:** `download_root` apunta a `~/Dev/u_crawler-sandbox/`, se hace un `sync` desde cero con v1 (commit actual), después `sync` con v2, que debe migrar sola. Se revisa que no vuelva a descargar nada (los contadores de HTTP en el log) y que Obsidian abra la carpeta y muestre las propiedades.
4. **Biblioteca real sin riesgo:** se clona el vault del NAS con hardlinks (`cp -al`, mismo FS, sin costo de espacio), se corre `u_crawler migrate --root <clon> --dry-run`, se revisa el reporte y luego se aplica sobre el clon. Hay que comprobar que el original no cambió (manifiesto de hashes antes y después) y que `find` en el clon da el mismo número de archivos.
5. `caldir push` en modo prueba después del rename de `caldir_root`: confirmar que no hay borrados o creaciones de UID en Radicale.
6. Solo entonces se publica la imagen Docker y se deja que el cron migre el NAS real.
