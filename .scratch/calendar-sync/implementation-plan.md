# Implementation plan — calendar-sync flow

**Inputs:** `.scratch/calendar-sync/issues/01..13`, `docs/specs/calendar-sync-flow.md`, `AGENTS.md`.
**Method:** `/implement` at the top, `/tdd` inside every lane, Sonnet subagents doing the work.
**Orchestrator:** this session. It dispatches, gates, and merges. It does not write feature code.

---

## 0. Decisions — both closed 2026-08-23

1. **Ticket 01 (spike): CLOSED.** Verified against the real Radicale on the homeserver. VTODO
   round-trips with fields intact, local deletions propagate on `push` (D8 confirmed), and
   **caldir respects the filename it is given**. Findings written into
   `docs/specs/calendar-sync-flow.md` § "Hallazgos del spike". D3 stands unchanged.
2. **Branching: CLOSED.** One long-lived `feat/calendar-sync` off `main`, one conventional
   commit per ticket, subagents in git worktrees off that branch.

**The filename finding changes the shape of the work.** Because caldir keeps our names, the
planner names files from the **UID**, which derives from the Canvas assignment id and is
therefore stable across date and title changes. Consequences:

- Ticket 06's sharp case — "a date change makes a new file and orphans the old one" — **cannot
  occur**. Its acceptance box "si el cambio de fecha implica un nombre de archivo distinto, el
  archivo anterior deja de existir" is satisfied vacuously: the path never depends on the date.
  The agent for 06 must state that explicitly rather than inventing cleanup code for a case the
  design rules out.
- The one thing the spike did not observe is whether caldir uploads a *modified, same-named*
  file as an update. It is the most basic sync case there is, but it is unverified — so it
  becomes an explicit check at the ticket 05 manual gate (§5).

---

## 1. Dependency graph and waves

```
  01 spike ─┐  (closed)
  03 datos ─┴─> 04 planner ─┬─> 05 subcomando ─┬─> 11 resiliencia
  02 prefactor ─────────────┘                  ├─> 12 build sin headless ──> 13 docker+cron
                                               └─> 06 idempotencia ──> 07 prioridad
                                                        └──> 10 borrados       │
                                                                               ├─> 08 ventana
                                                                               └─> 09 entregado
```

The tickets say 07/08/09 are blocked only by 05, and that is true of their *inputs*. But all
four of 06–09 widen the same pure function, so running them in parallel buys a merge conflict
in the one file that matters. They are serialized into a single lane instead.

| Wave | Runs | Parallelism | Gate to exit |
|---|---|---|---|
| ~~**W0**~~ | ~~02 prefactor · 03 datos~~ | **DONE** | Merged. fmt/clippy clean, 47 tests pass. One box deferred to the 02 human gate. |
| ~~**W1**~~ | ~~04 planner~~ | **DONE** | Merged after one rework round: missing `DTSTAMP` (RFC 5545) and a UID lacking a semantics discriminator. 55 tests green. |
| ~~**W2**~~ | ~~05 subcomando~~ | **DONE (manual gate OPEN)** | Merged + a follow-up fix aligning the `caldir_root` default across code/AGENTS.md/README/template. |
| ~~**W3**~~ | ~~11 resiliencia ∥ 12 build sin headless~~ | **DONE** | Both merged. 11 reworked to isolate write failures too. 12 found the coupling shallow and implemented rather than deferring. Verified in both build configs; chromiumoxide absent from the no-default tree. |
| ~~**W4**~~ | ~~Lane A: 06 → 07 → 08 → 09 → 10 ∥ Lane B: 13 docker~~ | **DONE** | All 13 tickets merged. 98 tests default / 92 no-default, clippy clean in both configs. Manual gates outstanding. |

**File-conflict map** (why the parallel lanes are safe):

- W0: 01 touches `docs/specs/` only · 02 touches `fsutil.rs`/`syncer.rs`/`announcements.rs` ·
  03 touches `canvas.rs`/`Cargo.toml`. Overlap: none.
