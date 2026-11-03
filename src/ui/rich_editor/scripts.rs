//! The scripts every editor page carries in its `<head>`. The JavaScript
//! lives in the `.js` files beside this one; each is wrapped in its
//! `<script>` element here.

/// Cuts the body's text history into steps a person would recognise.
///
/// WebKit folds a whole run of typing into one undo step — and folds the
/// deletes that follow into that same step, so Backspace was nothing Ctrl+Z
/// could take back on its own, and a paragraph written over several minutes
/// came back all at once. Its history can be stepped but not shaped from
/// outside, and there is no API for "end the current step"; the one thing
/// that closes an open typing command is the selection changing. Re-setting
/// the selection to exactly where it already is does that and nothing else:
/// the caret does not move and no content is touched. (`Unselect` closes the
/// run too, but drops the caret and with it the next keystroke; blur/focus
/// works and is heavier than it needs to be.)
///
/// The run is closed when the kind of edit changes — typing to deleting or
/// back — and after a pause long enough that what follows is a new thought.
/// An IME composition is left alone: its own events flip between inserting
/// and deleting while a character is being built, and a break mid-word would
/// cut a single character into pieces.
///
/// Serves the source-mode document too, where the editable is a `<textarea>`
/// and the same close is `setSelectionRange` to the selection it already has.
pub(super) const HISTORY_SCRIPT: &str =
    concat!("<script>", include_str!("history.js"), "</script>");

/// The editor's side of its emoji chooser (#348). `__hylkiEmojiMark` keeps
/// the caret, for the emoji to go to once the chooser has had the focus,
/// and says where it is (`x,y,w,h` in CSS pixels) for the chooser to point
/// at. `__hylkiEmojiPut` types the emoji there. A `<textarea>` (source
/// mode) keeps its own caret, and the chooser points at the field.
pub(super) const EMOJI_SCRIPT: &str = concat!("<script>", include_str!("emoji.js"), "</script>");

/// How the body follows the direction of each paragraph and line (#366,
/// #524): `dir="auto"` on its blocks, and a line of a plain `<div>` or `<p>`
/// kept as a block of its own while the message is written, joined back to
/// a `<br>` line in what the document hands back. Its helpers reach
/// [`PASTE_SCRIPT`] through `__hylkiLines`, so it goes in first.
pub(super) const LINES_SCRIPT: &str = concat!("<script>", include_str!("lines.js"), "</script>");

/// The paste choke point: every paste — shortcut, context menu, editing
/// command — raises a DOM `paste` event here, where the clipboard's flavours
/// can be inspected. `__hylkiPasteRich` is the standing mode (the user's
/// preference, stamped at document build); `__hylkiPasteOnce` overrides it for
/// exactly one paste (Ctrl+V and the context-menu items set it just before
/// running the Paste command, so a live preference change is honored).
///
/// Plain mode inserts the clipboard's text flavour with URLs linkified; rich
/// mode lets WebKit's native paste keep the formatting, linkifying only
/// pastes that carry no HTML at all. Lives in `<head>` (not the editable
/// body) so it never becomes part of the message.
pub(super) const PASTE_SCRIPT: &str = concat!("<script>", include_str!("paste.js"), "</script>");

/// The quoted original of a reply or forward, folded behind a button while
/// `__hylkiFoldQuote` is set (#549). After [`PASTE_SCRIPT`], whose plain
/// view flattens the body first.
pub(super) const FOLD_SCRIPT: &str = concat!("<script>", include_str!("fold.js"), "</script>");

/// Grammar and style marks (#452), drawn over the text the user wrote once
/// the host switches them on. See [`super::grammar`].
pub(super) const GRAMMAR_SCRIPT: &str =
    concat!("<script>", include_str!("grammar.js"), "</script>");
