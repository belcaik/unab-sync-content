# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).
## [Unreleased]

### Bug Fixes

- Enhance SSO handling by refining account tile selection and adding email/password input checks
- Address QA issues - add remote change detection and failed download tracking (qa-requested)

### Documentation

- Adjust Conventional Commits section formatting
- update README for improved usage instructions and configuration details

### Features

- Initialize u_crawler project with basic CLI and configuration management
- **logging**: Add file logger and config
- **canvas**: Add HTTP client, pagination, and scan listing
- **sync**: Add sync engine with markdown pages and attachment downloads
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

### Miscellaneous

- Sanitize example config and update Cargo.lock
- Enhance CI/CD workflows with improved build and release processes
- Update .gitignore to include local CI testing files
- Refactor release workflow to simplify tag handling and improve asset upload process

### Refactor

- Improve code readability by formatting and restructuring error handling in sync_module and handle_status functions


