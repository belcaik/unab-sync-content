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
  - [announcements](#announcements)
  - [recordings](#recordings)
  - [zoom](#zoom)
  - [status](#status)
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
- **Course filtering**: Exclude specific courses via `canvas.ignored_courses`

## Prerequisites

Before installing u_crawler, ensure you have:

| Requirement | Version | Purpose |
|-------------|---------|---------|
| Rust toolchain | 1.70+ | Building from source |
| Git | Any recent | One dependency is fetched from a Git repository at build time |
| ffmpeg | Any recent | Downloading Zoom recordings |
| Chromium or Chrome | Any recent | Launched automatically for Zoom authentication |

## Installation

### Windows

1. **Install Rust**

   Download and run the installer from [rustup.rs](https://rustup.rs/), then restart your terminal.

2. **Install ffmpeg**

   Download from [ffmpeg.org](https://ffmpeg.org/download.html#build-windows), extract to a folder (e.g., `C:\ffmpeg`), and add `C:\ffmpeg\bin` to your system PATH.

3. **Build u_crawler**

   ```powershell
   git clone https://github.com/belcaik/u_crawler.git
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
   git clone https://github.com/belcaik/u_crawler.git
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
   git clone https://github.com/belcaik/u_crawler.git
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

Set `canvas.sso_email` and `canvas.sso_password` in your config, then:

```bash
cargo run -- zoom flow --course-id 123456
```

u_crawler launches its own browser and completes the institutional SSO. You do
not need to start Chromium yourself.

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

### announcements

Downloads course announcements as Markdown, extracts the links and media in each
body, and writes an `index.json` describing them.

| Flag | Description |
|------|-------------|
| `--course-id ID` | Only this course |
| `--dry-run` | Report what would be written, without writing |

```bash
cargo run -- announcements --course-id 123456
```

Announcements are also synced as part of `sync`, unless `[announcements].enabled`
is set to `false`.

### recordings

Reports the Zoom links found across a course's pages, module items and
assignments. This is a discovery report — it downloads nothing.

| Flag | Description |
|------|-------------|
| `--course-id ID` | Only this course |
| `--dry-run` | Marks output lines as a preview |

```bash
cargo run -- recordings --course-id 123456
```

### zoom

Downloads Zoom cloud recordings. `zoom flow` performs the whole process: it
reuses a stored session if one is still valid, otherwise it drives the
institutional SSO in a headless browser to capture a new one, then lists and
downloads the recordings.

| Flag | Description |
|------|-------------|
| `--course-id ID` | Target course (required) |
| `--since DATE` | Only recordings after this date (`YYYY-MM-DD`) |

```bash
cargo run -- zoom flow --course-id 123456 --since 2024-01-01
```

The browser is launched and managed by u_crawler. You do not need to start
Chromium yourself, and there is no debugging port to configure.

### status

Summarises what has been backed up: per course, the number of tracked files,
storage used, the most recent sync timestamp, and any items whose last attempt
failed.

| Flag | Description |
|------|-------------|
| `--verbose` | List each failed item and its error |

```bash
cargo run -- status --verbose
```

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
# Institutional SSO, used by the Zoom flow to log in headlessly.
# SECURITY: sso_password is stored here in cleartext. Restrict the file's
# permissions (chmod 600) and prefer a machine you control.
sso_email = "you@your-school.edu"
sso_password = ""

# Announcement sync (also runs as part of `sync`)
[announcements]
enabled = true           # set false to skip announcements
download_media = true    # also download attachments and inline media

# Logging settings
[logging]
level = "info"           # trace | debug | info | warn | error
file = "~/.config/u_crawler/u_crawler.log"

# Zoom settings
[zoom]
enabled = true           # set false to skip Zoom entirely
ffmpeg_path = "ffmpeg"
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
| `canvas.sso_email` | Institutional SSO account, used by the Zoom flow | - |
| `canvas.sso_password` | SSO password, stored in cleartext | - |
| `user_agent` | Custom user agent | built-in default |
| `logging.level` | Log verbosity | info |
| `logging.file` | Log file path | `~/.config/u_crawler/u_crawler.log` |
| `announcements.enabled` | Sync announcements | true |
| `announcements.download_media` | Download announcement attachments | true |
| `zoom.enabled` | Enable Zoom features | true |
| `zoom.ffmpeg_path` | Path to ffmpeg binary | ffmpeg |
| `zoom.user_agent` | User agent for Zoom requests | built-in default |
| `zoom.external_tool_id` | Zoom LTI tool ID in Canvas | 187 |

## Zoom Recording Workflow

The `zoom flow` command automates the complete process of downloading Zoom cloud recordings:

### Prerequisites

1. Set `canvas.sso_email` and `canvas.sso_password` in your config. The flow logs
   in on your behalf, so it needs them.
2. Ensure ffmpeg is available (check with `ffmpeg -version`).
3. Ensure Chromium or Chrome is installed. u_crawler launches it itself.

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

**Symptoms**: the flow times out or fails to capture credentials.

**Solutions**:
- Confirm `canvas.sso_email` and `canvas.sso_password` are set and correct
- Confirm `zoom.external_tool_id` matches the Zoom LTI tool id in your Canvas
- Confirm Chromium or Chrome is installed and on `$PATH`
- Set `logging.level = "debug"` and check the log file for the step that stalled

### Rate Limit Errors

**Symptoms**: Network errors or exit code 12.

**Solutions**:
- Reduce `max_rps` in config (e.g., from 2 to 1)
- Reduce `concurrency` (e.g., from 4 to 2)
- Wait a few minutes before retrying

### Partial Download Failures

**Symptoms**: some files fail to download.

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
- u_crawler falls back to a plain HTTP download when ffmpeg fails; if both fail,
  the recording's short-lived URL likely expired — re-run the command
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
| 12 | Network, rate limit, or runtime failure |

Note that on a first run, u_crawler creates the config file and exits with code
10 so that you notice you need to edit it.

## Additional Notes

- **Incremental sync**: The sync command only downloads new or modified content.
- **File naming**: Names are sanitized to ASCII with underscores; repeated separators are collapsed.
- **Idempotent operations**: Commands can be safely re-run; they resume from where they stopped.
- **Ignored courses**: Use `ignored_courses` to exclude specific courses from bulk operations.
- **Dry-run mode**: Always preview with `--dry-run` before large sync operations.

## License

No license has been declared for this project yet. Until one is added, all
rights are reserved by default and the code carries no permission to reuse it.
