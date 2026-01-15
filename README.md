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

Troubleshooting
---------------

### Common Issues

#### ffmpeg not found

**Problem:** Error message "ffmpeg missing" or exit code 13.

**Solution:**
- Verify ffmpeg is installed: `ffmpeg -version`
- On Windows, ensure ffmpeg is in your PATH or set `zoom.ffmpeg_path` in config.toml to the full path (e.g., `C:\ffmpeg\bin\ffmpeg.exe`)
- On Linux/macOS, install via package manager or set absolute path in config

#### Zoom authentication fails

**Problem:** CDP flow times out or doesn't capture credentials.

**Solution:**
- Ensure browser is launched with remote debugging:
  ```bash
  chromium --remote-debugging-port=9222 --user-data-dir=/tmp/u_crawler-profile
  ```
- Log into Canvas manually in that browser instance before running `zoom flow`
- Complete any SSO prompts (Microsoft, etc.) in the popup tab when asked
- Try increasing timeout or use `--debug-port` if using a different port

#### Canvas authentication fails

**Problem:** "auth error" or exit code 11.

**Solution:**
- Verify your Personal Access Token (PAT) is valid and not expired
- Check base-url matches your Canvas instance (e.g., `https://canvas.instructure.com`)
- If using `token_cmd`, ensure the command executes successfully:
  ```bash
  # Test your token command
  pass show canvas/pat
  ```
- Re-run authentication: `cargo run -- auth canvas --base-url <url> --token <PAT>`

#### Rate limit errors

**Problem:** Network/rate-limit error (exit code 12).

**Solution:**
- Reduce `max_rps` in config.toml (e.g., from 2 to 1)
- Reduce `concurrency` for downloads (e.g., from 4 to 2)
- Wait a few minutes before retrying
- Check if your Canvas instance has stricter rate limits

#### Partial download failures

**Problem:** Some files fail to download (exit code 15).

**Solution:**
- Re-run the same command; downloads are resumable (`.part` files)
- Check disk space and permissions in `download_root`
- Use `--verbose` to see which items failed and why
- Check logs at `~/.config/u_crawler/u_crawler.log` with `level = "debug"`

#### Zoom recordings don't download

**Problem:** Zoom videos are listed but fail to download.

**Solution:**
- Verify you have download permissions for the recordings in Zoom
- Ensure ffmpeg is working: `ffmpeg -version`
- Check that CDP captured valid headers (exit code 14 indicates no download rights)
- Try manual flow: `zoom sniff-cdp`, then `zoom list`, then `zoom dl`
- Review logs for specific error messages

#### Config file not found

**Problem:** Tool can't find or read config.toml.

**Solution:**
- Run `cargo run -- init` to create default config
- Manually create `~/.config/u_crawler/config.toml` (or `%APPDATA%\u_crawler\config.toml` on Windows)
- Verify file permissions allow reading
- Check that the directory exists

### Getting More Help

If issues persist:
1. Set `level = "debug"` in `[logging]` section of config.toml
2. Re-run the failing command
3. Check the log file at `~/.config/u_crawler/u_crawler.log`
4. Include relevant log excerpts when reporting issues

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
