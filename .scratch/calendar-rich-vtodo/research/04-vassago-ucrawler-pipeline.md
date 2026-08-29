# 04 — Deadline VTODO through the u_crawler → vassago → Google Tasks pipeline

Research date: 2026-08-28.
Primary sources: read-only clone at
`<scratch>/deps/vassago`
(referred to below as `<vassago>`; nothing in it was modified — `git status` was
clean before and after) and `<repo>` on branch
`feat/calendar-rich-vtodo` (which currently differs from `main` only in
`AGENTS.md` and `docs/agents/*` — no source changes yet).

---

## Verdict

### Field-by-field trace of a deadline VTODO

Pipeline: **u_crawler `plan`/`render_vtodo`** → writes
`<caldir_root>/<course>/deadlines/assignment-<id>.ics` → **merge-ucrawler.py**
→ `<CALENDAR_ROOT>/202615_<course>__deadlines/assignment-<id>.ics` (canonical,
CalDAV-backed) → **bridge-vtodo.py** → `<CALENDAR_ROOT>/unab/assignment-<id>.ics`
(Google Tasks mirror) → **caldir sync phase 2** → Google Tasks.

| Canvas field | VTODO property emitted today | merge-ucrawler | bridge-vtodo classification | reaches Google Tasks? |
| --- | --- | --- | --- | --- |
| — (constant) | `BEGIN:VCALENDAR` / `VERSION:2.0` / `PRODID:-//u_crawler//calendar-sync//EN` | taken from **source** (lines outside `BEGIN/END:VTODO` come from source, `<vassago>/caldir/merge-ucrawler.py:111-116`) | outside the component; never hashed, never projected | envelope only |
| `assignment.id` | `UID:u_crawler-todo-<id>@u-crawler.local` (`src/calendar.rs:115-117`, emitted at `:301`) | copied from source | identity key (`MANAGED_UID_PREFIX`, `bridge-vtodo.py:37`, `:387`, `:413`); in `semantic_hash` | yes — object identity |
| `assignment.updated_at`, else `due_at` | `DTSTAMP:<Z>` (`src/calendar.rs:263-270`, `:302`) | copied from source | **`IGNORE_FOR_HASH`** (`bridge-vtodo.py:59-63`) — invisible to change detection | mirrored, ignored |
| `assignment.due_at` | `DUE:YYYYMMDDTHHMMSSZ` (`:303`) | copied from source | **`COMMON_FIELDS`** (bidirectional) with date-only normalisation (`bridge-vtodo.py:39-45`, `:182-190`, `:217-225`, `:228-261`) | yes, as `DUE;VALUE=DATE:YYYYMMDD` |
| `points_possible` + `omit_from_final_grade` + `submission_types` | `PRIORITY:1\|5\|9` (`src/calendar.rs:219-232`, `:304`) | copied from source | **`RICH_FIELDS`** — canonical → Google only (`bridge-vtodo.py:47-57`, `:327-372`) | local mirror only; Google Tasks cannot store it |
| submission `submitted_at` / `workflow_state=="graded"` | `STATUS:COMPLETED`, **omitted entirely when not done** (`src/calendar.rs:294-308`) | **`USER_STATE_FIELDS`** — source value dropped, existing canonical value re-attached (`merge-ucrawler.py:13-17`, `:96-118`) | **`COMMON_FIELDS`** (bidirectional) | yes |
| — (user/Google only) | `COMPLETED`, `PERCENT-COMPLETE` (u_crawler never emits these) | preserved from existing canonical (`merge-ucrawler.py:13-17`) | `COMPLETED` is `COMMON_FIELDS`; `PERCENT-COMPLETE` is `RICH_FIELDS` | `COMPLETED` yes; `PERCENT-COMPLETE` mirror only |
| `assignment.name` | `SUMMARY:<escaped>` (`src/calendar.rs:309-312`) | copied from source (**source wins**) | **`COMMON_FIELDS`** (bidirectional) | yes — the task title |
| `assignment.html_url` | `URL:<escaped>` (`src/calendar.rs:313-315`) | copied from source | **`RICH_FIELDS`** — canonical → Google only | mirror only |
| `assignment.description` | **not emitted today** (field exists on the struct, `src/canvas.rs:220`, and is unused by `calendar.rs`) | would be copied from source, source wins | **`COMMON_FIELDS`** (bidirectional) | would round-trip as the Google Tasks "notes" |
| `assignment.unlock_at` | **not on the VTODO today** — only on the sibling window `VEVENT` (`src/calendar.rs:457-477`) | would be copied from source | **`RICH_FIELDS`** — canonical → Google only | mirror only; Google Tasks drops it |
| any other new property | — | copied verbatim from source; merge is **whitelist-of-user-state, blacklist-of-nothing** | not in `COMMON_FIELDS`, not in `RICH_FIELDS` → **stripped from the Google mirror only if it starts with `X-`** and not `X-GOOGLE-` (`bridge-vtodo.py:342-347`); otherwise left in place on the Google side but never refreshed from canonical | depends |

**One-line verdict.** `merge-ucrawler.py` is fully property-agnostic and
preserves anything new u_crawler emits. `bridge-vtodo.py` is **not**: it knows
only three buckets (`COMMON_FIELDS`, `RICH_FIELDS`, `IGNORE_FOR_HASH`) and
`DESCRIPTION` and `DTSTART` are both already in them — `DESCRIPTION` in
`COMMON_FIELDS` (bidirectional), `DTSTART` in `RICH_FIELDS` (canonical → Google
only). So no vassago change is required to *carry* them. The risk is not
"dropped"; it is **`DESCRIPTION` is bidirectional and therefore participates in
`shared_signature`, which is what raises conflicts.**

---

## A. vassago

### A1. `caldir/merge-ucrawler.py`

**What it merges, from where to where.** Two independent halves driven from
`main()` (`<vassago>/caldir/merge-ucrawler.py:237-250`):