- W3: 11 touches the new calendar orchestration loop · 12 touches `Cargo.toml`, `main.rs`,
  `src/zoom/*`, `.github/workflows/ci.yml`. Overlap: `Cargo.toml` (03 already landed the
  `chrono/serde` change, so 12's edit is additive) and `main.rs` (12 adds `#[cfg]` around the
  Zoom arm; 11 does not touch main). Low, but merge 12 second.
- W4 Lane B (13) must not hardcode the on-disk layout, because Lane A's ticket 08 adds a
  second directory per course. 13 mounts the caldir root and nothing below it.

---

## 2. Pre-agreed seams

`/tdd` forbids writing a test at a seam that has not been agreed first. These are the four,
and they are the *only* places tests go:

| # | Seam | Signature (shape, not final) | Owned by |
|---|---|---|---|
| S1 | Course → directory | `fn course_dir(root: &Path, course: &Course) -> PathBuf` | 02 |
| S2 | Canvas JSON → `Assignment` | serde deserialization from a captured response body | 03 |
| S3 | **The planner** | `fn plan(course, assignments, submissions, now: DateTime<Utc>, prev: &State) -> Plan` | 04, 06, 07, 08, 09, 10 |
| S4 | Per-course run outcome | `run_calendar(...) -> Result<CalendarSummary>` with `synced`/`failed` counts | 11 |

S3 is the feature. Tests assert on the returned `Plan` — which files to write, with what
content, which to delete — and never on how the planner got there.

**Explicitly not tested** (repo has no HTTP mock server and this work does not add one, per
spec "Fuera del alcance de los tests"): the I/O executor, the Canvas calls, the Docker image.
Ticket 13's verification is manual and end-to-end.

---

## 3. Technical calls made up front

So five subagents don't each re-litigate them:

- **ICS emission is hand-rolled**, no crate. `VTODO`/`VEVENT` with `UID`/`DUE`/`DTSTART`/
  `DTEND`/`PRIORITY`/`STATUS`/`SUMMARY`/`URL` is a few dozen lines of string building, and it
  keeps the musl release target free of a new dependency the spec explicitly warns must be
  verified against it before any tag. If the spike (01) turns up a serialization subtlety that
  makes this a bad trade, the orchestrator revisits it — not the agent.
- **`chrono` gains `serde`** in ticket 03 (`features = ["clock", "serde"]`). Dates become
  `Option<DateTime<Utc>>`, per spec D2.
- **State namespace** is `calendar:{assignment_id}`, per spec D5, stored in the existing
  per-course `state.json` under `download_root` — **never inside the caldir tree**. caldir scans
  its directories for `.ics` files; a `state.json` sitting in a calendar collection is at best
  ignored and at worst confuses it, and spec D4 makes u_crawler the exclusive owner of what it
  puts there. State is u_crawler's bookkeeping, not calendar data.
- **UID scheme:** derived from the Canvas assignment id so it survives title and date changes
  (ticket 04), with the `VEVENT` UID distinguishable from the `VTODO` UID of the same
  assignment (ticket 08). Suggested: `u_crawler-todo-{assignment_id}@<host-ish>` /
  `u_crawler-window-{assignment_id}@…`. The agent for 04 fixes the exact form and 08 follows it.
- **UID scheme is settled by ticket 04 and must not change:** `u_crawler-todo-{assignment_id}@u-crawler.local`, with `u_crawler-window-{assignment_id}@u-crawler.local` reserved for ticket 08. A UID is the server-side identity of a published object — changing one after the first push orphans it rather than updating it.
- **`DTSTAMP` is derived from `updated_at`, falling back to `due_at` — never from `now`.** A clock-derived DTSTAMP would rewrite every file every run and make ticket 06 unsatisfiable.
- **Filenames derive from the UID, never from a date.** The spike proved caldir keeps the name
  we give it, so the path stays stable when a deadline moves and the file is simply rewritten
  in place. No date component in any filename.
- **Every HTTP call goes through `HttpCtx`; every list goes through `list_paginated`.**
  `AGENTS.md` non-negotiable. `student_ids[]=self` must be pre-encoded into the path string
  because `CanvasClient` has no query builder (spec D1).
