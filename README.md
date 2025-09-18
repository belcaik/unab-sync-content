u_crawler — Canvas/Zoom course backup CLI
=================================================

Quickstart
----------

1) Build and see help

```
cargo build
cargo run -- --help
```

2) Initialize config (writes to `~/.config/u_crawler/config.toml`)

```
cargo run -- init
```

3) Authenticate Canvas

Using a Personal Access Token (PAT):

```
cargo run -- auth canvas --base-url https://<tenant>.instructure.com --token <PAT>
```

Or retrieve the PAT from an external command (not stored in plaintext):

```
cargo run -- auth canvas --base-url https://<tenant>.instructure.com \
  --token-cmd "pass show canvas/pat"
```

4) List your active courses

```
cargo run -- scan
```

5) Inspect one course (modules + derived file count)

```
cargo run -- scan --course-id 123456
```

6) Dry-run sync (no writes) to see what would be saved/downloaded

```
cargo run -- sync --dry-run                  # all allowed courses
cargo run -- sync --course-id 123456 --dry-run
```

7) Run sync (writes Markdown + downloads attachments)

```
cargo run -- sync                            # all allowed courses
cargo run -- sync --course-id 123456         # one course
cargo run -- sync --course-id 123456 --verbose   # also show skipped items
```

8) Respaldar grabaciones Zoom del curso (sniff + captura + descarga en un solo flujo)

```
# Lanza previamente Chromium/Edge con debug remoto:
#   chromium --remote-debugging-port=9222 --user-data-dir=/tmp/u_crawler-profile

cargo run -- zoom flow --course-id 123456

# Opcionales:
#   --debug-port <puerto>    puerto CDP (default 9222)
#   --keep-tab               deja abierta la pestaña de captura
#   --concurrency <n>        descargas en paralelo (default 1)
#   --since YYYY-MM-DD       filtra reuniones desde esa fecha
```

Configuration
-------------

Config file: `~/.config/u_crawler/config.toml`

Example config:

```
download_root = "~/Documents/UNAB/data/Canvas"
concurrency = 4                  # download concurrency
max_rps = 2                      # requests per second
user_agent = ""                 # optional custom UA

[canvas]
base_url = "https://<tenant>.instructure.com"
token = ""                      # optional if token_cmd is used
token_cmd = "pass show canvas/pat"
ignored_courses = ["153095", "153607"]

[logging]
level = "info"                  # trace|debug|info|warn|error
file  = "~/.config/u_crawler/u_crawler.log"

[zoom]
enabled = true
ffmpeg_path = "ffmpeg"                       # ruta al binario ffmpeg
cookie_file = "~/.config/u_crawler/zoom_cookies.txt"  # legado (no se usa con CDP)
user_agent = "Mozilla/5.0"
external_tool_id = 187
```

Zoom recordings workflow
-----------------------

The new `zoom flow` command automatiza todo el ciclo:

1. **Preparación**
   - Inicia Chromium/Edge con `--remote-debugging-port` (default 9222) apuntando al perfil donde ya iniciaste sesión en Canvas/SSO.
   - Asegúrate de tener `ffmpeg` disponible (configurable en `zoom.ffmpeg_path`).
2. **Sniff CDP**
   - La herramienta abre `courses/{course}/external_tools/{external_tool_id}` en una pestaña controlada.
   - Captura `lti_scid`, cookies de `applications.zoom.us`, cabeceras API y, si detecta botones de descarga, clona las peticiones MP4.
   - Durante el flujo CDP puede pedirte completar SSO (Microsoft); hazlo en la pestaña emergente.
3. **Listado y captura**
   - Se consulta `applications.zoom.us` para enumerar reuniones y `playUrl` asociados.
   - Para cada `playUrl` se abre una pestaña efímera vía CDP, se siguen redirecciones y se almacenan las cabeceras firmadas necesarias para descargar.
4. **Descarga**
   - primero intenta `ffmpeg -c copy` con los encabezados capturados;
   - si Zoom rechaza el lector de `ffmpeg`, hace fallback a descarga HTTP directa reanudable y luego guarda el MP4.

El resultado final se almacena bajo `download_root/Zoom/<course_id>/`. Cada descarga usa `.part` y `Range` para permitir reintentos seguros.

Notas
-----

- Sync writes Markdown for module pages and assignment instructions, and downloads linked attachments (PDF/DOCX/PNG/etc.), preserving file extensions.
- Names are sanitized to stable ASCII with underscores; repeated separators are collapsed.
- `ignored_courses` prevents syncing specific courses in both bulk and per-course modes.
- Dry-run prints a plan without writing files or state; `--verbose` in normal mode prints details about skipped items (unchanged pages/files).
- Logs are written to the file configured in `[logging]`. For troubleshooting API issues, set `level = "debug"` and rerun commands.
- `zoom flow` es idempotente: si una descarga falla puedes repetir el comando; reutilizará cabeceras ya guardadas y reanudará desde `.part`.
- El comando también mantiene disponibles las utilidades anteriores (`zoom sniff-cdp`, `zoom list`, `zoom fetch-urls`, `zoom dl`) para flujos avanzados o manuales.

Exit Codes
----------

- 0: success
- 10: config error
- 11: auth error
- 12: network/rate-limit error (exhausted)
- 13: ffmpeg missing/failure
- 14: permissions (no download right)
- 15: partial (some items failed)
