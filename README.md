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
ffmpeg_path = "ffmpeg"
cookie_file = "~/.config/u_crawler/zoom_cookies.txt"
user_agent = "Mozilla/5.0"
```

Notes
-----

- Sync writes Markdown for module pages and assignment instructions, and downloads linked attachments (PDF/DOCX/PNG/etc.), preserving file extensions.
- Names are sanitized to stable ASCII with underscores; repeated separators are collapsed.
- `ignored_courses` prevents syncing specific courses in both bulk and per-course modes.
- Dry-run prints a plan without writing files or state; `--verbose` in normal mode prints details about skipped items (unchanged pages/files).
- Logs are written to the file configured in `[logging]`. For troubleshooting API issues, set `level = "debug"` and rerun commands.

Exit Codes
----------

- 0: success
- 10: config error
- 11: auth error
- 12: network/rate-limit error (exhausted)
- 13: ffmpeg missing/failure
- 14: permissions (no download right)
- 15: partial (some items failed)
