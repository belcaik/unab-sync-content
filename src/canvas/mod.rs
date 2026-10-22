//! Everything that speaks Canvas: the REST client and its wire types, plus
//! the per-feature flows built on them (module sync, announcements, calendar
//! projection, recordings discovery), the shared file downloader, HTML link
//! extraction, and the course-directory naming helper.

/// Course announcements: fetched, rendered to markdown, and indexed.
pub mod announcements;
/// The calendar-sync flow: a pure deadline planner plus its I/O executor.
pub mod calendar;
/// The Canvas LMS REST client.
pub mod client;
/// Derivation of a course's on-disk directory from its Canvas record.
pub mod course;
/// The single downloader for Canvas-hosted files.
pub mod download;
/// Extraction of links and media references from Canvas HTML.
pub mod links;
/// The Canvas LMS REST wire types.
pub mod models;
/// Discovery of Zoom links referenced across a course.
pub mod recordings;
/// Aggregation behind the `status` command: the download root as data.
pub mod status;
/// The main sync: courses, modules, pages, assignments and their files.
pub mod sync;

pub use client::CanvasClient;
pub use models::*;