- `sync_deadlines()` (`:135-194`): for every `ROOT/202615_*` course directory
  (`:140`), reads `<course>/deadlines/*.ics` and writes
  `ROOT/<course>__deadlines/*.ics` (`:141-158`).
- `sync_windows()` (`:197-234`): `<course>/windows/*.ics` →
  `ROOT/<course>__windows/*.ics`.

`ROOT` is `CALENDAR_ROOT`, default `/data/calendars` (`:9`).

**Identity.** Keyed on **filename**, not UID — `source_files` / `target_files`
are dicts keyed by `path.name` (`:149-157`). The UID is never parsed here. The
deletion rule is also filename-based: only files whose name starts with
`assignment-` and that are absent from the source are unlinked (`:186-192`),
with the Spanish comment stating the intent ("Sólo eliminamos archivos
pertenecientes al esquema de u_crawler", `:184-185`).

**Which properties are copied / rewritten / dropped / preserved.**
`merge_vtodo` (`:80-118`):

```python
merged = (
    source[: source_start + 1]
    + source_body
    + preserved
    + source[source_end:]
)
```

- `source_body` (`:101-109`) is **every** line of the source's VTODO body except
  those whose property name is in `USER_STATE_FIELDS`.
- `preserved` (`:96-99`) is the existing canonical file's lines for exactly
  those three fields.
- `USER_STATE_FIELDS = {"STATUS", "COMPLETED", "PERCENT-COMPLETE"}` (`:13-17`),
  with the comment "Estos campos pueden ser modificados por Thunderbird /
  Google. u_crawler no debe pisarlos en cada regeneración."
- Lines **outside** the `BEGIN:VTODO`/`END:VTODO` range come from the source
  (`:112`, `:115`), so `PRODID`, `VERSION` etc. follow u_crawler.

**Does it hash anything?** **No.** There is no hashing anywhere in the file. The
write decision is a raw string comparison, `if merged_raw != existing_raw`
(`:180`). Because `merge_vtodo` emits CRLF (`:118`) while `read_text()` has
already collapsed CRLF to LF (`:168`), the two never compare equal — every
canonical deadline is rewritten with byte-identical content on every run and
counted as `updated`. This is a *known, deliberately unfixed* fragility, pinned
by a test (see A5) and documented at `<vassago>/docs/sync-semantics.md:52-56`.

**Does it preserve unknown properties?** **Yes, completely.** `merge_vtodo`
copies the source body wholesale and only subtracts the three user-state names.
A brand-new `DESCRIPTION:` or `DTSTART:` on the u_crawler VTODO passes through
untouched, in source order. There is no allowlist to extend.

**Caveat — folding is destroyed.** `unfold` (`:20-32`) joins continuation lines
into logical lines and `merge_vtodo` re-emits them with `"\r\n".join(...)`
(`:118`) — it **never re-folds**. So a folded `DESCRIPTION` arriving in the
canonical file comes back out as one long unfolded line. u_crawler does not fold
either (`src/calendar.rs:318`), so today this is invisible; it matters the moment
a long `DESCRIPTION` exists.

**Writes** are atomic via a `.ics.tmp` sibling and `os.replace` (`:121-132`).

### A2. `caldir/bridge-vtodo.py`

**Direction and scope.** Canonical `ROOT/202615_*__deadlines/` (`:375-398`)
↔ the Google Tasks mirror directory `GOOGLE_TASKS_DIR`, default `ROOT/unab`
(`:23-28`, `:401-424`). Only UIDs starting with `u_crawler-todo-` are considered
(`:37`, `:387`, `:413`); personal Google tasks (`@google-tasks.com`) are skipped.

**Identity.** Keyed on **UID** (`Item.uid`, `:74-81`), parsed out of the VTODO.
Duplicate managed UIDs on either side raise `RuntimeError` (`:390-394`,
`:415-420`). The mirror filename is inherited from the canonical file on
`seed_google` (`:502`) but is not the identity.

**Field buckets** (`:39-63`):

```python
COMMON_FIELDS = {"SUMMARY", "DESCRIPTION", "DUE", "STATUS", "COMPLETED"}
RICH_FIELDS = {"PRIORITY", "URL", "DTSTART", "PERCENT-COMPLETE",
               "LOCATION", "ORGANIZER", "ATTENDEE", "ATTACH", "VALARM"}
IGNORE_FOR_HASH = {"DTSTAMP", "LAST-MODIFIED", "SEQUENCE"}
GOOGLE_PRIVATE_PREFIX = "X-GOOGLE-"
```

**Two hashes, two jobs.**

- `semantic_hash` (`:164-179`): SHA-256 over the **full logical lines** of every
  property in the component, sorted by property name, excluding
  `IGNORE_FOR_HASH` and anything starting with `X-GOOGLE-`. This answers "did
  *this side* change since the last run?" — compared against the state file.
  Because property names are sorted (`:167`) and values are the unfolded logical
  lines, both property order and line folding are invisible to it.
- `shared_signature` (`:193-214`): SHA-256 over **only** `COMMON_FIELDS`, sorted,
  with `DUE` reduced to `DUE-DATE:YYYYMMDD` (`:199-208`, via `normalize_due_date`
  `:182-190`). This answers "do the two sides agree?" — compared *between*
  canonical and Google, and is what decides conflict vs. no-conflict.

**Date-only normalisation.**

- Canonical → Google (`google_due_from_canonical`, `:217-225`): take the leading
  `YYYYMMDD` and emit `DUE;VALUE=DATE:YYYYMMDD`; pass the line through unchanged
  if there is no leading 8-digit date.
- Google → canonical (`canonical_due_from_google`, `:228-261`): if the existing
  canonical `DUE` was a datetime matching `^\d{8}(T.*)$`, the **time-of-day is
  preserved** and only the date replaced (`:252-259`) — the code's own example is
  `20260830T235900Z` + a Google move to 1 Sept → `20260901T235900Z` (`:247-251`).
- `seed_google` (`:499-522`) deliberately projects a date-only `DUE` immediately,
  "so that the first provider push does not create unnecessary canonicalization
  churn" (`:511-513`).

**DTSTART vs DUE.** They are treated as completely different classes. `DUE` is a
`COMMON_FIELD` with bespoke bidirectional date normalisation. `DTSTART` is a
`RICH_FIELD`: pushed canonical → Google by `copy_rich_fields_to_google`
(`:327-372`), **never** read back, and **never** normalised — no date-only
projection is applied to it at all. `replace_fields`'s special-casing is
`if field != "DUE"` (`:291`), so `DTSTART` would never even enter that path.

**What it sends onward.** `copy_rich_fields_to_google` (`:327-372`) rebuilds the
Google-side body by dropping every `RICH_FIELDS` line and every non-`X-GOOGLE-`
`X-` line, then appending the canonical versions of both (`:353-363`).
`replace_fields` (`:264-324`) does the same for `COMMON_FIELDS` in whichever
direction the `direction` argument names.

**Note — nondeterministic property order.** `replace_fields` iterates
`for field in fields:` over a **set** (`:288`), so the order in which
`SUMMARY`/`DESCRIPTION`/`DUE`/`STATUS`/`COMPLETED` are appended varies between
Python processes (hash randomisation). `copy_rich_fields_to_google` correctly
uses `sorted(RICH_FIELDS)` (`:353`). This is harmless for change detection —
both hashes sort by property name — but it means the mirror file's raw byte
order is not reproducible run to run.

### A3. `caldir/bridge-windows.py` — unaffected by VTODO changes

Confirmed: it handles the `windows` VEVENT collection only, and nothing in it can
see a VTODO.

- `MANAGED_UID_PREFIX = "u_crawler-window-"` (`:36`) — the `todo-` discriminator
  means a deadline UID can never match.
- `load_canonical` globs `ROOT/202615_*__windows` only (`:166-168`);
  `load_google` reads `GOOGLE_WINDOWS_DIR`, default `ROOT/unab-windows`
  (`:22-27`, `:189-211`) — a different directory from the VTODO bridge's `unab`.
- `find_vevent` (`:89-102`) raises `ValueError("VEVENT component not found")` on
  anything without a VEVENT, so a stray VTODO in a windows directory would crash
  rather than be silently mishandled — but no code path puts one there.
- Its own state file is separate: `WINDOWS_BRIDGE_STATE_FILE`, default
  `/data/.local/share/caldir/windows-bridge-state.json`, `version: 1` (`:29-34`,
  `:214-236`) vs. the VTODO bridge's `version: 2` (`:427-450`).
- It has **no** `COMMON_FIELDS`/`RICH_FIELDS` at all; every write is
  `merge_canonical_into_google` (`:264-305`), canonical body + Google's
  `X-GOOGLE-*` lines only.

The only shared surface is the **run order** in `caldir/run-sync.sh`: the VTODO
bridge runs first, and a non-zero rc (2 on conflicts) ends the run, so a VTODO
conflict prevents `bridge-windows.py` and phase-2 publication from running at all
(`<vassago>/docs/sync-semantics.md:271-292`, `AGENTS.md` "Pipeline"). That is the
one way a VTODO change can affect windows: by blocking them.

### A4. Authority rules — `docs/sync-semantics.md` and ADR-0004

`docs/sync-semantics.md:3-10` states the whole model in four lines:

> Who wins, in every case. Three programs write calendar data, and each has a
> different authority rule:
>
> - `merge-ucrawler.py` — u_crawler wins on content, the local file wins on user
>   state (`STATUS`, `COMPLETED`, `PERCENT-COMPLETE`).
> - `bridge-vtodo.py` — two-way for a small set of shared fields; ambiguity is a
>   conflict, never a guess.
> - `bridge-windows.py` — canonical wins, always; Google edits are overwritten.

`docs/sync-semantics.md:41-46`:

> Everything else — `SUMMARY`, `DESCRIPTION`, `DUE`, `URL`, and any other
> property — comes from u_crawler and overwrites whatever was in the canonical
> file. Lines outside the `BEGIN:VTODO` / `END:VTODO` range are taken from the
> source as well, so calendar-level properties follow u_crawler.

`docs/adr/0004-authority-rules-deadlines-vs-windows.md:25-44` is the decision:

> **Deadlines — LMS owns content, the user owns task state, shared fields are
> bidirectional.**
>
> `merge-ucrawler.py` regenerates each canonical VTODO from the `u_crawler`
> source on every run, but before writing it strips `STATUS`, `COMPLETED` and
> `PERCENT-COMPLETE` out of the source body and re-attaches the values found in
> the existing canonical file. […]
>
> `bridge-vtodo.py` then treats a set of fields as *shared* and bidirectional:
> `SUMMARY`, `DESCRIPTION`, `DUE`, `STATUS`, `COMPLETED`. Whichever side changed
> since the last observation wins for those fields — canonical-to-Google when the
> canonical side changed, Google-to-canonical when Google changed. A second set
> of fields (`PRIORITY`, `URL`, `DTSTART`, `PERCENT-COMPLETE`, `LOCATION`,
> `ORGANIZER`, `ATTENDEE`, `ATTACH`, `VALARM`, and non-Google `X-` properties)
> flows canonical-to-Google only, because Google Tasks does not round-trip them.
> When both sides changed but their shared signatures agree, the change is not a
> disagreement and only the rich fields are pushed. When both changed and the
> shared signatures differ, it is a conflict.

And `:46-51`:

> Deletion is asymmetric on purpose. A hard delete in Google of a task whose
> canonical assignment is unchanged does **not** delete the canonical assignment:
> the bridge re-seeds the Google file from the canonical one.

**"What happens when both change"** — the operative code is
`bridge-vtodo.py:714-746`:

- both changed **and** `shared_signature(left) == shared_signature(right)` →
  *not* a disagreement; only rich fields are pushed canonical → Google
  (`:725-744`).
- both changed **and** signatures differ → `CONFLICT … both sides changed shared
  fields`, `conflicts += 1`, nothing written (`:715-723`). `main()` returns
  `2 if conflicts else 0` (`:848`), and `run-sync.sh` aborts before phase 2 —
  so **one conflict blocks publication of every calendar for that run**, and
  recurs every 15 minutes until resolved (`docs/sync-semantics.md:289-292`).

`AGENTS.md` "Hard rules" also states: *"NEVER change the bridge authority rules
(which side wins, when a conflict is raised) without a new ADR. Deletion of a
canonical assignment is never a bridge outcome."*

### A5. The three test files

All three use plain `unittest`, no fixture files on disk — fixtures are **inline
Python builder functions** and every test writes into a fresh
`tempfile.TemporaryDirectory`. The shared helper is `<vassago>/tests/_load.py`,
which executes the hyphen-named scripts as fresh, uncached modules after setting
env vars (`_load.py:23-43`) — necessary because `ROOT`/`GOOGLE_DIR`/`STATE_FILE`
are read at import time. `_load.write` (`:46-51`) writes fixtures with CRLF, "as
the scripts do".

**`tests/test_merge_ucrawler.py`** — 16 tests, docstring: *"Pin the current
behaviour of caldir/merge-ucrawler.py."*

The u_crawler VTODO fixture (`test_merge_ucrawler.py:12-26`) — this is the
canonical shape the tests treat as u_crawler output:

```python
def vtodo(uid="u_crawler-todo-1@u-crawler.local", summary="Assignment 1",
          extra=""):
    body = (
        "BEGIN:VCALENDAR\n"
        "VERSION:2.0\n"
        "PRODID:-//u_crawler//EN\n"
        "BEGIN:VTODO\n"
        f"UID:{uid}\n"
        "DTSTAMP:20260101T000000Z\n"
        f"SUMMARY:{summary}\n"
        "DUE:20260830T235900Z\n"
    )
    if extra:
        body += extra.rstrip("\n") + "\n"
    return body + "END:VTODO\nEND:VCALENDAR\n"
```

Note it carries **no `PRIORITY`, no `URL`, no `DESCRIPTION`, no `DTSTART`** —
i.e. the vassago tests model a *thinner* VTODO than u_crawler actually emits
today. Extra properties are injected through the `extra=` parameter.

Asserted behaviour: `unfold` joins continuations including lone-CR and tab
(`:59-68`); source content wins over existing (`:70-77`); the three user-state
fields are preserved from the existing file and the source's are dropped
(`:79-94`); user state absent from the existing file is dropped entirely rather
than falling back to the source's (`:96-102` — **so u_crawler's own
`STATUS:COMPLETED` never survives an update, only an initial create**); CRLF
output with a single trailing CRLF (`:104-110`); create/update/delete counting
including the deliberate "rewrites every run because of line endings" quirk
(`:133-149`); `personal.ics` is never removed (`:177-185`).

