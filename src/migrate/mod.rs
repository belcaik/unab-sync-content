//! Offline migration from layout v1 to layout v2.
//!
//! [`rewrite`] copies a generated markdown note aside and then refreshes its
//! YAML frontmatter and Canvas file links. Detection, planning and the rename
//! executor are separate steps; they are not in this module yet.

pub mod rewrite;
