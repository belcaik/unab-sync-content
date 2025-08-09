u_crawler — Canvas/Zoom course backup CLI
=================================================

Milestone M0: Skeleton with CLI and config init.

Usage
-----

- Build: `cargo build`
- Help: `cargo run -- --help`
- Init config: `cargo run -- init`
- Set Canvas auth: `cargo run -- auth canvas --base-url https://<tenant>.instructure.com --token <PAT>`

Config
------

- Location: `~/.config/u_crawler/config.toml`
- On `init`, a default config is created with paths expanded (no `~`).

Notes
-----

- Future milestones will implement Canvas listing, sync, Zoom downloads, and status/clean flows.
- Never log secrets; tokens are stored in the config but not printed.

