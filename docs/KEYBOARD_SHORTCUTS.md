# Keyboard shortcuts

Hylki can be driven from the keyboard without holding a modifier, in the style
of Gmail and Geary. The scheme is **off by default** (a stray keystroke
shouldn't archive mail), so switch it on first in **Settings → Message List →
Single-key shortcuts**.

Press **Ctrl+?** (or F1, or *Main Menu → Keyboard Shortcuts*) at any time for
this list in the app; the same key closes it again.

| Move around | | Act on a message | |
| --- | --- | --- | --- |
| <kbd>j</kbd> <kbd>↓</kbd> | Next message | <kbd>r</kbd> | Reply |
| <kbd>k</kbd> <kbd>↑</kbd> | Previous message | <kbd>R</kbd> | Reply to all |
| <kbd>l</kbd> <kbd>→</kbd> | Open the selected message | <kbd>f</kbd> | Forward |
| <kbd>h</kbd> <kbd>←</kbd> <kbd>u</kbd> | Back to the message list | <kbd>a</kbd> | Archive |
| <kbd>w</kbd> | Next message in the conversation | <kbd>d</kbd> | Delete |
| <kbd>b</kbd> | Previous message in the conversation | <kbd>!</kbd> | Mark as spam |
| <kbd>/</kbd> | Search | <kbd>s</kbd> | Star or unstar |
| <kbd>c</kbd> | Compose | <kbd>m</kbd> | Mark read or unread |
| <kbd>?</kbd> | This list | <kbd>x</kbd> | Select the row (for a bulk action) |
| | | <kbd>1</kbd> … <kbd>9</kbd> | Add or remove a tag (the first nine, in Settings order) |
| | | <kbd>0</kbd> | Remove every tag |

<kbd>Esc</kbd> backs out of a reply, forward or compose and returns you to the
message list. It works whether or not single-key shortcuts are enabled, as
does everything in the menus, and in a search field it still clears the
search.

<kbd>Ctrl+Enter</kbd> sends the message you are writing, from the body or
any of its address rows. It does what the Send button does, so a message with
no recipient is not sent, and one scheduled for later is queued.

<kbd>Ctrl+Shift+F</kbd> turns [Focus Mode](FEATURES.md#the-app) on and off,
with or without single-key shortcuts.

<kbd>Ctrl++</kbd> and <kbd>Ctrl+-</kbd> zoom the message in the reading pane
in and out, with or without Reader View; <kbd>Ctrl+0</kbd> puts it back to
the default. Only the message scales, not the header or the toolbar; a chip
beside the Reader View switch shows the percentage while it is away from
the default, and a click on it goes back there.
The zoom stays as set from message to message until Hylki is next started,
which begins at the default from **Settings → Reading → Default zoom**.

Keys never fire while you are typing: whatever has focus gets first refusal, so
"archive" typed into the search box searches for it.
