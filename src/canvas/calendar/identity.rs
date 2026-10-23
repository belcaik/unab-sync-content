//! The calendar projection's stable identity: on-disk directory names,
//! component UIDs, filenames, and the `state.json` key namespaces (spec D4,
//! D5). Everything here derives from the Canvas assignment id, never from any
//! date.

/// The on-disk name of the directory holding deadline `VTODO` files for a
/// course. A sibling directory holds the availability-window `VEVENT`s
/// ([`WINDOWS_DIR`]); a third semantics (recurring classes, spec D4) has room
/// to land later without moving anything under either.
pub(super) const DEADLINES_DIR: &str = "deadlines";

/// The on-disk name of the directory holding availability-window `VEVENT`
/// files for a course (spec D4, ticket 08). Sibling of [`DEADLINES_DIR`], not
/// nested under it — the point is that a client can subscribe to one and not
/// the other.
pub(super) const WINDOWS_DIR: &str = "windows";

/// Derive the calendar UID for an assignment's deadline `VTODO`.
///
/// Derived only from the Canvas assignment id, so it survives title and due
/// date changes — the same id also names the on-disk file (via
/// [`deadline_filename`]), which is why a moved deadline rewrites the same
/// path instead of orphaning the old one (see the spike findings in
/// `docs/specs/calendar-sync-flow.md`).
///
/// The `todo-` segment is a semantic discriminator, not decoration: UIDs are
/// the server-side identity of a published CalDAV object and must be
/// globally unique across the account. Ticket 08 adds a `VEVENT` for the
/// same assignment id under `u_crawler-window-{id}@…` — without the
/// discriminator the two would collide, which is exactly the VTODO/VEVENT
/// confusion the spec cites in Radicale issue #101. Changing this scheme
/// after the first `caldir push` would orphan already-published objects, so
/// it is fixed now rather than in ticket 08.
pub(super) fn deadline_uid(assignment_id: u64) -> String {
    format!("u_crawler-todo-{assignment_id}@u-crawler.local")
}

/// Derive the on-disk filename for an assignment's deadline `VTODO`. Shares
/// its source (the assignment id) with [`deadline_uid`], never the due date.
pub(super) fn deadline_filename(assignment_id: u64) -> String {
    component_filename(assignment_id)
}

/// Derive the calendar UID for an assignment's availability-window `VEVENT`
/// (spec D4, ticket 08). The `window-` discriminator is what keeps this
/// distinct from [`deadline_uid`]'s `todo-` for the same assignment id — see
/// that function's doc comment for why the distinction is load-bearing
/// (Radicale issue #101, cited in the spec). Fixed by ticket 04's plan; do not
/// change it.
pub(super) fn window_uid(assignment_id: u64) -> String {
    format!("u_crawler-window-{assignment_id}@u-crawler.local")
}

/// The one filename implementation behind [`deadline_filename`] and
/// [`window_filename`]: both components name their file from the assignment
/// id alone, and the two live in different directories, so the shared name
/// can never collide on disk.
pub(super) fn component_filename(assignment_id: u64) -> String {
    format!("assignment-{assignment_id}.ics")
}

/// Derive the on-disk filename for an assignment's availability-window
/// `VEVENT`. Lives under [`WINDOWS_DIR`], a different directory from
/// [`deadline_filename`]'s [`DEADLINES_DIR`], so the two never collide on
/// disk even though both derive their name the same way from the assignment
/// id.
pub(super) fn window_filename(assignment_id: u64) -> String {
    component_filename(assignment_id)
}

/// Prefix of the `state.json` namespace for a deadline `VTODO`'s projected
/// content (spec D5): `calendar:{assignment_id}`. Distinct from the content
/// flow's own `assignment:{id}` key (`canvas/sync.rs`) — same assignment,
/// different projection, so a separate key stops the two flows from
/// clobbering each other's bookkeeping. Exposed as a constant (not just
/// baked into [`calendar_state_key`]) because the reconciliation pass in
/// [`plan`](fn@super::plan::plan) (ticket 10) needs to recognize this namespace when scanning
/// `prev`'s keys, not just build one.
pub(super) const CALENDAR_KEY_PREFIX: &str = "calendar:";

/// Prefix of the `state.json` namespace for an availability-window
/// `VEVENT`'s projected content (spec D5, ticket 08):
/// `calendar-window:{assignment_id}`. Distinct from
/// [`CALENDAR_KEY_PREFIX`] — the VTODO and VEVENT for the same assignment
/// are two different projections with two different hashes, and sharing a
/// namespace would make writing (or deleting) one component affect the
/// other regardless of its own state.
pub(super) const WINDOW_KEY_PREFIX: &str = "calendar-window:";

/// The `state.json` key for a deadline `VTODO`'s projected content.
pub(super) fn calendar_state_key(assignment_id: u64) -> String {
    format!("{CALENDAR_KEY_PREFIX}{assignment_id}")
}

/// The `state.json` key for an availability-window `VEVENT`'s projected
/// content.
pub(super) fn window_state_key(assignment_id: u64) -> String {
    format!("{WINDOW_KEY_PREFIX}{assignment_id}")
}
