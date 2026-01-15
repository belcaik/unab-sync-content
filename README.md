# u_crawler

A command-line tool for backing up Canvas LMS courses and Zoom cloud recordings.

## Overview

u_crawler automates the backup of your educational content from Canvas Learning Management System, including:

- **Course content**: Module pages, assignment instructions, and announcements exported as Markdown
- **Attachments**: PDFs, documents, images, and other files linked in your courses
- **Zoom recordings**: Cloud recordings from Zoom meetings integrated with Canvas

The tool supports resumable downloads, rate limiting, and incremental syncs to efficiently maintain up-to-date backups.

## Table of Contents

- [Features](#features)
- [Prerequisites](#prerequisites)
- [Installation](#installation)
  - [Windows](#windows)
  - [macOS](#macos)
  - [Linux](#linux)
  - [Verifying Installation](#verifying-installation)
- [Quick Start](#quick-start)
- [Commands](#commands)
  - [init](#init)
  - [auth](#auth)
  - [scan](#scan)
  - [sync](#sync)
  - [zoom](#zoom)
- [Configuration](#configuration)
- [Zoom Recording Workflow](#zoom-recording-workflow)
- [Troubleshooting](#troubleshooting)
- [Exit Codes](#exit-codes)
- [License](#license)

## Features

- **Canvas course backup**: Export module pages and assignments as Markdown files
- **Attachment downloads**: Automatically download linked files (PDF, DOCX, PNG, etc.)
- **Zoom integration**: Download cloud recordings from Zoom-enabled courses
- **Incremental sync**: Only download new or modified content
- **Resumable downloads**: Interrupted downloads resume from where they stopped
- **Rate limiting**: Configurable request throttling to avoid API limits
- **Dry-run mode**: Preview changes before writing files
- **Course filtering**: Include or exclude specific courses from sync operations

## Prerequisites

Before installing u_crawler, ensure you have:

| Requirement | Version | Purpose |
|-------------|---------|---------|
| Rust toolchain | 1.70+ | Building from source |
| ffmpeg | Any recent | Downloading Zoom recordings |
| Chromium or Edge | Any recent | Zoom authentication via Chrome DevTools Protocol |

## Installation

### Windows

1. **Install Rust**

   Download and run the installer from [rustup.rs](https://rustup.rs/), then restart your terminal.

2. **Install ffmpeg**

   Download from [ffmpeg.org](https://ffmpeg.org/download.html#build-windows), extract to a folder (e.g., `C:\ffmpeg`), and add `C:\ffmpeg\bin` to your system PATH.

3. **Build u_crawler**

   ```powershell
   git clone https://github.com/yourusername/u_crawler.git
   cd u_crawler
   cargo build --release
   ```

4. **Add to PATH (optional)**

   ```powershell
   copy target\release\u_crawler.exe C:\Windows\System32\
   ```

### macOS

1. **Install Rust**

   ```bash
   curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
   source "$HOME/.cargo/env"
   ```

2. **Install ffmpeg**

   ```bash
   brew install ffmpeg
   ```

3. **Build u_crawler**

   ```bash
   git clone https://github.com/yourusername/u_crawler.git
   cd u_crawler
   cargo build --release
   ```

4. **Add to PATH (optional)**

   ```bash
   # Add to your shell profile (.zshrc or .bash_profile)
   export PATH="$HOME/path/to/u_crawler/target/release:$PATH"
   ```

### Linux

1. **Install Rust**

   ```bash
   curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
   source "$HOME/.cargo/env"
   ```

2. **Install ffmpeg**

   ```bash
   # Ubuntu/Debian
   sudo apt update && sudo apt install ffmpeg

   # Fedora
   sudo dnf install ffmpeg

   # Arch Linux
   sudo pacman -S ffmpeg
   ```

3. **Build u_crawler**

   ```bash
   git clone https://github.com/yourusername/u_crawler.git
   cd u_crawler
   cargo build --release
   ```

4. **Install system-wide (optional)**

   ```bash
   sudo cp target/release/u_crawler /usr/local/bin/
   ```

### Verifying Installation

Confirm all components are installed correctly:

```bash
rustc --version          # Should show 1.70.0 or later
ffmpeg -version          # Should display ffmpeg version info
cargo run -- --help      # Should show u_crawler help
```

## Quick Start

### 1. Initialize configuration

Create the default configuration file:

```bash
cargo run -- init
```

This creates `~/.config/u_crawler/config.toml` (or `%APPDATA%\u_crawler\config.toml` on Windows).

### 2. Authenticate with Canvas

Using a Personal Access Token (PAT):

```bash
cargo run -- auth canvas --base-url https://your-school.instructure.com --token YOUR_TOKEN
```

Or retrieve the token from a password manager:

```bash
cargo run -- auth canvas --base-url https://your-school.instructure.com \
    --token-cmd "pass show canvas/pat"
```

### 3. List your courses

```bash
cargo run -- scan
```

### 4. Sync course content

Preview what would be downloaded:

```bash
cargo run -- sync --dry-run
```

Download all courses:

```bash
cargo run -- sync
```

Download a specific course:

```bash
cargo run -- sync --course-id 123456
```

### 5. Back up Zoom recordings

First, launch a browser with remote debugging enabled:

```bash
chromium --remote-debugging-port=9222 --user-data-dir=/tmp/u_crawler-profile
```

Then run the Zoom backup:

```bash
cargo run -- zoom flow --course-id 123456
```

## Commands

### init

Creates a default configuration file.

```bash
cargo run -- init
```

### auth

Configures authentication credentials for Canvas.

```bash
# Using a token directly
cargo run -- auth canvas --base-url URL --token TOKEN

# Using a command to retrieve the token
cargo run -- auth canvas --base-url URL --token-cmd "command"
```

### scan

Lists courses and inspects their content.

```bash
# List all active courses
cargo run -- scan

# Inspect a specific course
cargo run -- scan --course-id 123456
```

### sync

Downloads course content to the local filesystem.

| Flag | Description |
|------|-------------|
| `--course-id ID` | Sync only the specified course |
| `--dry-run` | Preview changes without downloading |
| `--verbose` | Show skipped items and additional details |

```bash
# Sync all courses
cargo run -- sync

# Sync one course with verbose output
cargo run -- sync --course-id 123456 --verbose
```

### zoom

Manages Zoom recording downloads. The primary command is `zoom flow`, which handles the entire process automatically.

| Flag | Description |
|------|-------------|
| `--course-id ID` | Target course (required) |
| `--debug-port PORT` | CDP port (default: 9222) |
| `--keep-tab` | Keep the browser tab open after capture |
| `--concurrency N` | Number of parallel downloads (default: 1) |
| `--since DATE` | Only download recordings after this date (YYYY-MM-DD) |

```bash
cargo run -- zoom flow --course-id 123456 --since 2024-01-01
```

For advanced use cases, individual subcommands are available:

- `zoom sniff-cdp` - Capture authentication credentials
- `zoom list` - List available recordings
- `zoom fetch-urls` - Retrieve download URLs
- `zoom dl` - Download recordings

## Configuration

Configuration is stored in `~/.config/u_crawler/config.toml` (Linux/macOS) or `%APPDATA%\u_crawler\config.toml` (Windows).

### Example Configuration

```toml
# General settings
download_root = "~/Documents/Canvas-Backup"
concurrency = 4          # Parallel downloads
max_rps = 2              # API requests per second
user_agent = ""          # Custom user agent (optional)

# Canvas LMS settings
[canvas]
base_url = "https://your-school.instructure.com"
token = ""               # Leave empty if using token_cmd
token_cmd = "pass show canvas/pat"
ignored_courses = ["153095", "153607"]

# Logging settings
[logging]
level = "info"           # trace | debug | info | warn | error
file = "~/.config/u_crawler/u_crawler.log"

# Zoom settings
[zoom]
enabled = true
ffmpeg_path = "ffmpeg"
cookie_file = "~/.config/u_crawler/zoom_cookies.txt"
user_agent = "Mozilla/5.0"
external_tool_id = 187
```

### Configuration Options

| Option | Description | Default |
|--------|-------------|---------|
| `download_root` | Directory for downloaded files | Required |
| `concurrency` | Number of parallel downloads | 4 |
| `max_rps` | Maximum API requests per second | 2 |
| `canvas.base_url` | Your Canvas instance URL | Required |
| `canvas.token` | Personal Access Token | - |
| `canvas.token_cmd` | Command to retrieve token | - |
| `canvas.ignored_courses` | Course IDs to skip | [] |
| `logging.level` | Log verbosity | info |
| `zoom.enabled` | Enable Zoom features | true |
| `zoom.ffmpeg_path` | Path to ffmpeg binary | ffmpeg |
| `zoom.external_tool_id` | Zoom LTI tool ID in Canvas | - |

## Zoom Recording Workflow

The `zoom flow` command automates the complete process of downloading Zoom cloud recordings:

### Prerequisites

1. Launch a Chromium-based browser with remote debugging enabled:

   ```bash
   chromium --remote-debugging-port=9222 --user-data-dir=/tmp/u_crawler-profile
   ```

2. Log into Canvas in that browser instance and complete any SSO authentication.

3. Ensure ffmpeg is available (check with `ffmpeg -version`).

### How It Works

1. **Credential Capture**: Opens the Zoom external tool in Canvas via Chrome DevTools Protocol (CDP), capturing authentication cookies and API headers.

2. **Recording Discovery**: Queries the Zoom API to enumerate available meetings and their download URLs.

3. **URL Resolution**: Opens each recording page in an ephemeral browser tab to capture the signed download headers.

4. **Download**: Attempts to download using `ffmpeg -c copy`. If that fails, falls back to direct HTTP download with resume support.

### Output Structure

Recordings are saved to:

```
<download_root>/Zoom/<course_id>/<meeting_title>_<date>.mp4
```

Downloads use `.part` files and HTTP Range requests, allowing safe resumption if interrupted.

## Troubleshooting

### ffmpeg Not Found

**Symptoms**: Error "ffmpeg missing" or exit code 13.

**Solutions**:
- Verify installation: `ffmpeg -version`
- On Windows: Add ffmpeg to PATH or set `zoom.ffmpeg_path` to the full path
- On Linux/macOS: Install via package manager or set absolute path in config

### Canvas Authentication Fails

**Symptoms**: "auth error" or exit code 11.

**Solutions**:
- Verify your Personal Access Token is valid and not expired
- Confirm `base_url` matches your Canvas instance exactly
- Test your `token_cmd` manually to ensure it returns the token
- Re-run: `cargo run -- auth canvas --base-url URL --token TOKEN`

### Zoom Authentication Fails

**Symptoms**: CDP flow times out or fails to capture credentials.

**Solutions**:
- Ensure browser is launched with `--remote-debugging-port=9222`
- Log into Canvas in that browser before running `zoom flow`
- Complete SSO prompts when they appear
- Use `--debug-port` if your browser uses a different port

### Rate Limit Errors

**Symptoms**: Network errors or exit code 12.

**Solutions**:
- Reduce `max_rps` in config (e.g., from 2 to 1)
- Reduce `concurrency` (e.g., from 4 to 2)
- Wait a few minutes before retrying

### Partial Download Failures

**Symptoms**: Some files fail to download (exit code 15).

**Solutions**:
- Re-run the command; downloads are resumable
- Check available disk space
- Verify write permissions for `download_root`
- Use `--verbose` to identify specific failures
- Check logs with `level = "debug"`

### Zoom Recordings Won't Download

**Symptoms**: Recordings are listed but fail to download.

**Solutions**:
- Verify you have download permissions in Zoom
- Confirm ffmpeg works: `ffmpeg -version`
- Exit code 14 indicates insufficient permissions
- Try the manual flow: `zoom sniff-cdp`, `zoom list`, `zoom dl`
- Check logs for specific error messages

### Configuration File Not Found

**Symptoms**: Tool can't find config.toml.

**Solutions**:
- Run `cargo run -- init` to create the default config
- Verify the config directory exists
- Check file permissions

### Debug Mode

For detailed diagnostics, enable debug logging:

```toml
[logging]
level = "debug"
```

Then check `~/.config/u_crawler/u_crawler.log` after running commands.

## Exit Codes

| Code | Meaning |
|------|---------|
| 0 | Success |
| 10 | Configuration error |
| 11 | Authentication error |
| 12 | Network or rate limit error |
| 13 | ffmpeg missing or failed |
| 14 | Permission denied (no download rights) |
| 15 | Partial failure (some items failed) |

## Additional Notes

- **Incremental sync**: The sync command only downloads new or modified content.
- **File naming**: Names are sanitized to ASCII with underscores; repeated separators are collapsed.
- **Idempotent operations**: Commands can be safely re-run; they resume from where they stopped.
- **Ignored courses**: Use `ignored_courses` to exclude specific courses from bulk operations.
- **Dry-run mode**: Always preview with `--dry-run` before large sync operations.

## License

See [LICENSE](LICENSE) for details.
