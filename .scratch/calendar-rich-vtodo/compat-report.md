# Compatibility report: the enriched deadline `VTODO` through the real pipeline

Ticket #15 (`.scratch/calendar-rich-vtodo/issues/03-regresion-idempotencia-y-contrato-e2e.md`),
spec ID8 / "Verificación externa". Date: 2026-08-28.

This is **evidence, not a test suite of this repo**. Everything below was run
by hand against read-only clones; the commands and their output are quoted
verbatim so the claims can be re-checked. Where a hop could not be verified it
is marked as such rather than glossed over.

## What was fed in

The two fixtures under `fixtures/` (see that directory's `README.md`), produced
by `calendar::plan` and pinned byte-for-byte by
`calendar::tests::the_compatibility_fixtures_render_these_exact_bytes`:

- `deadline-full.ics` — `DTSTART` present (`unlock_at` strictly before `due_at`)
- `deadline-no-unlock.ics` — no `unlock_at`, therefore no `DTSTART`

Both carry a non-ASCII course name (`Cálculo Diferencial`) whose `á` sits inside
the folded region of `DESCRIPTION`, and a three-logical-line `DESCRIPTION`
folded across two continuation lines per RFC 5545 §3.1.

## Hop 1 — caldir (`vtodo-support`, HEAD `02ee000`)

### How

caldir's own tests do not parse an external file, and the clone must not be
modified, so a throwaway crate outside it depends on `caldir-core` by path and
drives the public API: `CalendarItem::from_single_ics_str` → inspect →
`Todo::to_ics_string` → `CalendarItem::from_single_ics_str` again. `icalendar`
is pinned to `=0.17.10`, exactly what caldir's own `Cargo.lock` resolves, so the
probe exercises the codec caldir ships.

Probe source: `/home/belcaik/.cache/u_crawler-compat-probe/src/main.rs` (outside
both repos; not committed here).

```
cd /home/belcaik/.cache/u_crawler-compat-probe
CARGO_TARGET_DIR=/home/belcaik/.cache/u_crawler-compat-targets/probe cargo run --quiet -- \
  <repo>/.scratch/calendar-rich-vtodo/fixtures/deadline-full.ics \
  <repo>/.scratch/calendar-rich-vtodo/fixtures/deadline-no-unlock.ics
```

Exit code `0`, output ending in `ALL CHECKS PASSED`.

### What it showed, per property

For `deadline-full.ics`:

```
  kind                 = VTODO  (CalendarItem::Todo)  OK
  uid                  = u_crawler-todo-90210@u-crawler.local
  summary              = Some("Cálculo Diferencial - Sumativa 5: Informe de laboratorio")
  description          = Some("Cálculo Diferencial - Sumativa 5: Informe de laboratorio\nDisponible: 2026-09-09T14:00:00Z - Vence: 2026-09-16T23:59:00Z\nhttps://canvas.example.edu/courses/4210/assignments/90210")
  start   (DTSTART)    = DateTimeUtc(2026-09-09T14:00:00Z)
  due     (DUE)        = DateTimeUtc(2026-09-16T23:59:00Z)
  url     (URL)        = Some("https://canvas.example.edu/courses/4210/assignments/90210")
  priority (PRIORITY)  = Some(1)
  status  (STATUS)     = NeedsAction
```

and every round-trip check passed:

```
  round trip UID                      OK
  round trip SUMMARY                  OK
  round trip DESCRIPTION              OK
  round trip DTSTART                  OK
  round trip DUE                      OK
  round trip URL                      OK
  round trip PRIORITY                 OK
  round trip STATUS                   OK
  round trip whole Todo (PartialEq)   OK
  round trip no DTEND emitted         OK
  round trip no VEVENT emitted        OK
  round trip VTODO emitted            OK
```

`deadline-no-unlock.ics` gave the same, with `start (DTSTART) = <absent>` — the
absence is carried as an absence, not as a fabricated value.

Notable details from caldir's re-serialization:

- It re-emits the folded `DESCRIPTION` **at the same fold points** we chose,
  including the fold that lands after the `á`. The multi-line `DESCRIPTION`
  guarantee, which the research notes flagged as **UNVERIFIED for `VTODO`
  specifically** (only inherited from the `VEVENT` path), is now verified for
  `VTODO` with a real non-ASCII, folded value.
- `DTSTAMP` is re-stamped fresh, as documented; `PRODID` becomes `CALDIR`.
  Neither is content, and neither is hashed downstream.
- Property order changes (caldir emits alphabetically-ish). Irrelevant: nothing
  downstream is order-sensitive.

### Cross-check

caldir's own VTODO codec suite, unmodified clone:

```
cd <caldir clone>
CARGO_TARGET_DIR=/home/belcaik/.cache/u_crawler-compat-targets/caldir cargo test -p caldir-core todo
→ test result: ok. 48 passed; 0 failed; 0 ignored; 0 measured; 614 filtered out
```

## Hop 2 — vassago (HEAD `2f3f0f4`)

### The named test files, unmodified clone

```
cd <vassago clone>
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s tests -p 'test_merge_ucrawler.py'
→ Ran 16 tests in 0.025s / OK   (exit 0)

PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s tests -p 'test_bridge_vtodo.py'
→ Ran 26 tests in 0.088s / OK   (exit 0)

PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s tests -p 'test_bridge_windows.py'
→ Ran 15 tests in 0.061s / OK   (exit 0)

PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s tests
→ Ran 65 tests in 0.172s / OK   (exit 0)
```

These run against vassago's **own** fixtures. They prove vassago is unbroken;
they do not by themselves prove anything about the new shape. For that, the
fixture was driven through the real scripts.

### The new shape through the real scripts

A temporary `CALENDAR_ROOT` **outside** the clone
(`/home/belcaik/.cache/vassago-demo.*`) held
`202615_Calculo_Diferencial_MAT1101/deadlines/assignment-90210.ics` =
`deadline-full.ics`, plus an empty `unab/` standing in for the Google Tasks
mirror directory that `caldir` provisions. Then, from the clone:

```
CALENDAR_ROOT=$D GOOGLE_TASKS_DIR=$D/unab BRIDGE_STATE_FILE=$D/vtodo-bridge-state.json \
  python3 caldir/merge-ucrawler.py && python3 caldir/bridge-vtodo.py     # x3 cycles
```

```
### cycle 1
[u_crawler-merge] deadlines: 1 created, 0 updated, 0 removed; windows: 0 updated, 0 removed
[bridge] seed Google <- u_crawler-todo-90210@u-crawler.local
[bridge] done: 1 changed, 0 deleted, 0 conflicts        rc=0
### cycle 2
[u_crawler-merge] deadlines: 0 created, 1 updated, 0 removed; windows: 0 updated, 0 removed
[bridge] done: 0 changed, 0 deleted, 0 conflicts        rc=0
### cycle 3
[u_crawler-merge] deadlines: 0 created, 1 updated, 0 removed; windows: 0 updated, 0 removed
[bridge] done: 0 changed, 0 deleted, 0 conflicts        rc=0
```

**Bridge converges after one cycle: `0 changed, 0 conflicts`, rc 0, forever
after.** That is the "se fusiona, se hashea y converge" criterion, met.

The Google-Tasks mirror vassago produced from the new shape:

```
BEGIN:VCALENDAR
VERSION:2.0
PRODID:-//u_crawler//calendar-sync//EN
BEGIN:VTODO
UID:u_crawler-todo-90210@u-crawler.local
DTSTAMP:20260901T183000Z
DTSTART:20260909T140000Z
PRIORITY:1
URL:https://canvas.example.edu/courses/4210/assignments/90210
DESCRIPTION:Cálculo Diferencial - Sumativa 5: Informe de laboratorio\nDisponible: 2026-09-09T14:00:00Z - Vence: 2026-09-16T23:59:00Z\nhttps://canvas.example.edu/courses/4210/assignments/90210
SUMMARY:Cálculo Diferencial - Sumativa 5: Informe de laboratorio
DUE;VALUE=DATE:20260916
END:VTODO
END:VCALENDAR
```

`DESCRIPTION` is present and complete in the mirror — which is the whole point
of ID2, since `notes` is the only free-text field that reaches Google. Note
`DUE;VALUE=DATE:20260916`: the time of day is already gone at this hop, exactly
as ID3 predicted, and the exact hour survives only inside the `DESCRIPTION`
text.

### User state is not lost

`STATUS:COMPLETED` and `PERCENT-COMPLETE:100` were written into the canonical
file (simulating Thunderbird / Google), then `merge-ucrawler.py` was run again
with the same u_crawler output (which carries no `STATUS`):

```
  canonical STATUS lines after merge:
PERCENT-COMPLETE:100
STATUS:COMPLETED
```

Preserved, and still preserved after two further merges. The bridge then pushed
the completion to the mirror once and converged:

```
[bridge] Google <- canonical u_crawler-todo-90210@u-crawler.local
[bridge] done: 1 changed, 0 deleted, 0 conflicts   rc=0
[bridge] done: 0 changed, 0 deleted, 0 conflicts   rc=0
[bridge] done: 0 changed, 0 deleted, 0 conflicts   rc=0
```

### No repeated conflicts, including on `DESCRIPTION`

A Google-side-only edit of `DESCRIPTION` was simulated in the mirror. The bridge
carried it back to canonical without a conflict (`1 changed, 0 conflicts`,
rc 0); the next `merge-ucrawler.py` overwrote it with u_crawler's text
(`DESCRIPTION` is not in vassago's `USER_STATE_FIELDS`, so Canvas wins, which is
the intended direction — spec D5); the bridge then pushed that back once and
settled:

```
bridge run 1: rc=0  [bridge] done: 1 changed, 0 deleted, 0 conflicts
bridge run 2: rc=0  [bridge] done: 0 changed, 0 deleted, 0 conflicts
bridge run 3: rc=0  [bridge] done: 0 changed, 0 deleted, 0 conflicts
bridge run 4: rc=0  [bridge] done: 0 changed, 0 deleted, 0 conflicts
```

No `CONFLICT … both sides changed shared fields`, no rc 2, no loop. The residual
risk the spec documents (Google *normalizing* `notes` rather than a user editing
them) is not reproducible without a real Google account and remains a documented
risk, not an observed failure.

### Two vassago quirks observed, both pre-existing

1. **`merge-ucrawler.py` reports `1 updated` on every run forever**, even when
   the file it writes is byte-identical. `sha1sum` before and after the rewrite
   matched (`cd57e47c…` both times). Cause: `Path.read_text()` translates the
   canonical file's `CRLF` to `LF` under universal newlines, while
   `merge_vtodo` returns `CRLF`, so `merged_raw != existing_raw` is always
   true. Reproduced identically with a **pre-ticket-shaped** `VTODO` (no
   `DESCRIPTION`, no folding):
   `1 created` then `1 updated`, `1 updated`, … So this is not caused by this
   change. It is a cosmetic counter, not a real write churn.
2. **The canonical copy is un-folded.** `merge-ucrawler.py`'s `unfold` runs on
   the way in and nothing re-folds on the way out, so the canonical
   `DESCRIPTION` sits on one physical line well past RFC 5545 §3.1's 75-octet
   SHOULD. That is vassago's doing, not ours; caldir re-folds when it writes,
   and `bridge-vtodo.py` hashes logical lines, so nothing downstream cares.

## Per-property summary

| Property | caldir parse | caldir local round trip | vassago canonical | vassago Google mirror | Google Tasks |
|---|---|---|---|---|---|
| `UID` | `Todo.uid` | preserved | preserved (identity key) | preserved | not a field; identity via `X-GOOGLE-TASK-ID` |
| `DTSTAMP` | not modelled | re-stamped | preserved | preserved, `IGNORE_FOR_HASH` | n/a |
| `SUMMARY` | `Todo.summary` | preserved, UTF-8 intact | preserved | preserved | **not verified** (would be `title`) |
| `DESCRIPTION` | `Todo.description` | preserved, folds and `\n` escapes intact | preserved (unfolded) | **present and complete** | **not verified** (would be `notes`) |
| `DTSTART` | `Todo.start` (`DateTimeUtc`) | preserved | preserved | preserved (`RICH_FIELDS`, canonical → Google) | **never sent** — verified limitation |
| `DUE` | `Todo.due` (`DateTimeUtc`) | preserved | preserved | **`DUE;VALUE=DATE:20260916` — time of day lost here** | **not verified**; date only |
| `URL` | `Todo.url` | preserved | preserved | preserved (`RICH_FIELDS`) | **never sent** — verified limitation |
| `PRIORITY` | `Todo.priority` | preserved | preserved | preserved (`RICH_FIELDS`) | **never sent** — verified limitation |
| `STATUS` | `TodoStatus` | preserved | **preserved from user, never overwritten** | preserved (`COMMON_FIELDS`) | **not verified** |

## What could NOT be verified, and why

1. **The Google Tasks hop itself.** Nobody here has credentials and writes
   against a real Google account are out of scope for this ticket
   (spec "Out of Scope"). Every "Google Tasks" claim above is a **verified
   limitation carried over from the research notes**
   (`research/02-google-tasks-api.md`, `research/03-caldir-vtodo-support.md`),
   read out of caldir's `to_google.rs` and Google's own Discovery Document —
   **not a passing test**. In particular: `DTSTART`, `URL` and `PRIORITY` are
   never put on the wire, and `due` is date-only. Do not read this report as
   end-to-end proof that a task appears correctly in Google Tasks.
2. **A real CalDAV server (Radicale).** `caldir push` was not run; no server was
   started. The spike in `docs/specs/calendar-sync-flow.md` covers that hop and
   is not re-verified here.
3. **Google normalizing `notes`.** The sticky-`CONFLICT` risk the spec names can
   only be triggered by Google's own normalization, which needs a live account.
   Not reproducible here; still a documented residual risk.
4. **caldir's `URL`-as-`TEXT` escaping edge.** No observed Canvas URL contains a
   comma, and the fixtures do not either, so the known debt noted in
   `AGENTS.md` / spec ID9 is untested here as well.
