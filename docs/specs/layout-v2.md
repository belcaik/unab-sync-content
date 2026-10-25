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
| Slug del ramo | Plantilla en config: `[layout] course_dir = "{code}-{name}"`, más las regex opcionales `code_pattern`, `name_strip_pattern` y `section_pattern`. El valor por defecto es genérico y la plantilla de UNAB va como ejemplo en `assets/config.toml`. El sufijo `-s<nrc>` solo se agrega cuando dos ramos quedarían con el mismo nombre (ATDF106 2520/2524); el NRC lo extrae `section_pattern`. |
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

- **Nuevo `src/layout.rs`**: funciones **puras** que convierten metadatos en rutas: `slugify` (kebab-case sobre `deunicode`, reutilizando `fsutil::ascii_skeleton`), `course_slug(template, patterns, course)`, `module_dir`, `page_file`, `assignment_file`, `announcement_file`, `recording_file(start, topic, part, course_name)`, y `disambiguate` para el sufijo `-s<nrc>`. Es la única fuente de verdad de las rutas. La API exacta está en [API de `layout.rs`](#api-de-layoutrs). `fsutil::course_dir` y `sanitize_component` pasan a ser envoltorios o se eliminan.
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
- **Config**: `[layout] course_dir`, `code_pattern`, `name_strip_pattern` y `section_pattern`, conectados en `config.rs` y documentados en `assets/config.toml` (regla del repo: ninguna clave inerte).
- **Docs**: actualizar `AGENTS.md` (layout de descargas y calendario, CLI, nueva sección "Migración") y el README con una sección de breaking change.

### API de `layout.rs`

Implementada en T0. Todo es puro (sin E/S) y devuelve **nombres relativos**: quien llama arma la ruta con `root.join(ramo).join(layout::MODULES_DIR).join(layout::module_dir(..))`. T2–T6 dependen de estos nombres y firmas; cambiarlos es cambiar este spec.

**Constantes**

| Constante | Valor |
|---|---|
| `LAYOUT_VERSION` | `2` |
| `META_DIR` | `.u_crawler` |
| `MODULES_DIR` / `ASSIGNMENTS_DIR` / `ANNOUNCEMENTS_DIR` / `RECORDINGS_DIR` / `ATTACHMENTS_DIR` | `modulos` / `tareas` / `anuncios` / `grabaciones` / `adjuntos` |
| `UNTITLED` | `sin-titulo` (página, tarea, anuncio, módulo o adjunto sin nombre utilizable) |
| `DEFAULT_RECORDING_TOPIC` | `clase` |
| `MAX_SLUG_LEN` | `100` bytes; se corta en un `-` si hay uno en la segunda mitad |

**Ramos**

- `CourseMeta { id: Option<u64>, name: String, code: Option<String> }`, con `From<&canvas::Course>`. Sin red, el migrador llena lo que pueda.
- `CourseNaming::new(&config::Layout) -> Result<CourseNaming, LayoutError>` compila `[layout]`. Falla con `UnknownPlaceholder`, `UnbalancedBrace` o `InvalidPattern`; `Config::load_or_init` lo llama, así que un `[layout]` inválido es un error de config (`ConfigError::InvalidLayout`) antes de que corra cualquier comando.
- `CourseNaming::short_code(&CourseMeta)`: `code_pattern` sobre `course_code`; grupo 1 o, si no hay grupo, el match completo. Sin patrón o sin match, el código entero.
- `CourseNaming::short_name(&CourseMeta)`: el nombre sin ningún match de `name_strip_pattern`.
- `CourseNaming::section(&CourseMeta) -> Option<String>`: `section_pattern` sobre el código y, si no calza, sobre el nombre; grupo 1 o match completo, en slug.
- `CourseNaming::course_slug(&CourseMeta) -> String`: reemplaza `{code}`, `{name}` e `{id}` por sus slugs y vuelve a pasar `slugify` sobre el resultado, así un `{code}` vacío no deja un `-` colgando. Si no queda nada: `curso-<id>` o `curso`.
- `CourseNaming::course_dirs(existing: &[ExistingCourseDir { dir, course: CourseMeta }], courses: &[CourseMeta]) -> Vec<String>`: el directorio de cada ramo, en el orden de entrada. **Es la que deben usar los escritores y el migrador.** Los nombres son estables en el tiempo: un ramo que ya tiene directorio en la biblioteca lo conserva, se le pase lo que se le pase, y solo un ramo nuevo cuyo nombre ya está ocupado recibe sufijo. Un ramo es dueño de un `ExistingCourseDir` cuando los dos tienen el mismo id de Canvas o, si el directorio no tiene id registrado (`canvas_id: null` tras una migración offline), el mismo `name` y `code`. Los demás ramos pasan por `course_slug` y `disambiguate`, con todos los nombres de `existing` como ocupados. Así, `sync`, que solo lista ramos activos, igual evita los directorios de ramos archivados: un ramo recursado (ATDF105 202515/1033 archivado, 202615/2690 activo) recibe `atdf105-mineria-de-datos-s2690` y el archivado no se renombra. El migrador offline, sin directorios v2 previos, pasa `existing` vacío y todos los ramos a la vez. `existing` sale de los `curso.json` de la biblioteca (T5).
- `disambiguate(taken: &BTreeSet<String>, &[SlugCandidate { slug, section, id }]) -> Vec<String>`: nunca devuelve un nombre de `taken` ni dos nombres iguales. Un slug que no choca con nada queda igual. Uno que choca, con otro candidato o con `taken`, recibe `-s<section>`; si falta la sección, `-c<id>`; si faltan las dos, nada. Lo que siga chocando recibe el primer `-2`, `-3`… libre, en el orden de entrada, saltándose los nombres ocupados o ya producidos. Por eso hay que pasar los candidatos en orden estable (por ejemplo por id).

**Contenido del ramo**

| Función | Resultado |
|---|---|
| `slugify(&str)` | kebab-case ASCII en minúsculas sobre `fsutil::ascii_skeleton`; puede devolver `""` |
| `module_dir(position, name)` | `NN-<slug>` (`01-informacion-general`) |
| `page_file(position, title)` | `NN-<slug>.md`, `NN` = posición del ítem en el módulo |
| `assignment_file(title)` | `<slug>.md` |
| `announcement_file(posted: NaiveDate, title)` | `YYYY-MM-DD_<slug>.md` |
| `attachment_file(original)` | stem en slug + última extensión en minúsculas (`Guía 1.PDF` → `guia-1.pdf`) |
| `recording_file(start: NaiveDate, topic, part: Option<u32>, course_name)` | `YYYY-MM-DD_<tema>[-parte-N].mp4` |

`recording_file` quita del tema la secuencia de palabras más larga que comparte con `course_name`, siempre que tenga al menos dos palabras o sea el nombre completo del ramo (así un `de` suelto no se borra), y repite mientras encuentre otra. Sirve tanto el nombre de Canvas como el nombre de la carpeta v1. Si no queda nada, el tema es `clase`. Si solo quedan números, se les antepone `clase-` (`2025_10_01_202525_1149_MECANICA_1.mp4` sin hermano base → `2025-10-01_clase-1.mp4`). Los formatos de 2025 con el código corto o el NRC dentro del tema (`Clase_FMMA015_1226_18_06_2025`) todavía conservan esos tokens: `recording_file` solo conoce el nombre del ramo. `part` es 1-based y `None` es la primera parte (o la única), que no lleva sufijo.

**Lectura de nombres v1**

- `parse_v1_recording(file_name, siblings: &[&str]) -> Option<V1Recording { date, topic, part }>` separa `YYYY_MM_DD_<tema>[_N].mp4`. En v1, `zoom::headless` agregaba `_N` (desde 1) a la segunda, tercera… copia del mismo nombre, así que `_N` es la parte `N + 1`. Pero `Sesion_sincronica_3` también termina en `_3`: el sufijo solo se interpreta como parte cuando `siblings` contiene el nombre sin sufijo. Devuelve `None` para el fallback `zoom-<meeting id>` (sin fecha) y para lo que no sea `.mp4`.

**Casos de tabla (tests en `src/layout.rs`)**, con la plantilla UNAB comentada en `assets/config.toml`, que el test lee del propio archivo:

| Entrada v1 | Salida v2 |
|---|---|
| `202615_2690_MINERIA_DE_DATOS` + `ATDF105_202615_2690_EL_ON` | `atdf105-mineria-de-datos` |
| ATDF106 secciones 2520 y 2524 | `atdf106-…-s2520`, `atdf106-…-s2524` |
| ATDF105 202515/1033 con directorio, y luego 202615/2690 | el primero conserva `atdf105-mineria-de-datos`; el segundo recibe `atdf105-mineria-de-datos-s2690` |
| `2026_03_11_202605_2333_DISRUPCION_EMPRESARIAL_1.mp4` (existe la versión sin `_1`) | `2026-03-11_clase-parte-2.mp4` |
| `2026_10_14_202625_1108_RESPONSABILIDAD_SOCIAL_SEMANA_3.mp4` | `2026-10-14_semana-3.mp4` |
| `2026_07_02_Sesion_sincronica_3.mp4` | `2026-07-02_sesion-sincronica-3.mp4` |

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
| T0 | Spec `docs/specs/layout-v2.md` + `layout.rs` (funciones puras + tests de tabla con nombres v1 reales) + `[layout]` en config | — | Claude | Codex |
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

## Hallazgos en la biblioteca real (NAS, 2026-10-23)

La biblioteca completa vive en el NAS (`baphomet:/mnt/backup/UNAB`, ext4, x86_64, 4 núcleos, **podman, sin docker**). El PC la monta por sshfs. Tiene 43 GB y 4.693 archivos, distribuidos en 22 ramos y 21 carpetas `Zoom/<id>`.

1. **Hay layouts v1 de distintas épocas.** Los 7 ramos de 2025 (`202515_*`, `202525_*`) **no tienen `announcements/`**, así que su `course_id` no se puede sacar de `index.json`. La fixture (T1) tiene que incluir ramos así.
2. **Por eso, 7 carpetas Zoom no tienen el puente `index.json`** (157107, 157429, 157965, 188678, 188933, 189530, 189943). El planificador (T2) necesita una segunda regla offline para asociarlas: hacer coincidir los tokens de los nombres de las grabaciones con los de la carpeta del ramo. El token más fuerte es el NRC (`1033`, `1226`, `2411`, `1149`, `1332`, `2173`, `2211`). Después vienen el código corto (`FMMA015`, `FMMA115`) y el nombre (`ELEMENTOS_DE_FISICA_Y_MEDICION`). Formatos reales que aparecen:
   - `2025_06_23_202515_1033_ELEMENTOS_DE_FISICA_Y_MEDICION.mp4`
   - `2025_06_18_Clase_FMMA015_1226_18_06_2025.mp4`
   - `2025_10_06_Sesion_01_FMMA115_NRC_1332_0610.mp4`
   - `2025_10_01_202525_1149_MECANICA_1.mp4`

   La asociación solo se acepta cuando apunta a **un único** ramo. Si es ambigua, queda en `unresolved` y no se mueve. Con `--canvas` se puede confirmar con `GET /api/v1/courses/<id>`, que también responde para ramos inactivos. Ese endpoint no pasa por la lista de activos.
3. **La raíz del vault es un repositorio git** (`UNAB/.git`, 12 GB, 968 archivos versionados, con cambios sin commitear). El migrador **nunca entra a `.git`** ni lo mueve, y lo mismo vale para `.obsidian/` y los archivos sueltos de la raíz (`Pasted image ….png`): todo lo que está fuera de `download_root` es ajeno y se queda en su lugar. Los renames los detecta git (`git status` los muestra como renamed), así que no hace falta `git mv`.
4. **La migración real debe correr en el NAS** (podman o el binario directo sobre ext4), no a través de sshfs. Sobre sshfs, `rename` es remoto, `cp -al` no es fiable y el BLAKE3 obligaría a pasar los 43 GB por la red.
