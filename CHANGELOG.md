# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).
## [0.0.2] - 2026-08-22

### Bug Fixes

- Correct dry-run, credential leakage, and two panic paths
- Route the Zoom client through HttpCtx and stop logging header values

### Documentation

- Rewrite the documentation against the code that actually exists
- Rewrite the README from the code rather than from the old README
- Correct the repository URL

### Miscellaneous

- Remove an orphaned allow(too_many_arguments)
- Lint every target, and add a lint table that matches the gate
- Fix three lint failures and cover the musl release target
- **deps**: Bump tracing-subscriber from 0.3.19 to 0.3.20
- **deps**: Bump bytes from 1.10.1 to 1.11.1
- **deps**: Bump time from 0.3.41 to 0.3.47
- Merge main and document what act cannot catch

### Refactor

- Delete dead config, dependencies, and stale comments
- Route all HTTP through HttpCtx and collapse duplicated fetch/download
- Decompose sync_module from 585 lines to 117
- Split headless.rs and give the Zoom session a type
- Decompose announcements sync and drop the last too_many_arguments
- Type the errors and remove the production panic paths
- Give the crate an output layer

## [0.0.1] - 2026-01-22

### Bug Fixes

- Enhance SSO handling by refining account tile selection and adding email/password input checks
- Address QA issues - add remote change detection and failed download tracking (qa-requested)

### Documentation

- Adjust Conventional Commits section formatting
- Update README for improved usage instructions and configuration details

### Features

- Initialize u_crawler project with basic CLI and configuration management
- **logging**: Add file logger and config\n\n- tracing-based file logger writing to configured path\n- [logging] level+file in config with tilde expansion\n- initialize logger early in main and add error logs\n- README: logging section and examples
- **canvas**: Add HTTP client, pagination, and scan listing\n\n- HTTP client with UA and compression\n- Link header parser and tests\n- Canvas client: list courses, modules, files, get page\n- Wire scan to list modules and derived file count
- **sync**: Add sync engine with markdown pages and attachment downloads\n\n- Save module pages to Markdown, compute content hash\n- Download file attachments with ETag skip and Range resume\n- Add fs utilities and persistent JSON state per course
- **zoom**: Implement Zoom API client and CDP sniffing functionality
- Enhance Zoom integration with replay header management and download automation
- Add flow command for capturing and downloading Zoom recordings
- **zoom**: Add 'zoom flow' command for automated recording capture and download
- Implement unified Zoom headless flow for SSO authentication and credential capture, replacing CDP sniffing.
- Enhance Zoom headless header extraction, refresh stored headers, and update API referer for improved reliability.
- Implement Microsoft SSO login flow and utilize persisted cookies for authenticated downloads.
- Add Zoom recording sync to course processing and remove deprecated replay header and download management.
- Ensure browser launches in full headless mode by removing headful configuration.
- Introduce new logging, Canvas, and Zoom configuration options and update existing defaults.
- Refactor config loading with `Config::load_or_init` and improve Zoom headless data capture.
- Refactor project into a library crate, add GitHub Actions release workflow, and optimize regex compilation.
- Update .gitignore to include Auto Claude data and generated files
- Add initial configuration files for project setup
- Revamp README for improved clarity and structure
- Add initial configuration and status files for project setup
- Add changelog generation workflow and initial changelog file

### Miscellaneous

- Sanitize example config and update Cargo.lock\n\n- Remove accidental token from assets/config.toml\n- Update lockfile for new dependencies
- Enhance CI/CD workflows with improved build and release processes
- Update .gitignore to include local CI testing files
- Refactor release workflow to simplify tag handling and improve asset upload process

### Refactor

- Improve code readability by formatting and restructuring error handling in sync_module and handle_status functions


