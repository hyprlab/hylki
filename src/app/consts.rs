//! Sizes and budgets the window and its caches are built with.

/// The narrowest the reader pane may be squeezed. The header's right-hand
/// actions collapse into the overflow menu below READER_ACTIONS_BREAKPOINT
/// (the left group — Reply … Delete — stays). What binds is the composer
/// written in the reader's place: its folded header (Cancel, Save Draft, ⋯,
/// Send, close) needs this much in English. Longer labels raise the pane's
/// floor while a composer is open (cover_slot::mount). Kept modest on
/// purpose: the window's total minimum width must stay under half of a
/// 1920px screen, or GNOME refuses to tile the window to the left/right
/// screen edge (it only offers the top-edge maximize).
pub(super) const READER_MIN_WIDTH: i32 = 441;

/// Byte budgets for the in-RAM body and attachment caches (issue #106). Bodies
/// average well under 1 MB even with inline images, so 64 MiB holds hundreds of
/// recently seen messages; attachments run larger, and 128 MiB still keeps a
/// handful of big ones for instant re-preview. Everything evicted re-reads from
/// the SQLite cache.
pub(super) const BODY_CACHE_BUDGET: usize = 64 << 20;

pub(super) const ATTACHMENT_CACHE_BUDGET: usize = 128 << 20;
