//! The availability-window `VEVENT` renderer (spec D3, ticket 08). Frozen:
//! this component emits exactly the bytes it always has.

use chrono::{DateTime, Utc};

use super::ics::{dtstamp_for, escape_text, ics_datetime};
use crate::canvas::Assignment;

/// Render a single assignment's availability window as a complete `.ics`
/// file: one `VCALENDAR` wrapping one `VEVENT` spanning `unlock` to `due`
/// (spec D3, ticket 08). No `PRIORITY` or `STATUS` — those are `VTODO`
/// concepts (submission/completed status is ticket 09's scope, not this
/// one's).
pub(super) fn render_vevent(
    uid: &str,
    assignment: &Assignment,
    unlock: DateTime<Utc>,
    due: DateTime<Utc>,
) -> String {
    let dtstamp = dtstamp_for(assignment, due);
    let mut lines = vec![
        "BEGIN:VCALENDAR".to_string(),
        "VERSION:2.0".to_string(),
        "PRODID:-//u_crawler//calendar-sync//EN".to_string(),
        "BEGIN:VEVENT".to_string(),
        format!("UID:{uid}"),
        format!("DTSTAMP:{}", ics_datetime(dtstamp)),
        format!("DTSTART:{}", ics_datetime(unlock)),
        format!("DTEND:{}", ics_datetime(due)),
        format!(
            "SUMMARY:{}",
            escape_text(assignment.name.as_deref().unwrap_or(""))
        ),
    ];
    if let Some(url) = &assignment.html_url {
        lines.push(format!("URL:{}", escape_text(url)));
    }
    lines.push("END:VEVENT".to_string());
    lines.push("END:VCALENDAR".to_string());
    lines.join("\r\n") + "\r\n"
}
