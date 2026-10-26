# Spec: layout v2 de la biblioteca + migración retroactiva

> Fuente de verdad para los tickets T0–T7 del tablero herdr. Si un ticket contradice esto, gana este archivo; si este archivo está mal, corrígelo en el mismo PR.

## Contexto

`u_crawler` descarga bien, pero el árbol que produce es difícil de usar para una persona y para un agente:

- Las carpetas de ramo repiten el período y el NRC dos veces: `202615_2690_MINERIA_DE_DATOS_ATDF105_202615_2690_EL_ON`. Pasa porque `fsutil::course_dir` concatena `name` y `course_code`, y en UNAB los dos ya traen esos datos.
- Las grabaciones viven en `Canvas/Zoom/<course_id>/`, con ids numéricos, y algunos archivos no nombran el ramo (`Sesion_sincronica_1.mp4`).
- `Modules/<module_id>_<Nombre>` ordena por id y no por la posición del módulo. Las tareas están mezcladas dentro de los módulos.

Hay 15 GB ya descargados (2.621 archivos, 135 mp4) que **ya no se pueden volver a descargar**. Por eso la migración solo puede renombrar o mover, nunca borrar ni pisar un archivo (lo único que se elimina son carpetas v1 que quedaron vacías, con `rmdir`).

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
| Colisiones | Dos ítems **distintos** de Canvas que darían el mismo nombre (dos anuncios con la misma fecha y título, dos tareas con el mismo título) no son una colisión: se numeran `-2`, `-3`… en orden de id (o de `NN` en v1), igual en `sync` y en el migrador. Para dos copias del **mismo** ítem se compara tamaño y BLAKE3. Si el contenido es idéntico, el origen se queda donde está y se reporta como duplicado. Si es distinto, se guarda como `nombre.conflicto-<hash8>.ext`. Nunca se pisa ni se borra un archivo. |
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
  - `apply.rs`: ejecutor. Escribe primero la línea del diario y después ejecuta `rename(2)`. Solo trabaja dentro del mismo sistema de archivos y se detiene si `rename` devuelve `EXDEV`. Reanuda desde el diario y revierte con `--undo`. Para el BLAKE3 usa la crate `blake3`. API en [API de `migrate::apply`](#api-de-migrateapply).
  - `rewrite.rs` (T4): inserta el frontmatter y reescribe los enlaces de los `.md` generados. Antes copia los bytes a `.u_crawler/backup/<ts>/` y después escribe con `fsutil::atomic_write` (inode nuevo), nunca in-place, para que un clon con hardlinks (`cp -al`) no altere el original. La API está en [API de frontmatter y de la reescritura](#api-de-frontmatter-y-de-la-reescritura).
- **Escritores que cambian para usar `layout.rs`**:
  - `src/syncer.rs:246,342` (Modules/Attachments). Las tareas pasan a `tareas/`.
  - `src/announcements.rs:137-138`.
  - `src/zoom/headless.rs:361`: la salida pasa a `<ramo>/grabaciones/`. La deduplicación usa `state` (`zoom:<file_id>`) **y** el nombre calculado, porque el migrador no puede conocer los ids de grabación sin conexión.
  - `src/calendar.rs`, que usa `fsutil::course_dir`.
- **`src/index.rs`** (T5): renderizado puro de `README.md` por ramo y módulo, `curso.json`, `INDEX.md` y `AGENTS.md`. Lo llaman `sync` y `migrate`, este último sin red. La API está en [API de `index.rs`](#api-de-indexrs).
- **`src/frontmatter.rs` + `src/links.rs`** (T4): frontmatter YAML (`ramo`, `tipo`, `canvas_id`, `url`, `publicado`, `vence`, `disponible`) y reescritura de `canvas.../files/<id>` a la ruta relativa local. El regex `(?i)(?:/api/v1)?/files/(\d+)` vive en `links.rs` (`CANVAS_FILE_ID_RE`); `syncer::discover_file_ids` llama a `canvas_file_ids`. La API está en [API de frontmatter y de la reescritura](#api-de-frontmatter-y-de-la-reescritura).
- **CLI (`main.rs`)**: `migrate [--root P] [--dry-run] [--undo <ts>] [--canvas]`. `sync`, `announcements`, `calendar` y `zoom flow` llaman a `migrate::ensure_v2` al iniciar: detectan, planifican, aplican y emiten `warn!` con un resumen. Implementado en T7: ver [API de `migrate::ensure`](#api-de-migrateensure).
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
- `CourseNaming::term(&CourseMeta) -> Option<String>` (T8): `term_pattern` igual que `section`, sin slug. Llena `term` en `curso.json`.
- `CourseNaming::display_name(&CourseMeta) -> String` (T8): `short_name` pasado por `humanize` (`_` → espacio; un nombre sin minúsculas se pasa a título, con las palabras menores del castellano en minúscula y los romanos en mayúscula: `MINERIA_DE_DATOS` → `Mineria de Datos`; uno con minúsculas queda tal cual, con sus acentos). Es el nombre que muestran `README.md`, `INDEX.md` y el `name` de `curso.json`.
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

- `CourseNaming::parse_v1_course_dir(dir) -> CourseMeta` recupera nombre y código de una carpeta v1 (`<nombre>_<código>`, ambos saneados). Corta en el **último** `_` tras el cual `code_pattern` calza justo al inicio del resto, para que una palabra del nombre con forma de código (`PYTHON3`) no se tome por el código. Sin patrón o sin ese corte, todo es nombre y el código queda `None`. El id siempre es `None`. Con la plantilla UNAB, `202615_2690_MINERIA_DE_DATOS_ATDF105_202615_2690_EL_ON` da nombre `202615_2690_MINERIA_DE_DATOS` y código `ATDF105_202615_2690_EL_ON`. El nombre recuperado lleva `_` donde Canvas tenía espacios.

**Casos de tabla (tests en `src/layout.rs`)**, con la plantilla UNAB comentada en `assets/config.toml`, que el test lee del propio archivo:

| Entrada v1 | Salida v2 |
|---|---|
| `202615_2690_MINERIA_DE_DATOS` + `ATDF105_202615_2690_EL_ON` | `atdf105-mineria-de-datos` |
| ATDF106 secciones 2520 y 2524 | `atdf106-…-s2520`, `atdf106-…-s2524` |
| ATDF105 202515/1033 con directorio, y luego 202615/2690 | el primero conserva `atdf105-mineria-de-datos`; el segundo recibe `atdf105-mineria-de-datos-s2690` |
| `2026_03_11_202605_2333_DISRUPCION_EMPRESARIAL_1.mp4` (existe la versión sin `_1`) | `2026-03-11_clase-parte-2.mp4` |
| `2026_10_14_202625_1108_RESPONSABILIDAD_SOCIAL_SEMANA_3.mp4` | `2026-10-14_semana-3.mp4` |
| `2026_07_02_Sesion_sincronica_3.mp4` | `2026-07-02_sesion-sincronica-3.mp4` |

### API de frontmatter y de la reescritura

Implementada en T4. `render`, `parse`, `upsert`, `canvas_file_ids`, `rewrite_canvas_file_links`, `generated_kind` y `apply` son puras. La única escritura es `rewrite_file`. La conectan T6 (los escritores, vía `apply`) y T7 (`migrate::ensure`, vía `rewrite_file`). Cambiar estas firmas es cambiar este spec.

**`frontmatter`**

- `DocKind`: `Page` → `page`, `Assignment` → `tarea`, `Announcement` → `anuncio`, `Recording` → `grabacion`. `as_str` / `parse`. Cualquier otra cadena es `None`.
- `Frontmatter { ramo, tipo, canvas_id: Option<u64>, url, publicado, vence, disponible: Option<String> }`. `ramo` y `tipo` siempre se escriben. Un opcional `None` o en blanco se omite, y un `upsert` posterior borra esa clave si ya estaba.
- `render(&Frontmatter) -> String`: bloque `---`, claves en el orden de arriba, salto de línea final. Las cadenas van entre comillas dobles (`\\`, `\"`, `\n`, `\r`, `\t`, `\uXXXX`). `tipo` es un token sin comillas. `canvas_id` es un entero sin comillas.
- `parse(&str) -> Result<Option<Frontmatter>, FrontmatterError>`. Sin frontmatter, un `---` sin cierre, un `---` inicial que no es una lista de propiedades, o un bloque que no menciona ninguna de nuestras claves: `Ok(None)` (las propiedades del usuario no se leen como nuestras). Si hay alguna de nuestras claves pero falta `ramo` o `tipo`, o es null: `MissingRamo` / `MissingTipo`. `tipo` desconocido: `BadTipo`. `canvas_id` presente y no entero: `BadCanvasId`. Las fechas son texto opaco; no se validan. Una lista de propiedades no tiene una línea en blanco justo después del `---` de apertura, y cada línea es vacía, un comentario (`#`), una clave `nombre:`, o una continuación sangrada que es un ítem (`-`), otra clave o un comentario. html2md convierte `<hr>` en `---` seguido de una línea en blanco: eso es cuerpo, igual que un par de reglas con una línea sangrada que no es YAML. Un bloque apretado que solo trae `url:` sigue siendo frontmatter parcial (`MissingRamo`). Las notas v1 no traen frontmatter.
- `upsert(&str, &Frontmatter) -> String`. Si ya hay una lista de propiedades cerrada al inicio, actualiza nuestras claves en su sitio, descarta duplicados de esas claves y agrega las que falten. Conserva las claves ajenas, los comentarios, una línea en blanco entre claves y el cuerpo (incluido un `---` posterior). Si el `---` inicial no es esa lista, antepone el bloque y deja esos bytes en el cuerpo: una línea `url:` y las líneas sangradas que la siguen no se reemplazan ni se descartan. `upsert(upsert(s)) == upsert(s)`. Un BOM `\u{feff}` se conserva. No usa una crate de YAML: el subconjunto escalar alcanza para no reformatear el documento.

**`links`**

- `canvas_file_ids(&str) -> impl Iterator<Item = u64>`. El mismo regex de arriba. En `/courses/<n>/files/<id>` el id capturado es el del archivo. El primer id de izquierda a derecha gana: si ese id no está en el mapa, el enlace queda igual aunque un id posterior sí esté.
- `rewrite_canvas_file_links(&str, md_path, &BTreeMap<u64, PathBuf>) -> String`. `md_path` y los valores del mapa son relativos a la misma raíz. El destino nuevo es el camino desde el directorio de la nota hasta el archivo, con `/` y los `..` que hagan falta, sin `./`. Se reemplaza el destino entero (se pierde `?verifier=`). Un id ausente del mapa se deja byte a byte.
- Se reescriben enlaces e imágenes en línea (título opcional, destino entre `<>`), autolinks, definiciones de referencia con sangría 0–3, y URLs `http(s)` sueltas (estas dos últimas pasan a `[nombre-de-archivo](ruta)`). No se reescribe el código entre cercas (`` ``` `` o `~~~`, sangría ≤ 3) ni los spans de código en línea, aunque el span no se cierre en la misma línea. Tampoco el HTML crudo (`href`). Un destino que ya es la ruta calculada no se toca. Si la ruta relativa lleva un carácter de control, o un componente que no es un nombre normal (`..` dentro del mapa), el enlace queda igual. Un destino que no es `[A-Za-z0-9._~/-]+` se envuelve en `<>`.

**`migrate::rewrite`**

- `generated_kind(&Path) -> Option<DocKind>` es la compuerta. La ruta es relativa a la raíz de la biblioteca. Rechaza cualquier componente `.git`, `.obsidian` o `.u_crawler`, y cualquier `..` o componente que no sea UTF-8. El primer marcador con cola válida gana:
  - v1 `Modules` + exactamente `[<id>_<nombre>, archivo]`: el directorio es `^[0-9]+_.+`; el archivo es `^[0-9]{2,}-.+\.md`. Si el resto empieza por `ASSIGN-` y el título no está vacío, es tarea; `02-ASSIGN-.md` no es nada; `02-ASSIGN.md` es página.
  - v1 `announcements` + un solo archivo `(?:YYYY-MM-DD_)?[A-Za-z0-9_]+_[0-9]+.md`. `announcements/media/…` no entra.
  - v2 `modulos` + `[NN-kebab, NN-kebab.md]` es página. `README.md` y `sin-titulo.md` no lo son; `01-sin-titulo.md` sí. El kebab es minúsculas: `01-Intro.md` bajo `modulos` no es página.
  - v2 `tareas` + un `.md` kebab es tarea (incluye `tareas/index.md` y `tareas/sin-titulo.md`).
  - v2 `anuncios` o `grabaciones` + `YYYY-MM-DD_kebab.md` es anuncio o grabación. `index.md`, `adjuntos/` y un `.mp4` no entran.
  - Kebab: `[a-z0-9]+(?:-[a-z0-9]+)*`. La fecha es año de 4 dígitos, mes 01–12, día 01–31.
- `apply(&str, &Frontmatter, md_path, &BTreeMap<u64, PathBuf>) -> String`. Separa el frontmatter, reescribe solo el cuerpo y después hace `upsert`. La clave `url` no se trata como enlace de archivo. Una regla horizontal inicial no es frontmatter, así que un enlace de archivo entre las dos reglas también se reescribe.
- `rewrite_file(&Rewrite) -> Result<RewriteOutcome, RewriteError>`. `Rewrite` lleva `root`, `path`, `frontmatter`, `files` y `backup_ts` (un solo segmento `[A-Za-z0-9._-]`, ni `.` ni `..`). Normaliza sin `canonicalize` (un symlink de raíz no se sigue). Recorre cada componente y rechaza symlinks, incluido el de `.part` y el del respaldo, antes de escribir. Exige archivo regular y UTF-8. Si el texto ya coincide, devuelve `Unchanged` y no crea respaldo. Si no, escribe el respaldo en `<root>/.u_crawler/backup/<ts>/<ruta>` con `atomic_write` (copia, no hardlink) y después la nota. Si `<nota>.part` o el `.part` del respaldo ya existe y no es un symlink, `PartExists`: no se crea el respaldo y no se toca ni la nota ni ese archivo. Un `.part` que quedó de un corte también bloquea el reintento hasta que se borre. `atomic_write` no cambia: las descargas siguen truncando su propio `.part`. Si el respaldo ya existe y es igual a los bytes actuales, reanuda y no lo pisa. Si existe y difiere, `ChangedSinceBackup` y no toca ninguno de los dos. Ante cualquier error los bytes de la nota quedan como estaban. Quien llama elige `backup_ts`.

### API de `index.rs`

Implementada en T5. Todo es puro (sin E/S ni red) y devuelve **texto**: quien llama escribe el archivo con `fsutil::atomic_write`. Todos los nombres de archivo y directorio salen de `layout.rs` — este módulo nunca inventa nombres. Los textos están en español (el vault es del usuario) y las fechas se escriben como `YYYY-MM-DD HH:MM UTC`.

**Modelo en memoria.** `LibraryIndex { courses: Vec<CourseIndex> }`, construible tanto desde Canvas (sync, T6) como desde disco (migrate, sin red). `CourseIndex` lleva el `dir` final, un `layout::CourseMeta` (`canvas_id` puede ser `None` tras una migración offline), `term`, `section`, `zoom_ids` (los ids de las carpetas `Zoom/<id>` migradas al ramo) y las listas `modules`, `assignments`, `announcements`, `recordings`. `ModuleIndex` lleva `position`, `name`, `dir`, `pages: Vec<PageIndex>` y `assignments: Vec<AssignmentRef>` (referencias a tareas en `tareas/`). Los constructores `new` derivan el nombre del archivo desde `layout.rs` (`ModuleIndex::new`, `PageIndex::new`, `AssignmentIndex::new`, `AssignmentRef::new`, `AnnouncementIndex::new`); hay `From<&canvas::Assignment> for AssignmentIndex`. `AnnouncementIndex` no tiene constructor sin fecha: el nombre v2 es `YYYY-MM-DD_<slug>.md`, así que quien lee desde disco registra el nombre que encontró. El render siempre ordena (módulos por `position`, ramos por `dir`, tareas por vencimiento — sin fecha al final —, anuncios del más nuevo al más antiguo), así que la misma entrada produce el mismo texto.

**Render**

| Función | Archivo | Contenido |
|---|---|---|
| `render_course_readme(&CourseIndex)` | `<ramo>/README.md` | título + metadatos (`Código`, `Término`, `Sección`, `ID Canvas`), `## Módulos` ordenados, `## Tareas` con `— vence <fecha>`, `## Anuncios recientes` (tope `RECENT_ANNOUNCEMENTS = 10`, el resto se resume como "…y N más"), `## Grabaciones`. Secciones vacías se omiten. |
| `render_module_readme(&ModuleIndex)` | `modulos/NN-x/README.md` | `## Páginas` (enlaces relativos al mismo directorio) y `## Tareas` (`../../tareas/`). |
| `render_curso_json(&CourseIndex)` | `<ramo>/curso.json` | `{ canvas_id, course_code, name, canvas_name, term, section, zoom_ids }`, pretty JSON con `\n` final. `canvas_id: null` cuando el ramo se migró sin red. `name` es el nombre legible (`CourseIndex::title`, T8); `canvas_name` el de Canvas (o el de la carpeta v1), que es la identidad del ramo sin `canvas_id`. Un `curso.json` anterior sin `canvas_name` se lee con `name` como identidad. |
| `render_index_md(&LibraryIndex, today: NaiveDate)` | `<root>/INDEX.md` | `## Ramos` (nombre, código, término) y `## Próximas entregas` (tareas con `vence >= today`, de todas las ramas, más próximas primero). `today` es parámetro para que el render sea puro y testeable. |
| `render_agents_md()` | `<root>/AGENTS.md` | texto estático (incluye `LAYOUT_VERSION`) que explica a un agente cómo navegar el árbol v2; marca la biblioteca como de solo lectura para agentes. |

Los tests comparan el texto completo (snapshots en línea) de cada render, incluido el caso offline `canvas_id: null`, el tope de anuncios, el escape de `[]` en textos de enlace y el filtro de próximas entregas en el límite (`vence` hoy incluye).

### API de `migrate::detect` y `migrate::plan`

Implementada en T2. Todas las rutas son relativas a su raíz y usan `/`.

- **`migrate::scan(root, caldir_root) -> io::Result<Inventory>`** es la única E/S, y es de solo lectura. Lista cada archivo regular con su tamaño, nunca entra a `.git` y no lista symlinks (se quedan donde están). Lee los ids de `announcements/index.json` (`courses/<id>` del `html_url`; si dos registros no coinciden, no hay id) y los `curso.json` de la raíz como `ExistingCourseDir`, y lista las carpetas de primer nivel de `caldir_root`. Falla ante un nombre que no sea UTF-8: es mejor no planificar que planificar media biblioteca.
- **`detect::detect(&Inventory) -> Detected`**: `V2` si existe `.u_crawler/library.json`; si no, `V1(V1Signals { zoom_dirs, module_dirs, assignment_files, announcement_dirs })` con al menos una señal; si no, `Empty`. Una carpeta de primer nivel es un ramo v1 si tiene directamente `state.json`, `Modules/` o `announcements/`.
- **`plan::plan(&Inventory, &CourseNaming, &[CanvasCourse]) -> Plan`**, puro. `CanvasCourse { meta, module_positions }` es opcional (`[]` sin red). `Plan { courses, moves, edits, duplicates, conflicts, unresolved }`; `courses` (carpeta v1 → v2, `CourseMeta` y `zoom_ids`) se agregó a lo que pedía el spec porque `index.rs` lo necesita para `curso.json`. `Display` da el informe por líneas que usa el golden snapshot y que puede mostrar `--dry-run`.

**Correspondencia** (dentro del ramo):

| v1 | v2 |
|---|---|
| `state.json` | `.u_crawler/state.json` |
| `Modules/<id>_<Nombre>/NN-<Título>.md` | `modulos/<module_dir>/<page_file>` (edit `page`) |
| `Modules/<id>_<Nombre>/NN-ASSIGN-<Título>.md` | `tareas/<assignment_file>` (edit `assignment`) |
| `Modules/<id>_<Nombre>/Attachments/<f>` | `modulos/<module_dir>/adjuntos/<attachment_file>` |
| `announcements/<fecha>_<slug>_<id>.md` | `anuncios/<announcement_file>` (edit `announcement`) |
| `announcements/index.json` | `anuncios/index.json` |
| `announcements/media/<f>` | `anuncios/adjuntos/<attachment_file>` |
| `Zoom/<id>/<YYYY_MM_DD_tema[_N]>.mp4` | `grabaciones/<recording_file>`; un `.mp4` sin fecha pasa por `attachment_file` |
| `<caldir_root>/<carpeta v1>/` | `<caldir_root>/<carpeta v2>/` (un solo rename de directorio) |

Lo demás es ajeno y viaja con su carpeta **conservando su nombre**: una nota en un módulo va al módulo v2, un archivo en la raíz del ramo a la raíz del ramo v2, una subcarpeta desconocida de `announcements/` a `anuncios/`. Lo que está fuera de los ramos v1 y de `Zoom/` no se mueve. Solo los archivos generados que se mueven sin colisión reciben un `Edit`; una nota del usuario que se llame `NN-algo.md` se tomaría por página (no hay forma offline de distinguirla) y T4 la respaldaría antes de reescribirla.

**Reglas del plan:**

- **Ramos.** El id sale de `index.json`. Con Canvas, un ramo sin id se empareja si `fsutil::course_dir` del ramo de Canvas da exactamente la carpeta v1 (y solo uno lo hace); entonces nombre, código y posiciones de módulos son los oficiales. Si no, nombre y código salen de `parse_v1_course_dir`. Los directorios salen de `course_dirs`, con los ramos en orden de carpeta v1 y, como `existing`, los `curso.json` más **todo nombre de la raíz que no sea un ramo v1**: un ramo nunca cae dentro de una carpeta ajena. La excepción es una carpeta con `.u_crawler/state.json` y sin `curso.json`, que es una migración cortada: no se reserva, así que volver a planificar manda el ramo de vuelta a ella en lugar de partirlo en dos (los choques de archivo siguen protegidos como colisiones). Aun así, T3 debe reanudar desde el diario y escribir `curso.json` antes de dar la migración por terminada.
- **Módulos.** Número = `position` de Canvas; los módulos que Canvas no lista (o todos, sin red) siguen en orden de `module_id`, después de la mayor posición conocida.
- **Zoom.** `Zoom/<id>` va al ramo con ese id de Canvas. Si no hay, se buscan palabras de los temas de las grabaciones, solo entre los ramos **sin id conocido** (un ramo con otro id no puede ser dueño de la carpeta, que v1 nombró con el id del ramo). Los niveles van por orden: sección (NRC), código corto y todas las palabras del nombre corto. Para la sección no cuentan los números que salen de la fecha de la grabación (`2025`, `0610` para 2025-10-06). Cada nivel que encuentra algo acota los candidatos; si un nivel solo apunta a ramos ya descartados, los nombres se contradicen y la carpeta queda ambigua. Un solo ramo al final se acepta; varios dan `AmbiguousZoom`, ninguno da `OrphanZoom`. Lo que esté en `Zoom/` fuera de una carpeta numérica es `UnrecognizedZoomEntry`. Nada de eso se mueve.
- **Ítems distintos con el mismo nombre (T9).** Antes de buscar colisiones, los orígenes de un mismo destino se agrupan por ítem: un anuncio por el id de su nombre v1 (`…_<id>.md`), una tarea por su carpeta de módulo v1 (`<id>_…`) y su prefijo `NN` (`04-ASSIGN-…`, `06-ASSIGN-…`), porque `NN` es la posición dentro del módulo y se repite entre módulos; los demás archivos no tienen ítem. Todos los ítems de una carpeta (`anuncios/`, `tareas/`) se nombran juntos con `layout::unique_names`, en orden de id o `NN`: cada uno conserva su nombre salvo que un ítem anterior ya lo recibió, y si no recibe el primer `-2`, `-3`… libre (saltándose también los archivos que el plan pone ahí y los que ya existen en disco), con su `Edit`. Es la misma regla que usa `sync` con los ids de Canvas, así que la nota migrada y la que escribe el `sync` siguiente coinciden (para las tareas, mientras el orden de `NN` y el de id coincidan; si no, `sync` reescribe el contenido de cada una desde Canvas). Dos orígenes del mismo ítem (mismo id, o mismo módulo y `NN`) siguen siendo una colisión. Sin red no se distingue una tarea enlazada desde dos módulos de dos tareas con el mismo título, así que se tratan como dos: la copia que `sync` no reescribe queda como sobrante, nunca hay pérdida.
- **Carpetas vacías (T9).** `Plan::prune` lista, ordenadas, las carpetas v1 que la migración vacía: cada carpeta de ramo v1 migrada (o, si su nombre v2 es el mismo, sus `Modules/` y `announcements/`) y, si se migra alguna carpeta de `Zoom/`, todo `Zoom/` (también los `Zoom/<id>` vacíos, que el inventario no ve porque solo lista archivos; un `Zoom/<id>` sin resolver lo mantiene). Se imprime en el informe como `prune`.
- **Colisiones.** Dos orígenes del mismo ítem con el mismo destino, o un destino que ya existe, forman una `Collision { to, from (ordenados), occupied }`. Si todos los tamaños coinciden va a `duplicates` (probable duplicado, que T3 confirma con BLAKE3); si no, a `conflicts` (seguro distinto: `nombre.conflicto-<hash8>.ext`). Si el destino es un directorio existente o queda bajo un archivo, cada origen va a `unresolved` como `DestinationBlocked`. En caldir, si la carpeta v2 ya existe, `CaldirTargetExists`.
- **Vacío.** `Plan::is_empty()` mira `moves`, `edits`, `duplicates` y `conflicts`; `unresolved` es informe. Una biblioteca ya migrada (con su Zoom huérfano) planifica vacío. El test hace la migración con renames sobre la fixture en un tempdir, vuelve a escanear y comprueba plan vacío, el mismo multiconjunto de hashes y los archivos ajenos intactos.

El golden snapshot está en `tests/golden/migrate_plan_v1.txt`; `U_CRAWLER_BLESS=1 cargo test --test migrate_plan` lo regenera.

### API de `migrate::apply`

Implementada en T3. Es síncrona (como `scan`); quien la llama desde `tokio` usa `spawn_blocking`. `Roots { library, caldir }` da las dos raíces; el diario vive en la biblioteca.

- **`apply(&Plan, &Roots, ts) -> Result<Report, ApplyError>`** ejecuta un plan con un diario nuevo `<root>/.u_crawler/migrations/<ts>.jsonl` (`new_timestamp(now)` da `YYYYMMDDTHHMMSSmmmZ`, UTC). Orden: `moves` (biblioteca y después caldir), colisiones (`duplicates` y luego `conflicts`), la poda de `Plan::prune` y por último un `curso.json` por ramo cuyo directorio exista y no lo tenga.
- **Poda (T9).** Para cada carpeta de `Intent::prune` (que está en `begin`; un diario anterior sin ella no poda nada) se recorre el árbol sin seguir symlinks y se elimina con `rmdir(2)` cada carpeta vacía, de abajo hacia arriba, ella incluida, y después cada padre que quede vacío (`Zoom/`). Cada `rmdir` se escribe antes en el diario. `rmdir` no puede borrar una carpeta con contenido, y no hay `remove_dir_all`: una carpeta con cualquier archivo (un duplicado que se quedó, un archivo ajeno, un symlink) se conserva y va a `Report::kept_dirs`; las eliminadas, a `Report::removed_dirs`. Reanudar la repite sin problema. Un plan sin nada que hacer no crea diario. Un `ts` ya usado da `JournalExists`.
- **Diario.** Una línea JSON por entrada, escrita y con `fsync` **antes** de la operación. La primera es `begin` con la intención completa (`Intent { moves, collisions, courses, prune }`); después vienen `mkdir` (un nivel por línea), `rename`, `rmdir` y `write` (el `curso.json`, escrito en un temporal y renombrado), y cierra `done`. Una última línea cortada se ignora al leer, y al reanudar se empieza una línea nueva.
- **Renames.** En Linux, `renameat2(RENAME_NOREPLACE)` (crate `rustix`); en otros sistemas, o si el FS no soporta la bandera, se comprueba el destino antes. `EXDEV` detiene la ejecución con `CrossDevice` sin copiar nada. Una ruta que pasa por un symlink (`Symlink`) o que tiene `..`/`.` (`BadPath`) se rechaza.
- **Colisiones.** Para cada origen, en orden: si el destino está libre, se mueve. Si existe, se compara tamaño + BLAKE3 (crate `blake3`, feature `pure`). Igual: el origen se queda y va a `Report::duplicates`. Distinto: el origen va a `conflict_name(to, hash8)` = `nombre.conflicto-<hash8>.ext` (hash del origen, antes de la última extensión); si ese nombre ya tiene otro contenido, `ConflictNameTaken`. Un `Move` cuyo destino apareció después del plan se trata igual: nunca se pisa.
- **Reanudación: `pending(root)` + `resume(&Roots, ts)`.** `resume` ejecuta la intención guardada en `begin`, **no** un plan nuevo: un plan hecho sobre un árbol a medio migrar no es el mismo (con `X.mp4` ya movido, `X_1.mp4` deja de ser la parte 2 y recibiría otro nombre; un ramo vaciado antes que su `Zoom/` dejaría la carpeta huérfana). Como `rename` es atómico, cada operación registrada ocurrió o no, y el disco lo dice: un origen que ya no está y que el diario renombró se salta.
- **`undo(&Roots, ts) -> UndoReport`** recorre el diario al revés. Cada `rename` se revierte si el destino está y el origen no (antes escribe `revert` en el mismo diario); si están los dos, `UndoBlocked`; si no está ninguno, se reporta en `missing`. Cada `curso.json` escrito se aparta a `.u_crawler/migrations/<ts>.undo/<ruta>` (`set-aside`), nunca se borra. Cada `rmdir` se deshace creando otra vez la carpeta si falta (`UndoReport::dirs_restored`), y cada `mkdir` del diario se deshace con `rmdir` si la carpeta quedó vacía (`dirs_removed`); si tiene algo, se queda (`dirs_left`). Así el árbol vuelve también con sus carpetas. Cierra con `undone`. Se puede repetir tras un corte; sobre un diario ya deshecho no hace nada. Se niega (`NewerJournal`) si hay una migración posterior sin deshacer. Tras `undo`, el árbol (fuera de `.u_crawler/`) vuelve byte a byte al manifiesto de la fixture, con el mismo conjunto de carpetas, y planifica igual que antes.
- **Fuera de alcance.** `apply` no reescribe `.md` (`Plan::edits` es de `migrate::rewrite`, T4) ni escribe `library.json`: lo hace quien orquesta (`ensure_v2`, T7) después de `apply`, la reescritura y los índices. Orden esperado en T7: `pending` → `resume`; luego `scan` → `plan` → `apply` con un `ts` nuevo.
- `apply_with_hook` (oculta en la documentación) llama a un gancho después de sincronizar cada línea y antes de la operación; los tests lo usan para simular un corte en el peor momento.

### Escritores (T6)

Implementado en T6. `sync`, `announcements`, `calendar` y `zoom flow` escriben en v2. El detalle operativo está en `AGENTS.md` ("Directory Layout").

- **Carpeta del ramo** (`src/library.rs`). `library::read(root)` lee los `curso.json` de primer nivel y reserva toda carpeta sin `curso.json`, salvo la que tiene `.u_crawler/state.json` (corrida cortada), igual que `plan.rs`. `library::course_dirs` llama a `CourseNaming::course_dirs` con **todos** los ramos activos ordenados por id, antes de filtrar por `--course-id` o `ignored_courses`, así que una corrida filtrada nombra igual que una completa. Quien crea la carpeta escribe al tiro un `curso.json` mínimo (`library::claim`, nunca pisa uno existente). Al final del `sync` se reescribe completo y se conservan `zoom_ids` y `term` del anterior.
- **Notas.** Páginas, tareas y anuncios pasan por `migrate::rewrite::apply` (frontmatter + enlaces locales). Así un `sync` escribe lo mismo que el migrador y no deshace una nota migrada. Los archivos se descargan antes de escribir la nota que los enlaza. Un archivo que falló conserva el enlace a Canvas. `ramo` es el nombre de la carpeta. Una página lleva `canvas_id` = `page_id` y `url`. Una tarea lleva `canvas_id`, `url`, `vence` y `disponible` (RFC 3339 UTC, segundos). Un anuncio lleva `canvas_id`, `url` y `publicado` = `posted_at`.
- **Choques de nombre en un ramo.** Dos tareas con el mismo título, o dos anuncios con la misma fecha y título: `layout::unique_names` en orden de id de Canvas; cada uno conserva el nombre salvo que un id menor ya lo recibió; si no, recibe el primer `-2`, `-3`… libre (`tarea-2.md`) (T9; antes era `-<id>`). Un nombre solo depende de los ids menores, así que un ítem nuevo nunca renombra uno existente: una `Tarea 2` posterior a dos `Tarea` recibe `tarea-2-2.md`. El migrador usa la misma función. Un anuncio sin `posted_at` se fecha `1970-01-01`, así su nombre es estable.
- **Grabaciones** (`zoom::headless::recording_names` / `plan_recordings`, puras). La fecha sale de `start_time` y, si falta, de `recording_start`. Los segmentos de una reunión que darían el mismo nombre se ordenan por `recording_start` y pasan a `-parte-2`, `-parte-3`… Sin fecha se usa `layout::attachment_file("zoom-<meeting id>.mp4")`, como hace el migrador con el respaldo v1. Clave de estado: `zoom:<id>`, con el `id` de Zoom del archivo si viene (texto o número) y si no `zoom:<meeting id>:<recording_start>`. Se omite la descarga si la clave está en `state.json` sin error **o** si ya existe un archivo con el nombre calculado. En el segundo caso se registra la clave.
- **Índices.** Al final de `sync` (no en `--dry-run`): `README.md` y `curso.json` por ramo, un `README.md` por módulo y `AGENTS.md` en la raíz, escritos solo si cambió el texto. `INDEX.md` se escribe solo en un `sync` sin `--course-id`, porque una corrida filtrada no conoce las entregas de los demás ramos. Los ramos con `curso.json` que no se sincronizaron entran sin tareas.
- **Calendario.** `calendar::plan` recibe `<caldir_root>/<ramo>`, con el mismo nombre que en `download_root`. Los UID, los nombres de archivo y los bytes del `VEVENT` no cambian. Si `caldir_root` todavía tiene la carpeta v1 del ramo (`fsutil::course_dir`, `calendar::unmigrated_caldir`), `calendar` la renombra sin reemplazo antes de escribir (`calendar::adopt_v1_caldir`, T7): normalmente ya lo hizo la migración de la biblioteca, pero un despliegue de solo calendario no tiene biblioteca v1. Si la carpeta v2 también existe, salta el ramo y lo cuenta como fallido: escribir los mismos UID en una segunda carpeta haría que `caldir push` viera dos archivos por UID (Radicale #101). Con `download_root` solapado con `caldir_root` (la config de ejemplo del contenedor), `calendar` nombra los ramos solo desde Canvas (biblioteca vacía; leído como biblioteca, el caldir reservaría todas sus carpetas y la corrida siguiente pondría sufijo `-s<nrc>`), no escribe `curso.json` ahí, y mueve el `state.json` v1 que viajó con el rename a `.u_crawler/state.json`.
- **Adjuntos.** Varios archivos de Canvas pueden dar el mismo nombre en una carpeta (`Guía 1.pdf` y `guia_1.PDF` dan `guia-1.pdf`). `download::Claims` los separa sin mover nunca un archivo que ya tiene nombre. Cada archivo conserva el nombre guardado en su entrada `file:<id>` (`ItemState::file_name`, que se omite al serializar si falta). Una entrada sin nombre de antes de este cambio, pero que sí se descargó, se queda con el nombre simple. Un archivo nuevo toma el nombre simple solo si nadie lo usa en esta corrida y no hay nada en disco con ese nombre. En los demás casos se usa `<slug>-<id>.<ext>` (`layout::with_suffix`), un nombre que solo escribe ese id. Las tareas y los anuncios usan `layout::unique_names` en orden de id: nombre simple, después `-2`, `-3`… El sufijo sobrevive al corte de `MAX_SLUG_LEN`.
- **Fuera de T6.** `.u_crawler/library.json` lo escribe `migrate::ensure` (T7), que todo escritor corre antes de leer la biblioteca.

### API de `migrate::ensure`

Implementada en T7 (`src/migrate/ensure.rs`). Junta las piezas anteriores; es lo que corre `u_crawler migrate` y lo que corren los escritores al iniciar.

- **`Target { library, caldir: Option, naming }`**. `caldir: None` deja el árbol del calendario sin tocar.
- **`migrate(&Target, Option<&CanvasClient>, dry_run) -> Result<Outcome, MigrateError>`**. Orden:
  1. `apply::pending` → `apply::resume` (la intención del diario, no un plan nuevo).
  2. `scan` → `plan` → `apply` con un `ts` nuevo, si el plan no está vacío. Con cliente de Canvas, `canvas_courses` aporta nombre oficial y posiciones de módulos: la lista de activos más `GET /api/v1/courses/<id>` para los ids que dio un `index.json` y ya no están activos (si Canvas no responde por uno inactivo, se planifica sin red; si falla la lista de activos o los módulos de un activo, la migración se detiene).
  3. Reescritura de las notas generadas que **un diario** renombró (`Entry::Rename` con `generated_kind` en el origen v1 y en el destino v2), con el `ts` del diario como `backup_ts`. Frontmatter: `ramo` y `tipo`; un anuncio suma `canvas_id` (del nombre v1 `…_<id>.md`), `url` y `publicado` (del `index.json` del ramo). Mapa de archivos: los `media` de ese `index.json` (`file_id` → `local_path` v1 → su destino en el diario). Sin `library.json` se repiten todos los diarios `Done` (idempotente: `rewrite_file` deja igual lo que ya calza), así un corte entre los renames y la reescritura se cura en la corrida siguiente; con `library.json`, solo los de esta llamada, porque un `sync` posterior ya escribió frontmatter más completo. Una nota que falla se registra y se cuenta, sin detener nada.
  4. Todos los índices, desde el disco (`library::write_indexes_from_disk`, T8, siempre): `README.md` por ramo y por módulo donde no haya uno (uno existente, de un `sync` o del usuario, no se pisa), cada `curso.json` puesto al día (`name` legible, `canvas_name`, `term` y `section` si faltaban), y `AGENTS.md` e `INDEX.md` en la raíz, con las entregas que dice el frontmatter de las tareas. Un archivo de la raíz cuya primera línea no es la nuestra no se pisa. `ensure_v2` con `library.json` corre este paso solo si a un ramo o módulo le falta su `README.md`. `undo` aparta los `README.md` de los ramos cuyo `curso.json` creó el diario.
  5. `.u_crawler/library.json` = `{"layout_version": 2}`, al final.
  
  `dry_run` escanea y planifica (`Outcome::DryRun { pending, plan, marked }`) y no crea nada. `migrate` no mira `library.json` para decidir si planifica, así que un ramo v1 copiado dentro de una biblioteca v2 se migra corriéndolo a mano. Errores: `NoLibrary`, `Overlap` (la biblioteca y `caldir_root` son la misma carpeta o una contiene a la otra), `Apply`, `Io`, `Canvas`.
- **`ensure_v2(&Target, Option<&CanvasClient>, dry_run)`**: raíz inexistente → `NoLibrary` (el escritor la crea); raíz que se solapa con `caldir_root` → `NoLibrary` (registrado en `info`; las carpetas del caldir las adopta `calendar`); `library.json` y ningún diario abierto → `AlreadyV2`, sin escanear ni preguntar a Canvas. Si no, `migrate`, y si movió o reescribió algo, `warn!` con el resumen y una línea `status!`. `ensure_configured(&Config, ..)` lo llama con `download_root`, `calendar.caldir_root` y el cliente de Canvas; un error detiene el comando.
- **`undo(&Target, ts) -> Result<UndoSummary, MigrateError>`**: rechaza un diario desconocido o uno con un diario posterior vigente **antes** de tocar nada. Devuelve cada nota respaldada en `.u_crawler/backup/<ts>/` a sus bytes originales y aparta la versión reescrita en `.u_crawler/migrations/<ts>.undo/<ruta>` (rename sin reemplazo); después `apply::undo`; después aparta `library.json` igual, para que el próximo escritor vuelva a migrar (por ejemplo tras cambiar `[layout]`). `AGENTS.md` e `INDEX.md` se quedan. Cada paso es idempotente.
- **CLI.** `--root P` migra `P` con `caldir: None` (un clon `cp -al` no renombra el calendario real), salvo que `P` sea el propio `download_root`. El informe sale por `println!` en `main.rs` para que sobreviva a una redirección (`status!` no imprime si la salida no es una terminal). Códigos de salida: `0`, `10` (config), `12` (cualquier fallo de la migración).

Test de integración: `tests/migrate_ensure.rs`, sobre la fixture T1, sin red.

### Reglas del migrador (invariantes que se testean)

1. No existe ninguna operación de borrado de archivos. Las únicas son `rename`, `mkdir`, la escritura atómica de `.md` con respaldo previo y `rmdir(2)` de las carpetas v1 que la migración dejó vacías (de abajo hacia arriba, registrado en el diario, nunca `remove_dir_all`; `--undo` las vuelve a crear). Una carpeta v1 con cualquier archivo se conserva y se reporta.
2. El plan es determinista: la misma entrada siempre produce el mismo plan, con todo ordenado.
3. Es idempotente: correrlo sobre una biblioteca v2 da un plan vacío.
4. Antes de cada operación se escribe y se sincroniza su línea en el diario. Un corte a mitad de camino se reanuda.
5. `Zoom/<id>` sin un ramo asociado se queda en su lugar, con un `warn!`. Un ramo sin `index.json`, como ATDF106-2520, se migra igual y queda con `canvas_id: null` hasta el próximo `sync`.
6. Todo `rename` es sobre el mismo sistema de archivos. Ante `EXDEV` la ejecución se detiene y no copia.

### Fixture sintética (T1)

`tests/fixtures/v1/` escribe un árbol v1 determinista en un directorio vacío que le pasa el test (`write_v1_library`). No usa datos reales: los nombres son los ejemplos de este spec y de `layout.rs`, los ids son inventados y los mp4 son unos pocos bytes. Un test de integración lo incluye con `#[path = "fixtures/v1/mod.rs"] mod v1;`.

La función devuelve el catálogo y el manifiesto `ruta relativa → SHA-1` de cada archivo. `hash_tree` vuelve a hashear el árbol: en un test de no-pérdida el multiset de hashes no cambia aunque cambien las rutas. Los archivos de `foreign` (`.obsidian/`, `.git/HEAD`, `Pasted image.png`) se quedan en la misma ruta.

Los nombres v2 del catálogo salen de `layout` con la plantilla UNAB comentada en `assets/config.toml` (`unab_naming`, `course_dirs` con `existing` vacío, `parse_v1_recording` + `recording_file`, `attachment_file`). Los nombres v1 salen de `fsutil`, igual que los escritores. El árbol cubre las dos secciones de ATDF106 (2520 con `announcements/` y sin `index.json`), el par `…DISRUPCION_EMPRESARIAL.mp4` / `…_1.mp4`, `Sesion_sincronica_1`, cuatro ramos de 2025 sin `announcements/` (uno por cada nombre de grabación del hallazgo de la biblioteca), `nota-del-usuario.md` dentro de un módulo, `CodeR_S3.zip`, un `Zoom/424242/` huérfano y (T9) dos pares de ítems distintos con el mismo nombre v2: los anuncios 801 y 802 de DISRUPCION (mismo día y título, cuerpo idéntico) y las tareas `04-ASSIGN-` y `06-ASSIGN-` de MINERIA con el título `Semana 11 Sumativa 3 Informe de caso 3` (cuerpos distintos). No hay `library.json`.

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
