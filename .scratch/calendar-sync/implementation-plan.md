# calendar-sync — what shipped, and what is still unverified

All 13 tickets in `issues/` are implemented and merged on `feat/calendar-sync`.
This file now records only what a later agent cannot recover by looking.

**The design lives elsewhere.** `docs/specs/calendar-sync-flow.md` holds decisions
D1–D12 and the spike findings; `AGENTS.md` holds the shipped behaviour — directory
layout, UID scheme, `DTSTAMP` derivation, state namespaces, exit codes, the
exclusive-ownership invariant. Those are authoritative. Read them, not a summary.

`git log main..HEAD` records how the work was sequenced. The seams are visible in
`src/calendar.rs`.

---

## Verification still owed

Every item below was implemented and unit-tested, and none has been observed
working against real infrastructure. Each states what it proves and what its
failure would invalidate, because a failure here is a design finding, not a bug
report.

**These need live Canvas credentials, a real Radicale, or a calendar client.** No
agent can clear them.

| Ticket | Run this | Passes when | A failure means |
|---|---|---|---|
| 05 | `u_crawler calendar --dry-run`, then for real, then `caldir push` | Deadlines appear in the client | The write path or the caldir tree layout is wrong |
| 05 | Move one deadline in Canvas, re-run, re-push | Client shows the new date and **one** entry | See "The unverified assumption" below — this is the big one |
| 02 | `u_crawler sync --dry-run` on an already-synced course | No new directory proposed, nothing renamed | The `course_dir` extraction changed a path; it is provably identical by inspection, so this is belt-and-braces |
| 07 | Look at a graded vs ungraded task in the client | High-priority tasks render differently | The client collapses the 1/5/9 buckets differently than RFC 5545 describes |
| 08 | Hide the windows calendar in the client | Deadlines remain visible | The two semantics are not actually separate collections |
| 09 | Find a group assignment a teammate submitted | Shows completed | Canvas does not propagate a teammate's submission to your own record, and D7 needs group logic after all |
| 11 | `--course-id` at a nonexistent id, alongside valid ones | Valid courses still sync; exit code 13 | Per-course isolation does not hold |
| 13 | Bring up the container, let it run, `caldir push` from its container | Files land in the volume, host-usable, and publish | Volume permissions or the uid/gid alignment is wrong |

## The unverified assumption

**Nobody has observed caldir uploading a modified, same-named file as an update.**

The spike (ticket 01) proved a VTODO round-trips intact, that deletions propagate,
and that caldir keeps the filename it is given. It did not cover modification.

Six merged tickets rest on this. Filenames derive from the assignment id and never
from a date, so a moved deadline rewrites the same path — which is what makes
ticket 06's duplicate-file hazard impossible rather than cleaned-up. If caldir
instead creates a second object, that reasoning collapses and tickets 06 and 10
change shape.

It is the most basic sync case there is and almost certainly works. It is still
the one load-bearing claim with no evidence under it, which is why it sits in the
ticket 05 gate above.

## Before any release tag

Build the musl target and watch it go green. `release.yml` has no independent
guard, and ticket 12's CI matrix entry for `x86_64-unknown-linux-musl` with
`--no-default-features` has never actually executed. Both the hand-rolled ICS
emission and the `zoom` feature flag touch what that target compiles.

## Known follow-up, deliberately not done

Course selection is duplicated three ways — `syncer.rs`, `announcements.rs`, and
`calendar.rs::select_active_courses` — and the copies disagree on one point: an
explicit `--course-id` naming a course in `ignored_courses` is **honoured** by
`announcements` and `calendar`, and **skipped** by `sync`.

`sync` is the outlier, and its behaviour predates this branch. Extracting a shared
helper means first deciding which behaviour is correct, then changing a flow the
spec puts out of scope. That decision is the work; the extraction is the easy part.
