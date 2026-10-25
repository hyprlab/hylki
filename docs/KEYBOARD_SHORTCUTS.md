# Keyboard shortcuts

Hylki can be driven from the keyboard without holding a modifier, in the style
of Gmail and Geary. The scheme is on for a new install, and **Settings →
System → Single-key shortcuts** switches it off. An install that had it off
keeps it off until it is switched on there.

Press **Ctrl+?** (or F1, or *Main Menu → Keyboard Shortcuts*) at any time for
this list in the app; the same key closes it again.

The reader toolbar's ⋯ menu and a message's right-click menus, in the list
and in the reader, show the key beside each entry that has one: the single
key while single-key shortcuts are on (Gmail's with Gmail keys), otherwise
the Ctrl key where there is one. With single keys on, each of the first nine
tags in a Tags menu shows its number.

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
| | | <kbd>D</kbd> | Delete permanently, without going to Trash |
| | | <kbd>z</kbd> | Snooze |

<kbd>w</kbd> and <kbd>b</kbd> step through the cards of the conversation you
are reading, opening a folded one; outside a conversation they move along the
list. The keys keep working after a click in the message, apart from the ones
that scroll it (the arrows, Page Up and Down, Space).

## Gmail keys

**Settings → System → Gmail keys** swaps in Gmail's single keys. It needs
**Single-key shortcuts** on as well, and stays greyed out until they are. The
list moves under a cursor without opening anything until you ask, and a key
followed by another goes somewhere.

| Move around | | Act on a message | |
| --- | --- | --- | --- |
| <kbd>j</kbd> <kbd>k</kbd> | Move down or up the list without opening | <kbd>r</kbd> | Reply |
| <kbd>o</kbd> <kbd>Enter</kbd> | Open the conversation the cursor is on | <kbd>a</kbd> | Reply to all |
| <kbd>u</kbd> <kbd>Esc</kbd> | Close the conversation, back to the list | <kbd>f</kbd> | Forward |
| <kbd>n</kbd> <kbd>p</kbd> | Next or previous message in the conversation | <kbd>e</kbd> | Archive the conversation |
| <kbd>;</kbd> <kbd>:</kbd> | Expand or collapse every message of the conversation | <kbd>]</kbd> <kbd>}</kbd> | Archive the conversation and open the older one |
| <kbd>g</kbd> <kbd>i</kbd> | Inbox | <kbd>[</kbd> <kbd>{</kbd> | Archive the conversation and open the newer one |
| <kbd>g</kbd> <kbd>s</kbd> | Starred | <kbd>#</kbd> | Delete |
| <kbd>g</kbd> <kbd>t</kbd> | Sent | <kbd>!</kbd> | Mark as spam |
| <kbd>g</kbd> <kbd>d</kbd> | Drafts | <kbd>s</kbd> | Star or unstar |
| <kbd>g</kbd> <kbd>a</kbd> | Archive | <kbd>Shift+I</kbd> | Mark the conversation as read |
| <kbd>g</kbd> <kbd>c</kbd> | Contacts | <kbd>Shift+U</kbd> | Mark the conversation as unread and close it |
| <kbd>/</kbd> | Search | <kbd>_</kbd> | Mark as unread from this message on and close it |
| <kbd>c</kbd> | Compose | <kbd>x</kbd> | Select the row the cursor is on without opening it |
| <kbd>d</kbd> | Compose in a window | <kbd>*</kbd> <kbd>a</kbd> / <kbd>*</kbd> <kbd>n</kbd> | Select every row, or none |
| <kbd>Shift+N</kbd> | Check for new mail | <kbd>*</kbd> <kbd>r</kbd> / <kbd>*</kbd> <kbd>u</kbd> | Select the read or the unread conversations |
| <kbd>?</kbd> | This list | <kbd>*</kbd> <kbd>s</kbd> / <kbd>*</kbd> <kbd>t</kbd> | Select the starred or the unstarred conversations |
| | | <kbd>v</kbd> | Move to a folder |
| | | <kbd>b</kbd> | Snooze |
| | | <kbd>l</kbd> | Tags |
| | | <kbd>z</kbd> | Undo |
| | | <kbd>Shift+R</kbd> <kbd>Shift+A</kbd> <kbd>Shift+F</kbd> | Reply, reply to all or forward in a window |
| | | <kbd>1</kbd> … <kbd>9</kbd>, <kbd>0</kbd> | Tags by number, as above |

<kbd>j</kbd> and <kbd>k</kbd> step from conversation to conversation, over
the replies of one opened out in the list, and the row they reach has an
accent outline. <kbd>o</kbd> or <kbd>Enter</kbd> opens it and takes the
keyboard into the reader, as Gmail's conversation view does: there
<kbd>j</kbd> and <kbd>k</kbd> open the next or previous conversation, and
<kbd>u</kbd> or <kbd>Esc</kbd> close it and go back to the list, the cursor
on its row. <kbd>n</kbd> and <kbd>p</kbd> start from the first message that
is unread or open, and go on from the one they reached.

<kbd>e</kbd>, <kbd>Shift+I</kbd> and <kbd>Shift+U</kbd> act on the whole
conversation that is open, even with one of its messages stepped to with
<kbd>n</kbd> or <kbd>p</kbd>. Archiving moves the messages the list shows for
it and leaves your replies in Sent; marking unread leaves Sent and Drafts
alone. <kbd>_</kbd> marks the message you stepped to unread, and the later
ones of the conversation; with none stepped to, the newest. Both close the
conversation afterwards, as <kbd>u</kbd> does.

On a Gmail account every folder but the Inbox, Spam and Trash is a label, and
<kbd>e</kbd> there takes the conversation out of the Inbox and leaves it
under the label, as Gmail's Archive does. The toolbar's Archive does the
same. <kbd>]</kbd> and <kbd>[</kbd> archive the same way, then open the
conversation below or above in the list.

<kbd>Shift+R</kbd>, <kbd>Shift+A</kbd>, <kbd>Shift+F</kbd> and <kbd>d</kbd>
open the composer in a window of its own even when **Settings → Composing**
puts replies or new messages in the reading pane.

<kbd>x</kbd> selects the row the cursor is on without opening it, and closes
the conversation that was open. While rows are selected this way, the
reading pane counts them and the bar above the list acts on them, as do the
action keys; <kbd>x</kbd> again on a row takes it out. <kbd>o</kbd>,
<kbd>Enter</kbd> or a click opens a row in the reading pane and ends the
selection; with several rows selected, <kbd>o</kbd> and <kbd>Enter</kbd> do
nothing. Deleting the selected rows leaves the cursor on the conversation
above them, or the one below, without opening it. An action key pressed with
none selected says that a selection is needed.

<kbd>Enter</kbd> on a message that is already open opens it in a window of
its own. A <kbd>g</kbd> or <kbd>*</kbd> waits a second and a half for the key
that follows.

A message window takes the same keys for its own conversation:
<kbd>n</kbd>, <kbd>p</kbd>, <kbd>;</kbd> and <kbd>:</kbd> move through it,
and the action keys act on it as they do on an open conversation in the main
window. <kbd>u</kbd> and <kbd>Esc</kbd> close the window, as archiving or
marking it unread does. The list's keys, <kbd>g</kbd> and <kbd>*</kbd> do
nothing there.

With several messages selected, in the list or as cards of a conversation,
archive, delete, spam, star, read and tag keys act on all of them. A
conversation's row stands for every message of it the folder holds, so
marking several conversations read marks all of their messages. Star, read
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

<kbd>Ctrl+Shift+A</kbd> attaches files, and <kbd>Ctrl+Shift+T</kbd>,
<kbd>Ctrl+Shift+C</kbd> and <kbd>Ctrl+Shift+B</kbd> put the cursor in To, Cc
and Bcc, showing the row if it is hidden, as in Thunderbird. Outside the
composer, <kbd>Ctrl+Shift+A</kbd> and <kbd>Ctrl+Shift+C</kbd> keep the
meanings below.

In To, Cc and Bcc, a space after an address, <kbd>Return</kbd> or a comma
makes it a pill, and <kbd>Backspace</kbd> on an empty field turns the last
pill back into text. <kbd>Left</kbd> at the start of the field selects the
last pill, and <kbd>Ctrl+A</kbd> on an empty field selects all of them. On a
selected pill, <kbd>Left</kbd> and <kbd>Right</kbd> select the next one
(<kbd>Shift</kbd> extends the selection), <kbd>Delete</kbd> removes the
selection, <kbd>Return</kbd> edits the pill, and <kbd>Menu</kbd> or
<kbd>Shift+F10</kbd> opens the menu, which moves the selection to another
field. See [Addresses as pills](DOCUMENTATION.md#addresses-as-pills).

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
| <kbd>Ctrl+K</kbd> | Command palette: type to find an action, a folder, a view, a tag or a smart folder, and Enter runs it; start with `>` for actions only |
| <kbd>Ctrl+Shift+K</kbd> | Command palette with `>` already typed: actions only |
| <kbd>Delete</kbd> | Delete the selected messages, or the message you are reading, in the main window or its own |
| <kbd>Backspace</kbd> | Delete the selected messages, with the list focused |
| <kbd>Shift+Delete</kbd> | Delete the selected messages permanently, without going to Trash, after asking |
| <kbd>Ctrl+F</kbd> | Search the message list, or find in the message when it has the focus |
| <kbd>Ctrl+P</kbd> | Print the message you are reading |
| <kbd>Ctrl+Shift+P</kbd> | Preview it as a PDF first |
| <kbd>Ctrl+Z</kbd> | Undo the last action (a move, a delete, a star) |
| <kbd>Ctrl+Shift+Z</kbd> <kbd>Ctrl+Y</kbd> | Redo it |
| <kbd>Ctrl+Shift+S</kbd> | Reveal the status bar (also: long-press Refresh) |
| <kbd>Ctrl+Shift+A</kbd> | Show or hide the accounts in the sidebar |
| <kbd>Ctrl+Shift+C</kbd> | Console mode, when it is enabled in Settings |
| <kbd>F11</kbd> | Fullscreen on or off |
| <kbd>Ctrl+,</kbd> | Open Settings |
| <kbd>Ctrl+W</kbd> | Close the window; mail keeps syncing in the background. In Settings, a message window or a composer window, closes that window |
| <kbd>Ctrl+Q</kbd> | Quit Hylki |

In the composer, <kbd>Ctrl+Z</kbd> and <kbd>Ctrl+Shift+Z</kbd> undo and redo
your typing instead, and <kbd>Ctrl+B</kbd>, <kbd>Ctrl+I</kbd> and
<kbd>Ctrl+U</kbd> set bold, italic and underline.

In the attachment drawer, <kbd>Space</kbd> or <kbd>Enter</kbd> previews the
highlighted attachment. In the attachments gallery's preview, <kbd>←</kbd> and
<kbd>→</kbd> step to the previous and next one, and <kbd>Esc</kbd> closes it.

In the Contacts view, <kbd>Ctrl+F</kbd> and <kbd>/</kbd> open its search
and <kbd>Esc</kbd> closes it, as in the message list. <kbd>Ctrl</kbd> or
<kbd>Shift</kbd> and a click select several contacts, and <kbd>Ctrl+A</kbd>
all of them. <kbd>Delete</kbd> asks to delete the contact shown, or those
selected, while the contact list has the focus. Contacts dragged onto an
address book in the sidebar are copied there, or moved with <kbd>Ctrl</kbd>
held as they are dropped. In a group's form, <kbd>↓</kbd> and
<kbd>↑</kbd> move through the people matching what is typed in the field
that adds them, <kbd>Enter</kbd> adds the one highlighted, and <kbd>Esc</kbd>
puts the matches away.

<kbd>Ctrl+F</kbd> in the Settings window opens its search, and closes it
again. <kbd>Esc</kbd> closes it too, wherever the focus is in the window,
including after you have picked a result. Closing the window ends the
search.

<kbd>Ctrl++</kbd> and <kbd>Ctrl+-</kbd> zoom the message in the reading pane
in and out, with or without Reader View, and so does <kbd>Ctrl</kbd> with the
scroll wheel over the message; <kbd>Ctrl+0</kbd> puts it back to
the default. Only the message scales, not the header or the toolbar; a chip
beside the Reader View switch shows the percentage while it is away from
the default, and a click on it goes back there.
The zoom stays as set from message to message until Hylki is next started,
which begins at the default from **Settings → Reading → Default zoom**.

The same keys, and <kbd>Ctrl</kbd> with the scroll wheel over the body, zoom
the composer's text while it is written in Markdown, HTML source or plain
text; <kbd>Ctrl+0</kbd> puts it back to 100%. Only the screen changes, not
the message that is sent, and the next composer opens at the same size until
Hylki is next started. Rich text does not zoom, because there the size on
screen is the size that is sent; the keys go on to the message instead.

Keys never fire while you are typing: whatever has focus gets first refusal, so
"archive" typed into the search box searches for it.