- **Nothing outside `main.rs` prints.** Library code emits `tracing`; user output goes through
  `status!`.

---

## 4. The dispatch contract

Every ticket goes to one Sonnet subagent, launched with `isolation: "worktree"`, with this
prompt skeleton. The orchestrator fills the bracketed parts.

```
You are implementing ONE ticket in the u_crawler Rust CLI. Work only on this ticket.

Read first, in order:
  1. .scratch/calendar-sync/issues/[NN-name].md   <- your ticket, the acceptance boxes are the contract
  2. docs/specs/calendar-sync-flow.md              <- the design; decisions D1-D12 are settled, do not redesign
  3. AGENTS.md                                     <- non-negotiables; violating one fails the ticket
  4. .scratch/calendar-sync/implementation-plan.md <- sections 2 and 3: your seam and the settled technical calls

Invoke the `tdd` skill and follow it. Your seam is [S#: signature]. It is already agreed —
do not propose another, and do not write a test anywhere else. One failing test, then the
minimum code to pass it, then the next. No writing all tests up front.

Constraints:
  - Do not touch files outside [list]. If the ticket seems to require it, stop and report.
  - Do not add a dependency without saying so in your report.
  - Do not "fix" adjacent problems you notice. Note them in your report instead.
  - No `unwrap`/`expect` outside tests. No `#[allow(clippy::…)]`.

Before you report done, run and paste the output of:
  cargo fmt --all -- --check
  cargo clippy --all-targets --all-features --locked -- -D warnings
  cargo test

Commit to the current branch with a conventional-commit message (`feat:`/`refactor:`/`test:`/
`build:` as fits). Any doc the ticket names — AGENTS.md API table, README layout,
assets/config.toml — changes in the SAME commit.

Report back:
  - each acceptance box, ticked or not, with the evidence that ticks it
  - the test names you added and what behavior each pins
  - anything you had to decide that the ticket and spec did not settle
  - anything you found that is wrong with the ticket
```

**Rules the orchestrator holds to:**

- One ticket per agent. No agent gets two, even small ones — the acceptance boxes are the unit.
- An agent that reports a box unticked has *not* finished. Send it back with the specific box,
  or re-scope the ticket. Do not merge partial work and move on.
- The orchestrator re-runs `cargo clippy` and `cargo test` on the merged branch after every
  merge, not just on the agent's word. `AGENTS.md` warns that a stale local toolchain passes
  lints CI then fails — `rustup update` before trusting a clippy run.
- Ticket 12 has an explicit escape hatch: if separating `chromiumoxide` behind a feature flag
  turns out to be more invasive than the module structure suggests, the agent **reports rather
  than forces it**, and 13 proceeds with a larger image.

---

## 5. Human gates

These cannot be delegated. The plan stops at each until you clear it.

| After | You verify |
|---|---|
| 02 | Run `sync --dry-run` (or `announcements --dry-run`) against an already-synced course and confirm no new directory is proposed and nothing is renamed. The extraction is provably identical by inspection, but no agent has live credentials to run it. |
| 05 | Run the command, `caldir push`, confirm deadlines appear in your calendar client. **Then change one deadline in Canvas, re-run, re-push, and confirm the client shows the new date and not a duplicate** — this is the one spike question left unobserved. |
| 07 | High-priority tasks render differently in the client. |
| 08 | The window calendar can be hidden without hiding deadlines. |
| 09 | A group assignment submitted by a teammate shows as completed. |
| 11 | Point it at a nonexistent course id alongside real ones; the real ones still sync. |
| 13 | Container runs, writes to the volume, `caldir push` from its container publishes to Radicale. |

---

## 6. Close-out

After Wave 4: `/code-review` over the whole branch (Standards + Spec axes), then
`superpowers:finishing-a-development-branch` to decide integration. Before any release tag,
build the musl target — `release.yml` has no independent guard, and both the ICS work and
ticket 12's feature flag touch what that target compiles.
