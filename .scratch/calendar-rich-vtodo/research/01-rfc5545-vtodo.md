# RFC 5545 (iCalendar) — VTODO rules for a Canvas-assignment → VTODO generator

Research date: 2026-08-28
Primary source: RFC 5545, "Internet Calendaring and Scheduling Core Object Specification (iCalendar)", B. Desruisseaux, Ed., September 2009 (Standards Track, obsoletes RFC 2445).
All quotations below are verbatim from the RFC text (https://www.rfc-editor.org/rfc/rfc5545.txt), with the section number given per claim.

---

## Verdict (design answers)

- **`DTSTART` + `DUE` together is only legal when `DUE` is strictly later than `DTSTART`.** §3.8.2.3 says the value of `DUE` "MUST be later in time than the value of the 'DTSTART' property." So if `unlock_at >= due_at` — including exact equality — emitting both violates a **MUST**. **Omit `DTSTART` in that case.** Omitting it is always RFC-legal (`dtstart` is listed as OPTIONAL in §3.6.2), so it is the safe branch.
- **The value types of `DTSTART` and `DUE` must match.** §3.8.2.3: "the value type of this property MUST be the same as the 'DTSTART' property". Never pair a `VALUE=DATE` `DUE` with a `DATE-TIME` `DTSTART` (or vice versa). Also: `DUE` must be a date-with-local-time **iff** `DTSTART` is (§3.8.2.3), so if one is UTC (`Z`) the other must be too.
- **`DTEND` is never valid in a `VTODO`.** §3.8.2.2 Conformance limits it to `VEVENT`/`VFREEBUSY`, and the `todoprop` ABNF in §3.6.2 does not list it. Use `DUE`.
- **`DUE` and `DURATION` are mutually exclusive** in a `VTODO` (§3.6.2), and `DURATION` additionally requires `DTSTART`. Emitting `DUE` only (the natural Canvas mapping) is correct and needs no `DTSTART`.
- **A `VTODO` with neither `DTSTART` nor `DUE`/`DURATION`** "specifies a to-do that will be associated with each successive calendar date, until it is completed" (§3.6.2) — i.e., a perpetual/floating task. So an assignment with no `due_at` should either be emitted with no date properties (perpetual to-do) or be skipped, by product choice; both are RFC-legal.
- **Only `DTSTAMP` and `UID` are REQUIRED** in a `VTODO`, each at most once (§3.6.2). `SUMMARY`, `DESCRIPTION`, `URL`, `PRIORITY`, `STATUS`, `DTSTART` are OPTIONAL and MUST NOT occur more than once each.
- **Escape `\`, `;`, `,` and encode newlines as `\n`/`\N` in every TEXT value** (`SUMMARY`, `DESCRIPTION`, `UID`, `STATUS`) — §3.3.11. **Do NOT escape `:`** ("SHALL NOT be escaped"), and do NOT escape `"`. **`URL` is a URI value type, not TEXT** (§3.8.4.6, §3.3.13) — apply **no** backslash escaping to it.
- **Serialization mechanics:** CRLF line breaks (§3.1); fold lines longer than 75 octets, excluding the line break, with `CRLF` + one SPACE/HTAB (§3.1); UTC date-times as `YYYYMMDDTHHMMSSZ`, e.g. `19980119T070000Z` (§3.3.5 FORM #2), and `TZID` "MUST NOT be applied" to UTC values. `PRIORITY` is an INTEGER in `[0..9]`, 1 highest, 9 lowest, 0 = undefined (§3.8.1.9). `STATUS` in a `VTODO` is one of `NEEDS-ACTION` / `COMPLETED` / `IN-PROCESS` / `CANCELLED` (§3.8.1.11).

---

## 1. §3.6.2 — Which properties may appear in a `VTODO`, and how many times

§3.6.2 defines the component grammar. Verbatim:

```
 todoc      = "BEGIN" ":" "VTODO" CRLF
              todoprop *alarmc
              "END" ":" "VTODO" CRLF

 todoprop   = *(
            ;
            ; The following are REQUIRED,
            ; but MUST NOT occur more than once.
            ;
            dtstamp / uid /
            ;
            ; The following are OPTIONAL,
            ; but MUST NOT occur more than once.
            ;
            class / completed / created / description /
            dtstart / geo / last-mod / location / organizer /
            percent / priority / recurid / seq / status /
            summary / url /
            ;
            ; The following is OPTIONAL,
            ; but SHOULD NOT occur more than once.
            ;
            rrule /
            ;
            ; Either 'due' or 'duration' MAY appear in
            ; a 'todoprop', but 'due' and 'duration'
            ; MUST NOT occur in the same 'todoprop'.
            ; If 'duration' appear in a 'todoprop',
            ; then 'dtstart' MUST also appear in
            ; the same 'todoprop'.
            ;
            due / duration /
            ;
            ; The following are OPTIONAL,
            ; and MAY occur more than once.
            ;
            attach / attendee / categories / comment / contact /
            exdate / rstatus / related / resources /
            rdate / x-prop / iana-prop
            ;
            )
```

Answering the specific properties asked about (all per §3.6.2 unless noted):

| Property | Status in VTODO | Cardinality |
|---|---|---|
| `DTSTAMP` | **REQUIRED** | MUST NOT occur more than once (also §3.8.7.2: "This property MUST be included in the 'VEVENT', 'VTODO', 'VJOURNAL', or 'VFREEBUSY' calendar components.") |
| `UID` | **REQUIRED** | MUST NOT occur more than once (also §3.8.4.7: "The property MUST be specified in the 'VEVENT', 'VTODO', 'VJOURNAL', or 'VFREEBUSY' calendar components.") |
| `DTSTART` | OPTIONAL | MUST NOT occur more than once (also §3.8.2.4: "This property can be specified once in the 'VEVENT', 'VTODO', or 'VFREEBUSY' calendar components") |
| `DUE` | OPTIONAL | at most once, and mutually exclusive with `DURATION` (also §3.8.2.3: "The property can be specified once in a 'VTODO' calendar component.") |
| `SUMMARY` | OPTIONAL | MUST NOT occur more than once |
| `DESCRIPTION` | OPTIONAL | MUST NOT occur more than once (§3.8.1.5: "The property can be specified multiple times only within a 'VJOURNAL' calendar component.") |
| `URL` | OPTIONAL | MUST NOT occur more than once (§3.8.4.6: "This property can be specified once in the 'VEVENT', 'VTODO', 'VJOURNAL', or 'VFREEBUSY' calendar components.") |
| `PRIORITY` | OPTIONAL | MUST NOT occur more than once |
| `STATUS` | OPTIONAL | MUST NOT occur more than once (§3.8.1.11: "This property can be specified once in 'VEVENT', 'VTODO', or 'VJOURNAL' calendar components.") |
| `COMPLETED`, `PERCENT-COMPLETE`, `CLASS`, `CREATED`, `GEO`, `LAST-MODIFIED`, `LOCATION`, `ORGANIZER`, `RECURRENCE-ID`, `SEQUENCE` | OPTIONAL | at most once each |
| `CATEGORIES`, `ATTACH`, `COMMENT`, `CONTACT`, `RELATED-TO`, `RESOURCES`, `ATTENDEE`, `RDATE`, `EXDATE`, `REQUEST-STATUS`, `X-*`, IANA props | OPTIONAL | MAY occur more than once |

Note the enclosing object: §3.4 shows an iCalendar object is `BEGIN:VCALENDAR` … `END:VCALENDAR`, and its example carries `VERSION:2.0` and `PRODID:` at the calendar level.

---

## 2. §3.6.2 — The `DUE` / `DURATION` mutual-exclusion rule (exact quote)

From the `todoprop` ABNF comments in §3.6.2:

> ```
> ; Either 'due' or 'duration' MAY appear in
> ; a 'todoprop', but 'due' and 'duration'
> ; MUST NOT occur in the same 'todoprop'.
> ; If 'duration' appear in a 'todoprop',
> ; then 'dtstart' MUST also appear in
> ; the same 'todoprop'.
> ```

Two normative consequences: (a) never emit both `DUE` and `DURATION` in one `VTODO`; (b) `DURATION` without `DTSTART` is invalid.

---

## 3. §3.8.2.3 — Can `DTSTART` and `DUE` both be present? Value-type matching and ordering

Yes, both may be present (§3.6.2 lists `dtstart` as an optional once-only property, and `due` separately). But §3.8.2.3 ("Date-Time Due") imposes hard constraints. Verbatim from the Description:

> "This property defines the date and time before which a to-do is expected to be completed.  For cases where this property is specified in a "VTODO" calendar component that also specifies a "DTSTART" property, the value type of this property MUST be the same as the "DTSTART" property, and the value of this property MUST be later in time than the value of the "DTSTART" property.  Furthermore, this property MUST be specified as a date with local time if and only if the "DTSTART" property is also specified as a date with local time."

Normative strength: all three are **MUST** (not SHOULD).

Breaking that into three testable rules:

1. **Value type equality (MUST):** if `DTSTART` is `DATE-TIME`, `DUE` must be `DATE-TIME`; if `DTSTART;VALUE=DATE`, then `DUE;VALUE=DATE`.
2. **Strict ordering (MUST):** `DUE` > `DTSTART`. Note the wording is "**later in time than**", i.e. strictly greater — `DUE == DTSTART` is a violation, and `DUE < DTSTART` obviously is.
3. **Floating-time symmetry (MUST … if and only if):** `DUE` is a date-with-local-time (no `Z`, no `TZID`) exactly when `DTSTART` is. So a UTC (`Z`) `DUE` cannot be paired with a floating `DTSTART`, and vice versa.

Supporting ABNF (§3.8.2.3 and §3.8.2.4) shows both accept `date-time / date` with `;Value MUST match value type`:

```
 due        = "DUE" dueparam ":" dueval CRLF
 dueval     = date-time / date
 ;Value MUST match value type

 dtstart    = "DTSTART" dtstparam ":" dtstval CRLF
 dtstval    = date-time / date
 ;Value MUST match value type
```

The §3.6.2 example confirms the legal both-present shape (`DTSTART` strictly before `DUE`, both UTC `DATE-TIME`):

```
 BEGIN:VTODO
 UID:20070514T103211Z-123404@example.com
 DTSTAMP:20070514T103211Z
 DTSTART:20070514T110000Z
 DUE:20070709T130000Z
 COMPLETED:20070707T100000Z
 SUMMARY:Submit Revised Internet-Draft
 PRIORITY:1
 STATUS:NEEDS-ACTION
 END:VTODO
```

---

## 4. Is `DTEND` ever valid inside a `VTODO`? — No

Definitive, on two independent grounds:

1. **§3.6.2** — the `todoprop` production enumerates every property a `VTODO` may carry (plus `x-prop` / `iana-prop`). `dtend` does not appear anywhere in it.
2. **§3.8.2.2 ("Date-Time End"), Conformance:**
   > "This property can be specified in "VEVENT" or "VFREEBUSY" calendar components."

`VTODO` is not in that list. The to-do analogue of `DTEND` is `DUE` (§3.8.2.3), whose Conformance reads "The property can be specified once in a "VTODO" calendar component."

---

## 5. Is `DTSTART` optional in a `VTODO`? What does absence mean?

**Optional.** §3.6.2 places `dtstart` in the block commented "The following are OPTIONAL, but MUST NOT occur more than once." §3.8.2.4 Conformance:

> "This property can be specified once in the "VEVENT", "VTODO", or "VFREEBUSY" calendar components as well as in the "STANDARD" and "DAYLIGHT" sub-components.  This property is REQUIRED in all types of recurring calendar components that specify the "RRULE" property.  This property is also REQUIRED in "VEVENT" calendar components contained in iCalendar objects that don't specify the "METHOD" property."

So `DTSTART` becomes REQUIRED in a `VTODO` only when the to-do is recurring (has an `RRULE`), or (per §3.6.2) when `DURATION` is used. Neither applies to a plain Canvas-assignment to-do that carries only a `DUE`.

**Semantics when absent.** §3.6.2 Description:

> "A "VTODO" calendar component without the "DTSTART" and "DUE" (or "DURATION") properties specifies a to-do that will be associated with each successive calendar date, until it is completed."

Read precisely: it is the absence of **both** `DTSTART` and `DUE`/`DURATION` that produces the "associated with each successive calendar date" (perpetual) semantics. The RFC does **not** define a distinct special semantics for "`DUE` present, `DTSTART` absent" — that is simply an ordinary to-do anchored by its due date, exactly as in the first §3.6.2 example, which has `DUE` and no `DTSTART`:

```
 BEGIN:VTODO
 UID:20070313T123432Z-456553@example.com
 DTSTAMP:20070313T123432Z
 DUE;VALUE=DATE:20070501
 SUMMARY:Submit Quebec Income Tax Return for 2006
 CLASS:CONFIDENTIAL
 CATEGORIES:FAMILY,FINANCE
 STATUS:NEEDS-ACTION
 END:VTODO
```

Introduced in §3.6.2 as "an example of a "VTODO" calendar component that needs to be completed before May 1st, 2007.  On midnight May 1st, 2007 this to-do would be considered overdue." This is the RFC's own blessing of the `DUE`-only shape.

---

## 6. §3.3.5 — UTC "FORM #2" DATE-TIME serialization

§3.3.5 Format Definition:

```
 date-time  = date "T" time ;As specified in the DATE and TIME
                            ;value definitions
```

§3.3.5, FORM #2, verbatim:

> "FORM #2: DATE WITH UTC TIME
>
> The date with UTC time, or absolute time, is identified by a LATIN CAPITAL LETTER Z suffix character, the UTC designator, appended to the time value.  For example, the following represents January 19, 1998, at 0700 UTC:
>
> ```
>  19980119T070000Z
> ```
>
> The "TZID" property parameter MUST NOT be applied to DATE-TIME properties whose time values are specified in UTC."

The component grammars, from §3.3.4 (DATE) and §3.3.12 (TIME):

```
 date-value         = date-fullyear date-month date-mday
 date-fullyear      = 4DIGIT
 date-month         = 2DIGIT        ;01-12
 date-mday          = 2DIGIT        ;01-28, 01-29, 01-30, 01-31

 time         = time-hour time-minute time-second [time-utc]
 time-hour    = 2DIGIT        ;00-23
 time-minute  = 2DIGIT        ;00-59
 time-second  = 2DIGIT        ;00-60
 time-utc     = "Z"
```

So the exact UTC serialization is fixed-width, zero-padded, no separators: **`YYYYMMDDTHHMMSSZ`** (16 characters). Seconds are mandatory. §3.3.12: "Fractions of a second are not supported by this format." §3.3.5: "The form of date and time with UTC offset MUST NOT be used" — `19980119T230000-0800` is explicitly called out as "Invalid time format".

For a Canvas generator: convert Canvas's RFC 3339 timestamps (which carry `Z` or an offset) to UTC and emit form #2; never emit an offset, and never attach `TZID` to a `Z` value. Also §3.3.5: "No additional content value encoding (i.e., BACKSLASH character encoding, see Section 3.3.11) is defined for this value type" — date-times are never escaped.

---

## 7. §3.3.11 — TEXT escaping

ABNF, verbatim from §3.3.11:

```
 text       = *(TSAFE-CHAR / ":" / DQUOTE / ESCAPED-CHAR)
    ; Folded according to description above

 ESCAPED-CHAR = ("\\" / "\;" / "\," / "\N" / "\n")
    ; \\ encodes \, \N or \n encodes newline
    ; \; encodes ;, \, encodes ,

 TSAFE-CHAR = WSP / %x21 / %x23-2B / %x2D-39 / %x3C-5B /
              %x5D-7E / NON-US-ASCII
    ; Any character except CONTROLs not needed by the current
    ; character set, DQUOTE, ";", ":", "\", ","
```

Prose, verbatim from §3.3.11 Description:

> "An intentional formatted text line break MUST only be included in a "TEXT" property value by representing the line break with the character sequence of BACKSLASH, followed by a LATIN SMALL LETTER N or a LATIN CAPITAL LETTER N, that is "\n" or "\N".
>
> The "TEXT" property values may also contain special characters that are used to signify delimiters, such as a COMMA character for lists of values or a SEMICOLON character for structured values.  In order to support the inclusion of these special characters in "TEXT" property values, they MUST be escaped with a BACKSLASH character.  A BACKSLASH character in a "TEXT" property value MUST be escaped with another BACKSLASH character.  A COMMA character in a "TEXT" property value MUST be escaped with a BACKSLASH character.  A SEMICOLON character in a "TEXT" property value MUST be escaped with a BACKSLASH character.  However, a COLON character in a "TEXT" property value SHALL NOT be escaped with a BACKSLASH character."

**Escape table for TEXT values:**

| Input character | Emit | Normative wording |
|---|---|---|
| `\` (BACKSLASH, U+005C) | `\\` | MUST — "MUST be escaped with another BACKSLASH character" |
| `;` (SEMICOLON) | `\;` | MUST |
| `,` (COMMA) | `\,` | MUST |
| newline (CR, LF, or CRLF) | `\n` (or `\N`) | MUST — "MUST only be included … by representing the line break with … "\n" or "\N"" |
| `:` (COLON) | `:` — **do not escape** | "SHALL NOT be escaped with a BACKSLASH character" |
| `"` (DQUOTE) | `"` — **do not escape** | Not in `ESCAPED-CHAR`; explicitly permitted as a bare alternative in the `text` production |

Ordering matters in an implementation: escape backslash **first**, then `;`, `,`, and newlines — otherwise you double-escape the backslashes you just introduced.

Note the apparent tension: `TSAFE-CHAR` excludes DQUOTE, but the `text` production adds `DQUOTE` back as an explicit alternative (`text = *(TSAFE-CHAR / ":" / DQUOTE / ESCAPED-CHAR)`), so a raw `"` inside a TEXT **value** is legal. (Different rule for property **parameter** values — §3.2: "Property parameter values MUST NOT contain the DQUOTE character.")

Also note CONTROL characters are excluded from content-line values altogether (§3.1: `CONTROL = %x00-08 / %x0A-1F / %x7F ; All the controls except HTAB`). Canvas HTML descriptions should therefore be stripped/normalized: strip control chars, convert real line breaks to `\n`.

Which properties this applies to: `SUMMARY` (§3.8.1.12, Value Type: TEXT), `DESCRIPTION` (§3.8.1.5, Value Type: TEXT), `UID` (§3.8.4.7, Value Type: TEXT), `STATUS` (§3.8.1.11, Value Type: TEXT), `CATEGORIES`, `LOCATION`, `COMMENT`. It does **not** apply to `URL` (URI), `PRIORITY` (INTEGER), or `DTSTAMP`/`DTSTART`/`DUE` (DATE-TIME/DATE).

---

## 8. §3.1 — Line folding

Verbatim, §3.1 "Content Lines":

> "Lines of text SHOULD NOT be longer than 75 octets, excluding the line break.  Long content lines SHOULD be split into a multiple line representations using a line "folding" technique.  That is, a long line can be split between any two characters by inserting a CRLF immediately followed by a single linear white-space character (i.e., SPACE or HTAB).  Any sequence of CRLF followed immediately by a single linear white-space character is ignored (i.e., removed) when processing the content type."

And from the ABNF comments in §3.1:

> ```
> ; When parsing a content line, folded lines MUST first
> ; be unfolded according to the unfolding procedure
> ; described above.  When generating a content line, lines
> ; longer than 75 octets SHOULD be folded according to
> ; the folding procedure described above.
> ```

Key facts:

- **Limit: 75 octets, excluding the line break.** Octets, not characters — a non-ASCII UTF-8 character costs 2–4 octets. The strength is **SHOULD NOT** / **SHOULD**, not MUST, but real-world parsers assume it; fold.
- **Mechanism:** insert `CRLF` + exactly one SPACE or HTAB. Continuation lines are themselves subject to the 75-octet limit (the injected whitespace counts toward the continuation line's octets).
- **Unfolding is mandatory for parsers:** "folded lines MUST first be unfolded".
- The §3.1 worked example shows a continuation may begin with one space or two (the second continuation there is indented further, and the extra space becomes part of the value):

  ```
    DESCRIPTION:This is a lo
     ng description
      that exists on a long line.
  ```

**Multi-octet UTF-8 and folds** — the exact text is a Note, verbatim from §3.1:

> "   Note: It is possible for very simple implementations to generate
>    improperly folded lines in the middle of a UTF-8 multi-octet
>    sequence.  For this reason, implementations need to unfold lines
>    in such a way to properly restore the original sequence."

Be precise about what this does and does not say: it is **non-normative prose** (a Note, no MUST/SHOULD), and it is addressed to **parsers** ("implementations need to unfold lines in such a way to properly restore the original sequence") rather than stating an explicit generator prohibition. The RFC does not contain a literal "MUST NOT split a multi-octet sequence" sentence. But it labels such output "improperly folded", so the correct generator behaviour is unambiguous: **fold on UTF-8 character boundaries, never mid-sequence** — count octets, but only break where a character ends. This matters directly for Spanish-language Canvas course names and assignment titles (accented characters, `ñ`, `¿`/`¡`, em dashes, smart quotes are all multi-octet).

---

## 9. §3.1 — Line endings: CRLF

Verbatim, §3.1:

> "The iCalendar object is organized into individual lines of text, called content lines.  Content lines are delimited by a line break, which is a CRLF sequence (CR character followed by LF character)."

And §3.1 again: "the content information consists of CRLF-separated content lines."

Every production in the RFC terminates with `CRLF` — e.g. `contentline = name *(";" param ) ":" value CRLF` (§3.1), `todoc = "BEGIN" ":" "VTODO" CRLF todoprop *alarmc "END" ":" "VTODO" CRLF` (§3.6.2), `icalobject = "BEGIN" ":" "VCALENDAR" CRLF icalbody "END" ":" "VCALENDAR" CRLF` (§3.4). Bare LF is not conformant. Write `\r\n`, including after the final `END:VCALENDAR`.

Charset, §3.1.4: "The default charset for an iCalendar stream is UTF-8 as defined in [RFC3629]."

---

## 10. `SUMMARY` (§3.8.1.12), `DESCRIPTION` (§3.8.1.5), `URL` (§3.8.4.6)

### SUMMARY — §3.8.1.12

- **Value Type:  TEXT** → subject to §3.3.11 escaping.
- Conformance, verbatim: "The property can be specified in "VEVENT", "VTODO", "VJOURNAL", or "VALARM" calendar components." Cardinality in `VTODO` comes from §3.6.2: OPTIONAL, MUST NOT occur more than once.
- Description: "This property is used in the "VEVENT", "VTODO", and "VJOURNAL" calendar components to capture a short, **one-line** summary about the activity or journal entry." (emphasis added) — so a Canvas assignment title should be single-line; strip embedded newlines rather than encoding them as `\n`.
- ABNF: `summary = "SUMMARY" summparam ":" text CRLF`, with optional `ALTREP` and `LANGUAGE` parameters (`LANGUAGE` is worth setting for Spanish-language Canvas content).

### DESCRIPTION — §3.8.1.5

- **Value Type:  TEXT** → subject to §3.3.11 escaping (this is where `\n` matters most).
- Conformance, verbatim: "The property can be specified in the "VEVENT", "VTODO", "VJOURNAL", or "VALARM" calendar components.  The property can be specified **multiple times only within a "VJOURNAL"** calendar component." → in a `VTODO`, **at most once** (consistent with §3.6.2).
- Description: "This property is used in the "VEVENT" and "VTODO" to capture lengthy textual descriptions associated with the activity."
- ABNF: `description = "DESCRIPTION" descparam ":" text CRLF`, optional `ALTREP` / `LANGUAGE`.
- The §3.8.1.5 example demonstrates folding plus `\n`-encoded intentional line breaks in the same value.

### URL — §3.8.4.6

- **Value Type:  URI** — *not* TEXT. Therefore **no backslash escaping**: §3.3.13 (URI) states "No additional content value encoding (i.e., BACKSLASH character encoding, see Section 3.3.11) is defined for this value type." Emit the Canvas assignment URL literally; do not escape `,`, `;`, or `:`. (Any character needing protection should be percent-encoded per RFC 3986 instead: §3.3.13 "Property values with this value type MUST follow the generic URI syntax defined in [RFC3986].")
- Conformance, verbatim: "This property can be specified **once** in the "VEVENT", "VTODO", "VJOURNAL", or "VFREEBUSY" calendar components."
- ABNF: `url = "URL" urlparam ":" uri CRLF` — note only `other-param` parameters; no `VALUE`, no `ALTREP`.
- §3.8.4.6 Description: "This property may be used in a calendar component to convey a location where a more dynamic rendition of the calendar information associated with the calendar component can be found." That is exactly the Canvas assignment page — a correct use.
- Caveat: the colon in `https:` is fine in a **value** (§3.1 `VALUE-CHAR = WSP / %x21-7E / NON-US-ASCII`), and URIs are long, so `URL:` lines will frequently exceed 75 octets and must be folded (§3.1). Because a fold injects a leading space that unfolding removes, this is safe — but do not add your own indentation beyond the single fold whitespace, since a second space would land inside the URI. Note §3.3.13's separate rule that when a URI is a *parameter* value it "MUST be specified as a quoted-string value" — that does not apply to the `URL` property value itself.

---

## 11. §3.8.1.11 — Legal `STATUS` values for a `VTODO`

Confirmed. Verbatim ABNF from §3.8.1.11:

```
 statvalue-todo  = "NEEDS-ACTION" ;Indicates to-do needs action.
                 / "COMPLETED"    ;Indicates to-do completed.
                 / "IN-PROCESS"   ;Indicates to-do in process of.
                 / "CANCELLED"    ;Indicates to-do was cancelled.
 ;Status values for "VTODO".
```

Exactly those four, and no others (the `TENTATIVE`/`CONFIRMED` set is `statvalue-event`; `DRAFT`/`FINAL` is `statvalue-jour`). `CANCELLED` is the only value shared with `VEVENT`.

Other facts from §3.8.1.11:
- Value Type: TEXT; ABNF `status = "STATUS" statparam ":" statvalue CRLF`; only `other-param` parameters.
- Conformance: "This property can be specified once in "VEVENT", "VTODO", or "VJOURNAL" calendar components."
- Description: "In a "VTODO" calendar component, the "Organizer" can indicate that an action item needs action, is completed, is in process or being worked on, or has been cancelled."
- Example given in §3.8.1.11: `STATUS:NEEDS-ACTION`.
- Values are case-insensitive per §3.1 ("All names of properties, property parameters, enumerated property values and property parameter values are case-insensitive") — but emit them uppercase as written.

Mapping note for Canvas: a submitted/graded assignment maps to `COMPLETED` (and §3.6.2 permits the `COMPLETED` **property**, a UTC DATE-TIME per §3.8.2.1, once); an unsubmitted one to `NEEDS-ACTION`.

---

## 12. §3.8.1.9 — `PRIORITY` range and meaning

- **Value Type:  INTEGER** (§3.8.1.9). ABNF:
  ```
   priority   = "PRIORITY" prioparam ":" priovalue CRLF
   ;Default is zero (i.e., undefined).
   prioparam  = *(";" other-param)
   priovalue   = integer       ;Must be in the range [0..9]
      ; All other values are reserved for future use.
  ```
- Conformance: "This property can be specified in "VEVENT" and "VTODO" calendar components." Cardinality in a `VTODO` per §3.6.2: OPTIONAL, at most once.
- Meaning, verbatim from §3.8.1.9 Description:
  > "This priority is specified as an integer in the range 0 to 9.  A value of 0 specifies an undefined priority.  A value of 1 is the highest priority.  A value of 2 is the second highest priority.  Subsequent numbers specify a decreasing ordinal priority.  A value of 9 is the lowest priority."
- Three-level mapping, verbatim:
  > "A CUA with a three-level priority scheme of "HIGH", "MEDIUM", and "LOW" is mapped into this property such that a property value in the range of 1 to 4 specifies "HIGH" priority.  A value of 5 is the normal or "MEDIUM" priority.  A value in the range of 6 to 9 is "LOW" priority."
- Also: "Other integer values are reserved for future use." → never emit a negative value or anything > 9.
- "Within a "VTODO" calendar component, this property specified a priority for the to-do.  This property is useful in prioritizing multiple action items for a given time period." (sic — typo is in the RFC)
- §3.8.1.9 notes `PRIORITY:0` "is equivalent to not specifying the "PRIORITY" property", so prefer omitting the property over emitting `0`.

Practical mapping for a Canvas generator: use 1–4 for high-stakes/near-due or high-points assignments, 5 for normal, 6–9 for low; omit the property when you have no signal.

---

## RFC-grounded verdict on the design question

**Question:** if `unlock_at >= due_at` (start not before due), is it RFC-legal to emit `DTSTART=unlock_at` together with `DUE=due_at`?

**Answer: No. It violates a MUST.**

§3.8.2.3 (Date-Time Due):

> "For cases where this property is specified in a "VTODO" calendar component that also specifies a "DTSTART" property, the value type of this property MUST be the same as the "DTSTART" property, and the value of this property **MUST be later in time than the value of the "DTSTART" property**."

The requirement is "later in time than" — strict inequality. Therefore:

| Relation | Legal? | Why |
|---|---|---|
| `unlock_at < due_at` | ✅ legal | `DUE` is strictly later than `DTSTART` (§3.8.2.3) |
| `unlock_at == due_at` | ❌ **illegal** | `DUE` is not "later in time than" `DTSTART` — equality fails the MUST (§3.8.2.3) |
| `unlock_at > due_at` | ❌ **illegal** | `DUE` is earlier than `DTSTART` — plainly fails the MUST (§3.8.2.3) |

Canvas data makes both illegal cases reachable in practice: an assignment can be published at the moment it is due (a same-instant `unlock_at`/`due_at`), an instructor can move `due_at` earlier without touching `unlock_at`, or `unlock_at` may be inherited from a module/override that lands after the due date.

**Why omitting `DTSTART` is the RFC-safe choice:**

1. `DTSTART` is **OPTIONAL** in a `VTODO` — §3.6.2 lists `dtstart` under "The following are OPTIONAL, but MUST NOT occur more than once." Dropping it can never make the component invalid.
2. Nothing makes it required here. §3.8.2.4 makes `DTSTART` REQUIRED only "in all types of recurring calendar components that specify the "RRULE" property" (and in `VEVENT`s of METHOD-less objects); §3.6.2 additionally requires it only when `DURATION` is used. A Canvas to-do with a plain `DUE` and no `RRULE` and no `DURATION` triggers none of these.
3. The `DUE`-only shape is explicitly exemplified by the RFC. §3.6.2's first example is a `VTODO` with `UID`, `DTSTAMP`, `DUE`, `SUMMARY`, `CLASS`, `CATEGORIES`, `STATUS` and **no `DTSTART`** — described as "a "VTODO" calendar component that needs to be completed before May 1st, 2007."
4. Emitting the pair anyway is not a soft violation: it breaks a **MUST** in §3.8.2.3, i.e. the produced `.ics` is non-conformant, and strict consumers may reject the component or the whole calendar.

**Recommended generator rule:**

```
emit DUE       when due_at is present
emit DTSTART   only when unlock_at is present
                AND due_at is present
                AND unlock_at <  due_at          (strict, per §3.8.2.3)
                AND both are rendered with the same value type
                    (both DATE-TIME, or both VALUE=DATE)   (§3.8.2.3)
                AND both are the same flavour of time
                    (both UTC "Z", or both floating)       (§3.8.2.3)
otherwise omit DTSTART entirely                            (§3.6.2: OPTIONAL)
```

Emitting everything as UTC form #2 `DATE-TIME` (`YYYYMMDDTHHMMSSZ`, §3.3.5 FORM #2) automatically satisfies both the value-type-equality and the local-time-iff clauses, leaving strict ordering as the only runtime check. If `due_at` itself is absent, omit `DUE` too and accept the §3.6.2 semantics ("associated with each successive calendar date, until it is completed") or skip the assignment.

---

## Sources

Single primary source, consulted in full:

- **RFC 5545** — B. Desruisseaux, Ed., *"Internet Calendaring and Scheduling Core Object Specification (iCalendar)"*, RFC 5545, Standards Track, September 2009. Obsoletes RFC 2445.
  - HTML: https://www.rfc-editor.org/rfc/rfc5545.html
  - Plain text (the copy quoted throughout): https://www.rfc-editor.org/rfc/rfc5545.txt
  - DOI: 10.17487/RFC5545

Sections cited in this document:

| Section | Title | Used for |
|---|---|---|
| 3.1 | Content Lines | CRLF, 75-octet limit, folding, UTF-8 fold note, `VALUE-CHAR`/`CONTROL`, case sensitivity |
| 3.1.4 | Character Set | UTF-8 default |
| 3.2 | Property Parameters | DQUOTE rule for parameter values |
| 3.3.4 | Date | `DATE` grammar |
| 3.3.5 | Date-Time | UTC FORM #2 serialization, no-offset rule, `TZID` prohibition |
| 3.3.11 | Text | TEXT escaping ABNF and prose |
| 3.3.12 | Time | `TIME` grammar, `Z` designator, no fractional seconds |
| 3.3.13 | URI | URI value type, no backslash encoding |
| 3.4 | iCalendar Object | `VCALENDAR` wrapper, `VERSION`/`PRODID` |
| 3.6.2 | To-Do Component | `todoprop` ABNF, DUE/DURATION exclusion, no-DTSTART semantics, examples |
| 3.8.1.5 | Description | TEXT, once in VTODO |
| 3.8.1.9 | Priority | INTEGER `[0..9]`, meanings |
| 3.8.1.11 | Status | `statvalue-todo` enumeration |
| 3.8.1.12 | Summary | TEXT, one-line |
| 3.8.2.1 | Date-Time Completed | `COMPLETED` property (referenced) |
| 3.8.2.2 | Date-Time End | `DTEND` conformance excludes VTODO |
| 3.8.2.3 | Date-Time Due | value-type match + strictly-later MUST |
| 3.8.2.4 | Date-Time Start | DTSTART conformance / when REQUIRED |
| 3.8.4.6 | Uniform Resource Locator | URL value type URI, once |
| 3.8.4.7 | Unique Identifier | UID required, TEXT |
| 3.8.7.2 | Date-Time Stamp | DTSTAMP required, UTC |

Note on scope: RFC 5545 is the sole normative source used. Later extensions (e.g. RFC 7986 additional properties, RFC 6638 scheduling) were not consulted and are not relied upon by any claim above.
