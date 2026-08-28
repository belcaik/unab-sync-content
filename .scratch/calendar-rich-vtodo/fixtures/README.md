# Compatibility fixtures

Two canonical `.ics` files, produced by the real planner (`calendar::plan`)
and used as input to the external-compatibility evidence in
`../compat-report.md` (ticket #15, spec ID8 / "Verificación externa").

| File | Canvas input |
|---|---|
| `deadline-full.ics` | course `Cálculo Diferencial` / `MAT1101`, assignment `90210` "Sumativa 5: Informe de laboratorio", `updated_at` 2026-09-01T18:30:00Z, `unlock_at` 2026-09-09T14:00:00Z (strictly before `due_at`), `due_at` 2026-09-16T23:59:00Z, `html_url`, 30 points, `online_upload` |
| `deadline-no-unlock.ics` | the same assignment with `unlock_at` absent |

Between them they carry every property the deadline `VTODO` can emit:
`UID`, `DTSTAMP`, `DTSTART` (present in one, absent in the other), `DUE`,
`PRIORITY`, `SUMMARY`, `DESCRIPTION` (three logical lines, RFC 5545 §3.1
folded across a non-ASCII character) and `URL`. `STATUS` is absent from both
by design — no submission was supplied, and §3.8.1.11 already reads an absent
`STATUS` on a `VTODO` as "needs action".

## How they were generated, and why they cannot drift

They were written out by a throwaway `#[test]` calling `plan` with exactly the
inputs above; the test was then deleted. Nothing hand-typed them.

What stops them going stale is
`calendar::tests::the_compatibility_fixtures_render_these_exact_bytes`, which
pins the *same* bytes as a hand-written literal in `src/calendar.rs`. If the
renderer changes, that test fails and these files must be regenerated in the
same commit.

Do not edit these files by hand.