**`tests/test_bridge_vtodo.py`** — 26 tests. Fixture at `:14-30`, same shape,
with a `due=` parameter so date-only vs. datetime `DUE` can be varied. Asserts:
`normalize_due_date` / `google_due_from_canonical` / `canonical_due_from_google`
including time-of-day preservation (`:59-109`); `shared_signature` equal for
date-only and datetime on the same day and different across days (`:111-131`);
`semantic_hash` ignores `DTSTAMP`, `LAST-MODIFIED`, `SEQUENCE`, `X-GOOGLE-*`
(`:133-158`) and reacts to `SUMMARY` (`:160-167`); then every row of the
`reconcile` decision table — seed, Google-only conflict, unmanaged task untouched,
canonical delete, canonical delete + Google change conflict, Google delete
re-seed leaving canonical byte-identical, Google-only change → canonical, both
changed differently → conflict, canonical-only change → Google, duplicate UIDs,
missing mirror dir, `main()` rc 0/2, and version-mismatch state discard
(`:211-341`).

**`tests/test_bridge_windows.py`** — 15 tests. VEVENT fixture at `:14-29`.
Asserts `merge_canonical_into_google` replaces the body and retains only the
Google-side `X-GOOGLE-*` lines while dropping a stale canonical one (`:57-77`);
`semantic_hash` ignores `X-GOOGLE-*`/`SEQUENCE` (`:79-89`); and the reconcile
table including "Google edit is overwritten by canonical" (`:150-163`) and
"no change on either side is a no-op" (`:207-211`).

