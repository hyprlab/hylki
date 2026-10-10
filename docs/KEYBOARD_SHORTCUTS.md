# Keyboard shortcuts

Hylki can be driven from the keyboard without holding a modifier, in the style
of Gmail and Geary. The scheme is **off by default** (a stray keystroke
shouldn't archive mail), so switch it on first in **Settings → System →
Single-key shortcuts**.

Press **Ctrl+?** (or F1, or *Main Menu → Keyboard Shortcuts*) at any time for
this list in the app; the same key closes it again.

| Move around | | Act on a message | |
| --- | --- | --- | --- |
| <kbd>j</kbd> <kbd>↓</kbd> | Next message | <kbd>r</kbd> | Reply |
| <kbd>k</kbd> <kbd>↑</kbd> | Previous message | <kbd>R</kbd> | Reply to all |
| <kbd>l</kbd> <kbd>→</kbd> | Open the selected message | <kbd>f</kbd> | Forward |
| <kbd>h</kbd> <kbd>←</kbd> <kbd>u</kbd> | Back to the message list | <kbd>a</kbd> | Archive |
| <kbd>w</kbd> | Next message in the conversation | <kbd>d</kbd> <kbd>Delete</kbd> | Delete |
| <kbd>b</kbd> | Previous message in the conversation | <kbd>!</kbd> | Mark as spam |
| <kbd>/</kbd> | Search | <kbd>s</kbd> | Star or unstar |
| <kbd>c</kbd> | Compose | <kbd>m</kbd> | Mark read or unread |
| <kbd>?</kbd> | This list | <kbd>x</kbd> | Select the row (for a bulk action) |
| | | <kbd>1</kbd> … <kbd>9</kbd> | Add or remove a tag (the first nine, in Settings order) |
| | | <kbd>0</kbd> | Remove every tag |

With several messages selected, in the list or as cards of a conversation,
archive, delete, spam, star, read and tag keys act on all of them. Star, read
and a tag key set the state on every message unless all of them already have
it, in which case they clear it from all.

<kbd>Esc</kbd> backs out of a reply, forward or compose and returns you to the
message list. Once something has been written, it asks first whether to save
the message to Drafts, discard it or keep editing, as the Cancel button and
the composer window's close button do; <kbd>Esc</kbd> again keeps editing.
It works whether or not single-key shortcuts are enabled, as does everything
in the menus, and in a search field it still clears the search.

<kbd>Ctrl+N</kbd> starts a new message, <kbd>Ctrl+Shift+N</kbd> starts one
from a [template](DOCUMENTATION.md#templates), <kbd>Ctrl+R</kbd> replies,
<kbd>Ctrl+Shift+R</kbd> replies to all and <kbd>Ctrl+U</kbd> shows the
message's source, with or without single-key shortcuts. While you are
writing, the composer keeps them: there <kbd>Ctrl+U</kbd> underlines.

<kbd>Ctrl+Enter</kbd> sends the message you are writing, from the body or
any of its address rows. It does what the Send button does, so a message with
no recipient is not sent, and one scheduled for later is queued.

In the body, <kbd>Return</kbd> starts a new line and <kbd>Shift+Return</kbd>
a new paragraph; **Settings → Composing → Return starts a new paragraph**
swaps them. See [Return and Shift+Return](DOCUMENTATION.md#return-and-shiftreturn).

<kbd>Ctrl+.</kbd> or <kbd>Ctrl+;</kbd> opens the emoji chooser at the cursor,
in the body of a message or in any text field that takes emoji; the emoji you
pick goes where the cursor was.

<kbd>Ctrl+Shift+F</kbd> turns [Focus Mode](FEATURES.md#the-app) on and off,
with or without single-key shortcuts.

These work with or without single-key shortcuts too:

| Key | What it does |
| --- | --- |
| <kbd>Delete</kbd> <kbd>Backspace</kbd> | Delete the selected messages, with the list focused |
| <kbd>Ctrl+F</kbd> | Search the message list |
| <kbd>Ctrl+P</kbd> | Print the message you are reading |
| <kbd>Ctrl+Shift+P</kbd> | Preview it as a PDF first |
| <kbd>Ctrl+Z</kbd> | Undo the last action (a move, a delete, a star) |
| <kbd>Ctrl+Shift+Z</kbd> <kbd>Ctrl+Y</kbd> | Redo it |
| <kbd>Ctrl+Shift+S</kbd> | Reveal the status bar (also: long-press Refresh) |
| <kbd>Ctrl+Shift+A</kbd> | Show or hide the accounts in the sidebar |
| <kbd>Ctrl+Shift+C</kbd> | Console mode, when it is enabled in Settings |
| <kbd>Ctrl+W</kbd> | Close the window; mail keeps syncing in the background |
| <kbd>Ctrl+Q</kbd> | Quit Hylki |

In the composer, <kbd>Ctrl+Z</kbd> and <kbd>Ctrl+Shift+Z</kbd> undo and redo
your typing instead, and <kbd>Ctrl+B</kbd>, <kbd>Ctrl+I</kbd> and
<kbd>Ctrl+U</kbd> set bold, italic and underline.

In the attachment drawer, <kbd>Space</kbd> or <kbd>Enter</kbd> previews the
highlighted attachment. In the attachments gallery's preview, <kbd>←</kbd> and
<kbd>→</kbd> step to the previous and next one, and <kbd>Esc</kbd> closes it.

In the Contacts view, <kbd>Delete</kbd> asks to delete the contact shown,
while the contact list has the focus.

<kbd>Ctrl+F</kbd> in the Settings window opens its search, and closes it
again. <kbd>Esc</kbd> closes it too, wherever the focus is in the window,
including after you have picked a result.

<kbd>Ctrl++</kbd> and <kbd>Ctrl+-</kbd> zoom the message in the reading pane
in and out, with or without Reader View; <kbd>Ctrl+0</kbd> puts it back to
the default. Only the message scales, not the header or the toolbar; a chip
beside the Reader View switch shows the percentage while it is away from
the default, and a click on it goes back there.
The zoom stays as set from message to message until Hylki is next started,
which begins at the default from **Settings → Reading → Default zoom**.

Keys never fire while you are typing: whatever has focus gets first refusal, so
"archive" typed into the search box searches for it.
