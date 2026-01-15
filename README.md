u_crawler — Canvas/Zoom course backup CLI
=================================================

Installation
------------

### Prerequisites

Before installing `u_crawler`, ensure you have:

1. **Rust toolchain** (1.70 or later)
2. **ffmpeg** (required for Zoom recording downloads)
3. **Chromium or Edge browser** (for Zoom authentication via CDP)

### Windows Installation

1. **Install Rust:**
   - Download and run the installer from [rustup.rs](https://rustup.rs/)
   - Follow the prompts to complete installation
   - Restart your terminal to update PATH

2. **Install ffmpeg:**
   - Download from [ffmpeg.org](https://ffmpeg.org/download.html#build-windows)
   - Extract to a folder (e.g., `C:\ffmpeg`)
   - Add `C:\ffmpeg\bin` to your system PATH

3. **Install u_crawler:**
   ```powershell
   git clone https://github.com/yourusername/u_crawler.git
   cd u_crawler
   cargo build --release
   ```

4. **Optionally add to PATH:**
   ```powershell
   # Add target\release to your PATH, or copy the executable
   copy target\release\u_crawler.exe C:\Windows\System32\
   ```

### macOS Installation

1. **Install Rust:**
   ```bash
   curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
   source $HOME/.cargo/env
   ```

2. **Install ffmpeg:**
   ```bash
   # Using Homebrew
   brew install ffmpeg
   ```

3. **Install u_crawler:**
   ```bash
   git clone https://github.com/yourusername/u_crawler.git
   cd u_crawler
   cargo build --release
   ```

4. **Optionally add to PATH:**
   ```bash
   # Add to your shell profile (.zshrc, .bash_profile, etc.)
   export PATH="$HOME/path/to/u_crawler/target/release:$PATH"
   ```

### Linux Installation

1. **Install Rust:**
   ```bash
   curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
   source $HOME/.cargo/env
   ```

2. **Install ffmpeg:**
   ```bash
   # Ubuntu/Debian
   sudo apt update && sudo apt install ffmpeg

   # Fedora
   sudo dnf install ffmpeg

   # Arch Linux
   sudo pacman -S ffmpeg
   ```

3. **Install u_crawler:**
   ```bash
   git clone https://github.com/yourusername/u_crawler.git
   cd u_crawler
   cargo build --release
   ```

4. **Optionally install system-wide:**
   ```bash
   sudo cp target/release/u_crawler /usr/local/bin/
   ```

### Verify Installation

After installation, verify everything is working:

```bash
# Check Rust
rustc --version

# Check ffmpeg
ffmpeg -version

# Check u_crawler
cargo run -- --help
```

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

8) Back up Zoom course recordings (sniff + capture + download in a single flow)

```
# First launch Chromium/Edge with remote debugging:
#   chromium --remote-debugging-port=9222 --user-data-dir=/tmp/u_crawler-profile

cargo run -- zoom flow --course-id 123456

# Optional:
#   --debug-port <port>      CDP port (default 9222)
#   --keep-tab               keep the capture tab open
#   --concurrency <n>        parallel downloads (default 1)
#   --since YYYY-MM-DD       filter meetings from that date
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
ffmpeg_path = "ffmpeg"                       # path to ffmpeg binary
cookie_file = "~/.config/u_crawler/zoom_cookies.txt"  # legacy (not used with CDP)
user_agent = "Mozilla/5.0"
external_tool_id = 187
```

Zoom recordings workflow
-----------------------

The new `zoom flow` command automates the entire cycle:

1. **Preparation**
   - Launch Chromium/Edge with `--remote-debugging-port` (default 9222) pointing to the profile where you've already logged into Canvas/SSO.
   - Make sure you have `ffmpeg` available (configurable in `zoom.ffmpeg_path`).
2. **Sniff CDP**
   - The tool opens `courses/{course}/external_tools/{external_tool_id}` in a controlled tab.
   - Captures `lti_scid`, `applications.zoom.us` cookies, API headers, and if it detects download buttons, clones the MP4 requests.
   - During the CDP flow it may ask you to complete SSO (Microsoft); do so in the popup tab.
3. **Listing and capture**
   - Queries `applications.zoom.us` to enumerate meetings and associated `playUrl`s.
   - For each `playUrl` an ephemeral tab is opened via CDP, redirects are followed, and the signed headers necessary for downloading are stored.
4. **Download**
   - first attempts `ffmpeg -c copy` with the captured headers;
   - if Zoom rejects `ffmpeg`'s reader, falls back to resumable direct HTTP download and then saves the MP4.

The final result is stored under `download_root/Zoom/<course_id>/`. Each download uses `.part` and `Range` to allow safe retries.

Notes
-----

- Sync writes Markdown for module pages and assignment instructions, and downloads linked attachments (PDF/DOCX/PNG/etc.), preserving file extensions.
- Names are sanitized to stable ASCII with underscores; repeated separators are collapsed.
- `ignored_courses` prevents syncing specific courses in both bulk and per-course modes.
- Dry-run prints a plan without writing files or state; `--verbose` in normal mode prints details about skipped items (unchanged pages/files).
- Logs are written to the file configured in `[logging]`. For troubleshooting API issues, set `level = "debug"` and rerun commands.
- `zoom flow` is idempotent: if a download fails you can repeat the command; it will reuse already saved headers and resume from `.part`.
- The command also keeps the previous utilities available (`zoom sniff-cdp`, `zoom list`, `zoom fetch-urls`, `zoom dl`) for advanced or manual workflows.

Exit Codes
----------

- 0: success
- 10: config error
- 11: auth error
- 12: network/rate-limit error (exhausted)
- 13: ffmpeg missing/failure
- 14: permissions (no download right)
- 15: partial (some items failed)