A fourth file, `tests/test_repo_invariants.py`, also exists and is picked up by
plain discovery.

---

## How to run vassago's tests

**There is no pytest, no venv, no `pyproject.toml`, no `conftest.py`, and no
dependencies.** The project is stdlib-only by hard rule (`AGENTS.md`: *"ALWAYS
stay on stdlib Python 3 and POSIX `sh`"*). The sanctioned runner is
`python3 -m unittest discover -s tests`, invoked from `scripts/check.sh:134`.
No network and no credentials are needed — every test works in a
`tempfile.TemporaryDirectory`.

`python3 -m pytest --version` in the clone returns
`/usr/bin/python3: No module named pytest`, so a literal
`python -m pytest tests/test_merge_ucrawler.py -q` **cannot run**. I did not
install anything and did not create a venv. Python in this environment is 3.14.7.

The `PYTHONDONTWRITEBYTECODE=1` / `PYTHONPYCACHEPREFIX=...` pair below is what
`scripts/check.sh:19-21` uses to keep `__pycache__` out of the tree; I pointed the
cache prefix at the scratchpad so the clone stayed byte-clean (`git status` was
empty before and after).

### Exact command

```sh
cd <scratch>/deps/vassago \
  && PYTHONDONTWRITEBYTECODE=1 \
     PYTHONPYCACHEPREFIX=<scratch>/pyc \
     python3 -m unittest discover -s tests -v -p 'test_merge_ucrawler.py'
```

### Exact output (test_merge_ucrawler.py)

```
test_merge_vtodo_drops_user_state_absent_from_existing (test_merge_ucrawler.PureHelpersTest.test_merge_vtodo_drops_user_state_absent_from_existing) ... ok
test_merge_vtodo_keeps_source_content (test_merge_ucrawler.PureHelpersTest.test_merge_vtodo_keeps_source_content) ... ok
test_merge_vtodo_output_is_crlf_with_single_trailing_crlf (test_merge_ucrawler.PureHelpersTest.test_merge_vtodo_output_is_crlf_with_single_trailing_crlf) ... ok
test_merge_vtodo_preserves_existing_user_state (test_merge_ucrawler.PureHelpersTest.test_merge_vtodo_preserves_existing_user_state) ... ok
test_unfold_accepts_lone_cr_and_tab_continuations (test_merge_ucrawler.PureHelpersTest.test_unfold_accepts_lone_cr_and_tab_continuations) ... ok
test_unfold_joins_continuation_lines (test_merge_ucrawler.PureHelpersTest.test_unfold_joins_continuation_lines) ... ok
test_changed_source_updates_target (test_merge_ucrawler.SyncDeadlinesTest.test_changed_source_updates_target) ... ok
test_course_without_deadlines_dir_is_skipped (test_merge_ucrawler.SyncDeadlinesTest.test_course_without_deadlines_dir_is_skipped) ... ok
test_merge_rewrites_every_run_because_of_line_endings (test_merge_ucrawler.SyncDeadlinesTest.test_merge_rewrites_every_run_because_of_line_endings) ... ok
test_new_source_file_is_created (test_merge_ucrawler.SyncDeadlinesTest.test_new_source_file_is_created) ... ok
test_removes_only_assignment_files_missing_from_source (test_merge_ucrawler.SyncDeadlinesTest.test_removes_only_assignment_files_missing_from_source) ... ok
test_update_preserves_user_state_in_target (test_merge_ucrawler.SyncDeadlinesTest.test_update_preserves_user_state_in_target) ... ok
test_identical_file_is_left_alone (test_merge_ucrawler.SyncWindowsTest.test_identical_file_is_left_alone) ... ok
test_local_edit_is_overwritten_unconditionally (test_merge_ucrawler.SyncWindowsTest.test_local_edit_is_overwritten_unconditionally) ... ok
test_new_file_is_copied (test_merge_ucrawler.SyncWindowsTest.test_new_file_is_copied) ... ok
test_removes_only_assignment_files_missing_from_source (test_merge_ucrawler.SyncWindowsTest.test_removes_only_assignment_files_missing_from_source) ... ok

----------------------------------------------------------------------
Ran 16 tests in 0.033s

OK
```

### The other two, and all four together

Swapping `-p` (`-p 'test_bridge_vtodo.py'`, `-p 'test_bridge_windows.py'`, or no
`-p` at all for full discovery). Summaries, with the scripts' own `[bridge]` /
`[windows-bridge]` stdout/stderr chatter elided:

```
### test_bridge_vtodo
Ran 26 tests in 0.108s
OK

### test_bridge_windows
Ran 15 tests in 0.082s
OK

### full discovery (python3 -m unittest discover -s tests)
Ran 65 tests in 0.204s
OK
```

The conflict-path tests print to stderr as designed, e.g.
`[bridge] CONFLICT u_crawler-todo-1@u-crawler.local: both sides changed shared fields`
and
`[windows-bridge] CONFLICT u_crawler-window-1@u-crawler.local: managed event exists only in Google`.
These are expected output, not failures.

The full project gate is `sh scripts/check.sh` (7 steps: shell/python syntax,
`docker compose config`, secret guard, term-prefix lockstep across five scripts,
Dockerfile COPY coverage, the unit tests, and `scripts/check_docs_paths.py`).
Step 2 self-skips when docker is unavailable (`check.sh:62-63`). I did not run
the full `check.sh` because step 3 shells out to `git ls-files` and step 2 may
touch docker; the tests are step 6 and were run directly above.

---

## B. u_crawler

### B7. How the deadline VTODO is rendered today

`src/calendar.rs:294-319`, `render_vtodo(uid, assignment, due, done)`:

```rust
let mut lines = vec![
    "BEGIN:VCALENDAR".to_string(),
    "VERSION:2.0".to_string(),
    "PRODID:-//u_crawler//calendar-sync//EN".to_string(),
    "BEGIN:VTODO".to_string(),
    format!("UID:{uid}"),
    format!("DTSTAMP:{}", ics_datetime(dtstamp)),
    format!("DUE:{}", ics_datetime(due)),
    format!("PRIORITY:{}", priority_for(assignment)),
];
if done { lines.push("STATUS:COMPLETED".to_string()); }
lines.push(format!("SUMMARY:{}", escape_text(assignment.name.as_deref().unwrap_or(""))));
if let Some(url) = &assignment.html_url { lines.push(format!("URL:{}", escape_text(url))); }
lines.push("END:VTODO".to_string());
lines.push("END:VCALENDAR".to_string());
lines.join("\r\n") + "\r\n"
```

**Property order (fixed):** `UID`, `DTSTAMP`, `DUE`, `PRIORITY`,
[`STATUS:COMPLETED`], `SUMMARY`, [`URL`].

**SUMMARY today** is `assignment.name` only — no course prefix, no due-date
suffix, no decoration (`:309-312`). **There is no `DESCRIPTION` today**, even
though `Assignment.description` is deserialised (`src/canvas.rs:220`); `calendar.rs`
never reads it. **There is no `DTSTART` on the VTODO today** — `unlock_at` is only
used for the sibling window `VEVENT` (`src/calendar.rs:457-477`).

**Escaping:** `escape_text` (`:190-195`) escapes `\` → `\\`, `;` → `\;`,
`,` → `\,`, `\n` → `\n`. It does **not** escape or strip `\r`, and does not strip
HTML. Canvas `description` is HTML and typically CRLF-delimited, so a bare `\r`
would survive into the ICS value; vassago's `unfold` turns lone `\r` into a line
break (`merge-ucrawler.py:21`, `bridge-vtodo.py:85`), splitting the property.

**Folding:** none. `lines.join("\r\n")` (`:318`) emits every property as a single
physical line regardless of length, violating RFC 5545 §3.1's 75-octet limit for
anything long. Today no emitted value is long; a `DESCRIPTION` would be.

**`DTSTAMP` is deliberately not `now`** — `dtstamp_for` (`:263-270`) derives it
from `assignment.updated_at` (RFC 3339) falling back to `due_at`, with the doc
comment at `:256-262` explaining that using `now` "would make every run emit
different content for unchanged data, defeating the 'no changes, empty plan'
guarantee".

**Content hash.** `content_hash` (`:177-181`) is **SHA-1, hex-encoded, over the
entire rendered `.ics` string's UTF-8 bytes** — envelope, CRLFs, trailing CRLF and
all:

```rust
fn content_hash(content: &str) -> String {
    let mut hasher = Sha1::new();
    hasher.update(content.as_bytes());
    hex::encode(hasher.finalize())
}
```

It is computed twice over the same string: once in `plan` for the comparison
(`:439`, `:462`) and once in `record_writes` for the recording (`:860`).

**State keys.** `calendar:{assignment_id}` for the VTODO
(`CALENDAR_KEY_PREFIX`, `:152`, `calendar_state_key` `:164-166`) and
`calendar-window:{assignment_id}` for the VEVENT (`WINDOW_KEY_PREFIX`, `:161`,
`window_state_key` `:170-172`). Both are deliberately distinct from the content
flow's `assignment:{id}`. The hash lives in `ItemState.content_hash`
(`src/state.rs:18`). The state file is **not** in the caldir tree — it is
`fsutil::course_dir(&download_root, course).join("state.json")`
(`src/calendar.rs:758`), i.e. the same per-course `state.json` the content sync
uses, under `download_root`.

**Rewrite vs. no-op** (`src/calendar.rs:441-450`):

```rust
let unchanged =
    prev.get(&key).and_then(|item| item.content_hash.as_deref()) == Some(hash.as_str());
if !unchanged { writes.push(PlannedWrite { .. }); }
```

The comparison is against **`state.json`, never the file on disk** (spec D5,
`docs/specs/calendar-sync-flow.md:110`). Consequences: (1) whatever the vassago
bridges rewrite in the canonical file is invisible to u_crawler and cannot
trigger a re-plan; (2) a missing/absent state entry is treated as changed, so a
wiped `state.json` produces exactly one write per component. The filename and UID
derive only from `assignment.id` (`:115-123`), never a date, so a due-date or
title change rewrites the same path (`:380-386`).

Applying is `apply_plan` (`:836-851`, `fsutil::atomic_write`), then
`record_writes` (`:858-873`) / `record_deletes` (`:883-887`), then `state.save`
(`:789`) — none of which runs under `--dry-run` (`:771-783`).

### B8. If u_crawler starts emitting `DESCRIPTION` and `DTSTART`

**(a) Does the content hash change → exactly one rewrite? YES.**
`content_hash` is SHA-1 over the whole rendered string (`src/calendar.rs:177-181`),
so adding any line changes it. The stored `calendar:{id}` hash will not match
(`:441-442`), so `plan` emits one `PlannedWrite` per affected assignment. Nothing
short-circuits per property — it is all-or-nothing on the rendered blob. On the
vassago side, `merge_vtodo` copies the new lines through verbatim
(`merge-ucrawler.py:101-116`), and the canonical file's `semantic_hash` changes,
so `bridge-vtodo.py` takes row 13 ("canonical changed, Google unchanged",
`:748-779`) and pushes once.

**(b) Does the SECOND run converge to an empty plan? YES — conditionally.**
`record_writes` stores exactly the hash of the string that was written
(`:858-873`), and `plan` recomputes the same string from the same inputs, so
`a_second_plan_against_the_recorded_state_is_empty` (`src/calendar.rs:1513-1533`)
generalises. Convergence holds **iff the new properties are pure functions of
Canvas data with no clock and no nondeterminism**:

- `DESCRIPTION` from `assignment.description` — stable per Canvas record. Safe,
  *provided* any HTML→text transformation you apply is deterministic (no hash-map
  iteration order, no locale-dependent formatting, no timestamps).
- `DTSTART` from `assignment.unlock_at` — stable. Safe.
- Anything derived from `now` would break it; the spec explicitly rejects
  clock-derived content for exactly this reason
  (`docs/specs/calendar-sync-flow.md:124`, and `dtstamp_for`'s doc at
  `src/calendar.rs:256-262`). Note `plan` already takes `_now` unused (`:419`).

Also note `plan` compares against `state.json`, not the file, so the bridges'
rewrites of the canonical file cannot un-converge u_crawler.

**(c) Do the vassago scripts preserve or fight the new properties?**

*`merge-ucrawler.py`: preserves, no change needed.* It is property-agnostic —
only `STATUS`, `COMPLETED`, `PERCENT-COMPLETE` are special
(`merge-ucrawler.py:13-17`, `:96-118`). `DESCRIPTION` and `DTSTART` come from the
source and overwrite the canonical, which is exactly the documented rule
(`docs/sync-semantics.md:41-43`).

*`bridge-vtodo.py`: carries both, but the two behave very differently.*

- **`DTSTART` is `RICH_FIELDS`** (`:47-57`) — canonical → Google only, pushed by
  `copy_rich_fields_to_google` (`:327-372`), never read back, never normalised.
  **Latent churn, not a conflict.** Google Tasks cannot store a start date on a
  task, so once phase 2 pushes and phase 1 of the next run pulls back, the mirror
  file returns without `DTSTART`; its `semantic_hash` then differs from the
  recorded `google_hash`, so `right_changed` is true and every run takes row 14
  ("canonical unchanged, Google changed", `:781-813`), re-pushing `DTSTART` and
  logging `[bridge] canonical <- Google <uid>` and `1 changed`, forever. This is
  **not** a conflict path — row 14 never increments `conflicts` — so rc stays 0
  and the pipeline keeps running. **Crucially, this exposure already exists
  today** for `PRIORITY` and `URL`, which are also `RICH_FIELDS` and already
  emitted by `render_vtodo` (`:304`, `:313-315`). Adding `DTSTART` adds nothing
  new in kind. This is why `docs/sync-semantics.md:101-104` says rich fields
  "are carried in the local mirror file so a CalDAV client reading it still sees
  them, but a change to them on the Google side is meaningless and is discarded
  on the next push."

- **`DESCRIPTION` is `COMMON_FIELDS`** (`:39-45`) — **bidirectional, and it is one
  of the five inputs to `shared_signature`** (`:193-214`). This is the real risk.
  Google Tasks *does* store notes, so `DESCRIPTION` round-trips, but Google is
  free to normalise it (trim, re-wrap, strip or re-escape markup, truncate — the
  Tasks notes field has a documented length cap). If what comes back differs by
  even one byte after unfolding, then:
  - `semantic_hash(google)` changes → `right_changed` (`:637-639`);
  - the ordinary case is row 14 (`:781-813`): **Google's normalised
    `DESCRIPTION` is merged INTO the canonical file** — a Google-side edit
    overwriting the LMS-derived text;
  - the very next `merge-ucrawler.py` run restores u_crawler's version, because
    `DESCRIPTION` is not in `USER_STATE_FIELDS` (`merge-ucrawler.py:13-17`), so
    `left_changed` becomes true too;
  - if both flags land in the same run, `bridge-vtodo.py:714-723` fires:
    `shared_signature` differs (the descriptions differ) → **`CONFLICT … both
    sides changed shared fields`**, `main()` returns 2 (`:848`), and
    `run-sync.sh` aborts **before phase 2**, blocking publication of *every*
    calendar including the windows, recurring every 15 minutes until someone
    deletes that UID's entry from `vtodo-bridge-state.json`
    (`docs/sync-semantics.md:271-292`, `:342-345`).

  So: **a permanent write loop is likely for `DTSTART` (benign, already the
  status quo for `PRIORITY`/`URL`), and a sticky rc-2 conflict loop is possible
  for `DESCRIPTION` (not benign)** — the difference is entirely that
  `DESCRIPTION` is bidirectional and `DTSTART` is not.

  Mitigations that follow directly from the code: emit a `DESCRIPTION` whose
  exact bytes survive a Google Tasks round-trip — plain text, no HTML, no CRLF,
  short (well under Google's notes cap), no trailing whitespace. Both hashes are
  computed over *unfolded logical lines* (`:84-96`, `:167-176`), so **line
  folding is safe**: fold or don't fold, the hash is the same. Property order is
  also safe (`sorted(item.props)`, `:167`).

**(d) Is any user state at risk?**

- **Completion state: structurally safe.** `STATUS`, `COMPLETED` and
  `PERCENT-COMPLETE` are excluded from the u_crawler-wins path by
  `USER_STATE_FIELDS` (`merge-ucrawler.py:13-17`, `:96-118`), pinned by
  `test_merge_vtodo_preserves_existing_user_state`
  (`tests/test_merge_ucrawler.py:79-94`). Adding `DESCRIPTION`/`DTSTART` does not
  touch that set. A canonical hard-delete is still never a bridge outcome
  (`bridge-vtodo.py:672-706`, ADR-0004:46-51).
- **Indirect risk, real:** a sticky `DESCRIPTION` conflict returns rc 2, which
  stops phase 2, so a completion the user ticked in Google Tasks that run is
  **not published back** until the conflict is cleared. Nothing is lost on disk,
  but the sync is dead until a human deletes the UID's state entry.
- **Direct risk, real but narrow:** row 14 (`:781-813`) writes Google's
  `DESCRIPTION` into the canonical file. That is a Google-authored value landing
  in an LMS-authoritative field. `merge-ucrawler.py` repairs it on the next run
  (source wins for `DESCRIPTION`), so the damage is transient — but it is exactly
  the oscillation that produces the conflict above.
- **Pre-existing quirk worth knowing (not caused by this change):**
  u_crawler's own `STATUS:COMPLETED` (emitted at `src/calendar.rs:306-308` when
  Canvas says submitted/graded) is **dropped by `merge-ucrawler.py` on every
  update** if the existing canonical file has no `STATUS` — pinned by
  `test_merge_vtodo_drops_user_state_absent_from_existing`
  (`tests/test_merge_ucrawler.py:96-102`). Canvas-derived completion therefore
  only reaches the canonical file on the *initial create* (`merge-ucrawler.py:163-166`).

**Additional concrete recommendations from the code, if `DTSTART` is added:**

- RFC 5545 §3.6.2 requires `DTSTART` ≤ `DUE` on a `VTODO`. u_crawler already
  guards `unlock < due` for the VEVENT (`src/calendar.rs:457-458`); the same
  guard must be applied before emitting `DTSTART`, or an `unlock_at` after
  `due_at` produces an invalid component.
- `DTSTART` is not date-normalised anywhere in the bridge (`replace_fields`'s
  special case is `if field != "DUE"`, `bridge-vtodo.py:291`), so whatever format
  u_crawler emits is what the mirror carries. Emit the same `Z`-suffixed UTC form
  as `DUE` (spec D9, `src/calendar.rs:185-187`) for consistency.
- Fix or avoid the `\r` gap in `escape_text` (`src/calendar.rs:190-195`) before
  putting Canvas HTML into a `DESCRIPTION`: a bare CR becomes a line break under
  vassago's `unfold` (`merge-ucrawler.py:21`), which silently splits the property.
- Consider folding at 75 octets in `render_vtodo` (`:318` currently does not),
  since a `DESCRIPTION` will exceed it. Folding is safe for both vassago hashes.

---

## Convergence analysis

| Question | Answer | Deciding code |
| --- | --- | --- |
| u_crawler run 1 after adding `DESCRIPTION` + `DTSTART` | exactly one write per affected assignment | `src/calendar.rs:439`, `:441-450` |
| u_crawler run 2 | **empty plan**, provided both new values are pure functions of Canvas data (no clock, no nondeterminism) | `:441-442` vs `:858-873`; test `:1513-1533` |
| Does a bridge rewriting the canonical file re-trigger u_crawler? | **No** — u_crawler compares against `state.json`, never the file | `:441-442`, `:758-759`; spec D5 `docs/specs/calendar-sync-flow.md:110` |
| `merge-ucrawler.py` steady state | rewrites byte-identical content every run and reports `updated` (pre-existing CRLF-vs-LF quirk) | `merge-ucrawler.py:168`, `:180`; test `test_merge_ucrawler.py:133-149` |
| `bridge-vtodo.py` steady state for `DTSTART` | **non-converging but benign**: row 14 fires every run, `1 changed`, rc 0. Same as today's `PRIORITY`/`URL` | `bridge-vtodo.py:47-57`, `:781-813` |
| `bridge-vtodo.py` steady state for `DESCRIPTION` | converges **iff** Google returns the bytes it was given; otherwise row 14 ping-pong that can escalate to a sticky rc-2 conflict | `:39-45`, `:193-214`, `:714-723`, `:781-813` |
| Blast radius of that conflict | phase 2 never runs → **no calendar publishes at all**, recurring every 15 min until the UID's state entry is deleted | `docs/sync-semantics.md:271-292`, `:342-345`; `bridge-vtodo.py:848` |
| Is `bridge-windows.py` affected? | **No**, except by being skipped when the VTODO bridge exits 2 | `bridge-windows.py:36`, `:89-102`, `:166-168`; `AGENTS.md` Pipeline |
| Is user completion state at risk? | Not structurally; only indirectly, via a blocked phase 2 | `merge-ucrawler.py:13-17`, `:96-118` |

**Bottom line.** Adding `DTSTART` is low-risk and needs no vassago change.
Adding `DESCRIPTION` needs no vassago *code* change either, but it moves a large,
LMS-authored, HTML-derived blob into the bidirectional `COMMON_FIELDS` set that
`shared_signature` is computed from — turning any Google-side normalisation of
that text into a pipeline-halting conflict. Either make the emitted
`DESCRIPTION` provably round-trip-stable through Google Tasks notes, or open an
ADR in vassago to move `DESCRIPTION` from `COMMON_FIELDS` to `RICH_FIELDS`
(canonical → Google only) — noting that `AGENTS.md` requires a new ADR for any
change to the bridge authority rules.

---

## Sources

vassago (read-only clone; unmodified):

- `<vassago>/caldir/merge-ucrawler.py` — `:9`, `:13-17`, `:20-32`, `:35-39`,
  `:80-118`, `:121-132`, `:135-194`, `:197-234`, `:237-250`
- `<vassago>/caldir/bridge-vtodo.py` — `:23-35`, `:37`, `:39-65`, `:68-81`,
  `:84-96`, `:99-108`, `:126-147`, `:164-179`, `:182-190`, `:193-214`,
  `:217-225`, `:228-261`, `:264-324`, `:327-372`, `:375-424`, `:427-450`,
  `:474-492`, `:499-522`, `:525-822` (rows at `:548`, `:553-566`, `:569-580`,
  `:585-593`, `:595-614`, `:643-669`, `:672-706`, `:711-712`, `:714-746`,
  `:748-779`, `:781-813`), `:825-852`
- `<vassago>/caldir/bridge-windows.py` — `:22-34`, `:36-44`, `:89-102`,
  `:145-160`, `:163-211`, `:214-236`, `:264-305`, `:308-316`, `:340-552`,
  `:494-496`, `:555-585`
- `<vassago>/docs/sync-semantics.md` — `:3-10`, `:15-56`, `:82-192`, `:193-234`,
  `:271-292`, `:294-345`
- `<vassago>/docs/adr/0004-authority-rules-deadlines-vs-windows.md` — `:23-62`,
  `:64-84`, `:86-116`
- `<vassago>/tests/_load.py` — `:1-51`
- `<vassago>/tests/test_merge_ucrawler.py` — `:12-26`, `:59-110`, `:113-191`,
  `:194-241`
- `<vassago>/tests/test_bridge_vtodo.py` — `:11-30`, `:59-167`, `:170-341`
- `<vassago>/tests/test_bridge_windows.py` — `:11-29`, `:32-89`, `:92-248`
- `<vassago>/scripts/check.sh` — `:9-21`, `:131-134`
- `<vassago>/AGENTS.md` — Pipeline, Directory layout, Hard rules
- `<vassago>/CLAUDE.md`

u_crawler (`<repo>`, branch
`feat/calendar-rich-vtodo`, read-only):

- `<repo>/src/calendar.rs` — `:1-29` (module doc),
  `:43-85`, `:91-97`, `:115-142`, `:152-172`, `:177-181`, `:185-195`,
  `:207-232`, `:245-254`, `:256-270`, `:294-319`, `:326-353`, `:355-512`,
  `:514-540`, `:699-800`, `:836-851`, `:853-887`, tests at `:952-1003`,
  `:1222-1229`, `:1493-1533`
- `<repo>/src/canvas.rs` — `:217-240`
- `<repo>/src/state.rs` — `:8-23`, `:25-72`
- `<repo>/src/config.rs` — `:55-79`, `:205`
- `<repo>/docs/specs/calendar-sync-flow.md` —
  D3 (`:82-89`), D5 (`:99-112`), D6 (`:114-124`), D7 (`:126-130`), D9 (`:136-138`),
  D10 (`:140-151`), Testing Decisions (`:167-188`), spike findings (`:203-226`)
- `<repo>/AGENTS.md` — `:31-32` (exclusive
  ownership of `calendar.caldir_root`), `:131`, `:182`, `:225-255`
