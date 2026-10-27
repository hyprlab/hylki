//! The composer in its host: inline or in a window, its size, its fields shown
//! or folded, and leaving it.

use super::messages::ComposeInput;
use super::messages::ComposeOutput;
use super::suggest::field_row;
use super::types::ComposePrefill;
use super::types::Field;
use super::Compose;
use super::ComposeWidgets;
use crate::i18n::i18n;
use crate::ui::context_menu::{show_context_menu, MenuEntry};
use adw::prelude::*;
use relm4::prelude::*;

/// The narrowest the compose pane goes (its `adw::BreakpointBin` floor).
pub(super) const COMPOSE_MIN_WIDTH: i32 = 360;

/// Fallback fold threshold when the toolbar could not be measured at init.
const COMPOSE_ACTIONS_BREAKPOINT: f64 = 620.0;

/// An OpenPGP switch's tooltip, header button and menu row alike: what it
/// does, then whether it is on, which the icon alone says only to someone
/// who knows what its color means.
pub(super) fn pgp_tooltip(action: &str, on: bool) -> String {
    format!("{action}\n{}", if on { i18n("On") } else { i18n("Off") })
}

/// The width a header bar's rows need side by side: the natural widths of
/// its centre box's children (start row, title, end row) summed — what the
/// bar itself reports doubles the wider side to keep the title centred.
/// `None` when the bar's insides are not the expected shape.
fn header_rows_width(header: &gtk::Widget) -> Option<i32> {
    fn find_center_box(w: &gtk::Widget, depth: u32) -> Option<gtk::Widget> {
        if w.type_().name() == "GtkCenterBox" {
            return Some(w.clone());
        }
        if depth == 0 {
            return None;
        }
        let mut c = w.first_child();
        while let Some(child) = c {
            if let Some(found) = find_center_box(&child, depth - 1) {
                return Some(found);
            }
            c = child.next_sibling();
        }
        None
    }
    let cb = find_center_box(header, 6)?;
    let mut total = 0;
    let mut c = cb.first_child();
    while let Some(w) = c {
        total += w.measure(gtk::Orientation::Horizontal, -1).1;
        c = w.next_sibling();
    }
    Some(total)
}

/// Size the pane for its host. Both hosts impose a definite height now — the
/// reader-covering overlay inline (it fills the whole pane), the window itself
/// popped out — so the editor always expands to fill whatever it is given.
/// Inline, the compose header also stands in for the reader's (which it
/// covers, or tops), so it takes over the GNOME window decorations — unless
/// `decorations` is off: a split reply beneath the reader leaves the reader's
/// own header in place, and a second set of controls mid-pane would be wrong.
/// In a window of its own the header is the window's, close button and all.
pub(super) fn size_for_host(
    root: &adw::ToolbarView,
    header: &adw::HeaderBar,
    editor_holder: &gtk::Box,
    windowed: bool,
    decorations: bool,
) {
    root.set_vexpand(true);
    editor_holder.set_vexpand(true);
    editor_holder.set_height_request(-1);
    header.set_show_end_title_buttons(windowed || decorations);
}

/// Set the inline/window toggle button's icon + tooltip for the current host.
pub(super) fn set_toggle_icon(btn: &gtk::Button, windowed: bool) {
    if windowed {
        btn.set_icon_name("view-restore-symbolic");
        btn.set_tooltip_text(Some(i18n("Collapse into reader").as_str()));
    } else {
        btn.set_icon_name("view-fullscreen-symbolic");
        btn.set_tooltip_text(Some(i18n("Open in window").as_str()));
    }
}

/// Save the edited message, discard it, or go back to it. Escape answers
/// Keep Editing, so a second press never discards what the first one asked
/// about (#290).
#[allow(deprecated)] // adw::MessageDialog: AlertDialog would change how it looks
fn confirm_discard_dialog(
    parent: Option<&gtk::Window>,
    template: bool,
    sender: relm4::Sender<ComposeInput>,
) {
    let (heading, body, save) = if template {
        (
            i18n("Save the template?"),
            i18n("Its changes have not been saved. Save them to the template, or discard them."),
            i18n("Save Template"),
        )
    } else {
        (
            i18n("Save the message?"),
            i18n("It has not been sent. Save it to Drafts to finish later, or discard it."),
            i18n("Save Draft"),
        )
    };
    let dialog = adw::MessageDialog::new(parent, Some(heading.as_str()), Some(body.as_str()));
    dialog.add_response("keep", &i18n("Keep Editing"));
    dialog.add_response("discard", &i18n("Discard"));
    dialog.add_response("save", &save);
    dialog.set_response_appearance("discard", adw::ResponseAppearance::Destructive);
    dialog.set_response_appearance("save", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("save"));
    dialog.set_close_response("keep");
    dialog.connect_response(None, move |_, resp| {
        let _ = sender.send(match resp {
            "save" => ComposeInput::SaveDraft,
            "discard" => ComposeInput::Discard,
            _ => ComposeInput::KeepEditing,
        });
    });
    dialog.present();
}

impl Compose {
    /// Where the cursor starts: in To while the message is addressed to
    /// nobody, in Subject once it is addressed but untitled, and in the body
    /// when both are filled in, which is every reply. A new message used to
    /// open in the body like a reply, where other mail clients start from
    /// the recipients (#266). A compact reply with its field rows folded
    /// away always starts in the body.
    pub(super) fn focus_initial(&self, widgets: &ComposeWidgets) {
        let fields = widgets.fields_list.is_visible();
        if fields && widgets.to_row.text().trim().is_empty() {
            widgets.to_row.grab_focus();
        } else if fields && widgets.subject_row.text().trim().is_empty() {
            widgets.subject_row.grab_focus();
        } else {
            self.editor.grab_focus();
        }
    }

    /// Handles [`ComposeInput::Cancel`].
    pub(super) fn on_cancel(&mut self, sender: &ComponentSender<Self>) {
        if self.asking_discard {
            return;
        }
        if self.fields_dirty {
            sender.input_sender().emit(ComposeInput::ConfirmDiscard);
        } else {
            let s = sender.clone();
            self.editor.is_dirty(move |body_dirty| {
                s.input_sender().emit(if body_dirty {
                    ComposeInput::ConfirmDiscard
                } else {
                    ComposeInput::Discard
                });
            });
        }
    }

    /// Handles [`ComposeInput::ConfirmDiscard`].
    pub(super) fn on_confirm_discard(
        &mut self,
        sender: &ComponentSender<Self>,
        root: &adw::BreakpointBin,
    ) {
        if self.asking_discard {
            return;
        }
        self.asking_discard = true;
        let parent = root.root().and_downcast::<gtk::Window>();
        confirm_discard_dialog(
            parent.as_ref(),
            self.template,
            sender.input_sender().clone(),
        );
    }

    /// Handles [`ComposeInput::Discard`].
    pub(super) fn on_discard(&mut self, sender: &ComponentSender<Self>) {
        // A message nobody saved leaves nothing behind, though the
        // composer saved it on its own meanwhile (#340).
        match self.draft_origin.clone().filter(|_| self.autosave_made) {
            Some(origin) => {
                let _ = sender.output(ComposeOutput::DiscardAutosave {
                    id: self.compose_id,
                    origin,
                });
            }
            None => {
                let _ = sender.output(ComposeOutput::Close(self.compose_id));
            }
        }
    }

    /// Handles [`ComposeInput::FocusField`].
    pub(super) fn on_focus_field(&mut self, field: Field, widgets: &ComposeWidgets) {
        if !widgets.fields_list.is_visible() {
            // The chevron's toggle sends ShowFields, which records it.
            widgets.fields_btn.set_active(true);
            widgets.fields_list.set_visible(true);
        }
        let row = field_row(widgets, field);
        row.set_visible(true);
        row.grab_focus();
    }

    /// Handles [`ComposeInput::SetNarrow`].
    pub(super) fn on_set_narrow(
        &mut self,
        narrow: bool,
        widgets: &ComposeWidgets,
        root: &adw::BreakpointBin,
    ) {
        self.narrow = narrow;
        self.dress_format_buttons();
        // Folded, the header is as narrow as it gets: the pane goes
        // no narrower, so a small window stops shrinking rather than
        // cutting Send off at its edge. Unfolded, the floor drops
        // back, or the pane could never get narrow enough to fold.
        let (root, header) = (root.clone(), widgets.header.clone());
        gtk::glib::idle_add_local_once(move || {
            let floor = if narrow {
                header
                    .measure(gtk::Orientation::Horizontal, -1)
                    .0
                    .max(COMPOSE_MIN_WIDTH)
            } else {
                COMPOSE_MIN_WIDTH
            };
            root.set_size_request(floor, 200);
        });
    }

    /// Handles [`ComposeInput::OverflowMenu`].
    pub(super) fn on_overflow_menu(
        &mut self,
        widgets: &ComposeWidgets,
        sender: &ComponentSender<Self>,
    ) {
        let entry = |label: String, icon: &str, msg: fn() -> ComposeInput| {
            let s = sender.clone();
            MenuEntry::new(&label, move || s.input_sender().emit(msg()))
                .icon(format!("{icon}-symbolic"))
        };
        // Save Draft, Save as Template and Delete Draft are not here:
        // the Save Draft button and its arrow stay in the header at
        // any width.
        let mut attach = vec![entry(i18n("Attach files"), "mail-attachment", || {
            ComposeInput::AttachFiles
        })];
        if !self.cloud_accounts.is_empty() {
            attach.push(
                entry(
                    i18n("Upload to cloud storage and share a link"),
                    "cloud",
                    || ComposeInput::CloudAttach,
                )
                .enabled(self.cloud_busy == 0),
            );
        }
        attach.push(entry(
            i18n("Open Contacts"),
            "x-office-address-book",
            || ComposeInput::OpenContacts,
        ));
        // The OpenPGP toggles go through their buttons so the
        // toggled handlers keep the model and the buttons agreeing.
        let mut pgp = Vec::new();
        if crate::mail::pgp::available() {
            let sign = widgets.sign_btn.clone();
            pgp.push(
                MenuEntry::new(i18n("Sign with your OpenPGP key"), move || {
                    sign.set_active(!sign.is_active())
                })
                .icon("security-high-symbolic")
                .checked(self.sign)
                .tooltip(pgp_tooltip(&i18n("Sign with your OpenPGP key"), self.sign)),
            );
            let encrypt = widgets.encrypt_btn.clone();
            pgp.push(
                MenuEntry::new(
                    i18n("Encrypt with OpenPGP to every recipient's key"),
                    move || encrypt.set_active(!encrypt.is_active()),
                )
                .icon("channel-secure-symbolic")
                .checked(self.encrypt)
                .tooltip(pgp_tooltip(
                    &i18n("Encrypt with OpenPGP to every recipient's key"),
                    self.encrypt,
                )),
            );
        }
        // The format chooser, the preview toggle and Insert Signature
        // are not here: they sit at the end of the formatting row,
        // which a narrow pane never folds away.
        let mut host = Vec::new();
        if self.can_toggle {
            host.push(if self.windowed {
                entry(i18n("Collapse into reader"), "view-restore", || {
                    ComposeInput::ToggleWindowed
                })
            } else {
                entry(i18n("Open in window"), "view-fullscreen", || {
                    ComposeInput::ToggleWindowed
                })
            });
        }
        let btn = &widgets.overflow_btn;
        show_context_menu(
            btn,
            (btn.width() / 2) as f64,
            btn.height() as f64,
            vec![attach, pgp, host],
        );
    }

    /// Handles [`ComposeInput::DeleteDraft`].
    pub(super) fn on_delete_draft(&mut self, sender: &ComponentSender<Self>) {
        if let Some(origin) = self.draft_origin.clone() {
            let _ = sender.output(ComposeOutput::DeleteDraft {
                id: self.compose_id,
                origin,
            });
        }
    }

    /// Handles [`ComposeInput::SetDecorations`].
    pub(super) fn on_set_decorations(&mut self, on: bool, widgets: &ComposeWidgets) {
        self.decorations = on;
        size_for_host(
            &widgets.toolbar_root,
            &widgets.header,
            &widgets.editor_holder,
            self.windowed,
            on,
        );
    }

    /// Handles [`ComposeInput::SetWindowed`].
    pub(super) fn on_set_windowed(&mut self, windowed: bool, widgets: &ComposeWidgets) {
        self.windowed = windowed;
        set_toggle_icon(&widgets.toggle_btn, windowed);
        size_for_host(
            &widgets.toolbar_root,
            &widgets.header,
            &widgets.editor_holder,
            windowed,
            self.decorations,
        );
        // A compact reply grows its field rows back in a window (and
        // sheds them again if it returns inline).
        widgets
            .fields_list
            .set_visible(!(self.compact && !windowed) || self.fields_shown);
    }

    pub(super) fn show_initial_rows(&self, widgets: &ComposeWidgets, prefill: &ComposePrefill) {
        // Per-row visibility (#25): To always; Cc/Bcc only when prefilled (a
        // reply-all carries Cc). The Subject is always shown — replies and
        // forwards arrive with it prefilled, but it stays the user's to see
        // and change (2026-08-31).
        let cc_shown = !prefill.cc.trim().is_empty();
        let bcc_shown = !prefill.bcc.trim().is_empty();
        widgets.cc_row.set_visible(cc_shown);
        widgets.bcc_row.set_visible(bcc_shown);
        // Reply-To (#58) is rare enough to always start hidden behind "More".
        widgets.reply_to_row.set_visible(false);
        widgets.subject_row.set_visible(true);
        // Compact split reply: only the editor shows; the full field rows
        // return when the composer pops out to a window. Never for a pane
        // that arrives unaddressed — a forward — which needs its To row
        // (#139).
        widgets
            .fields_list
            .set_visible(!self.compact || self.fields_shown);
        widgets.fields_btn.set_active(self.fields_shown);
        {
            let more = gtk::Button::with_label(&i18n("More"));
            more.add_css_class("flat");
            more.set_valign(gtk::Align::Center);
            more.set_tooltip_text(Some(i18n("Show Cc, Bcc and Reply-To").as_str()));
            let cc = widgets.cc_row.clone();
            let bcc = widgets.bcc_row.clone();
            let reply_to = widgets.reply_to_row.clone();
            let from = widgets.from_row.clone();
            let custom = widgets.custom_from_row.clone();
            let btn = more.clone();
            more.connect_clicked(move |_| {
                cc.set_visible(true);
                bcc.set_visible(true);
                reply_to.set_visible(true);
                // With one address the From row is hidden; More brings it,
                // with its pencil for typing another (#347).
                if !custom.is_visible() {
                    from.set_visible(true);
                }
                btn.set_visible(false);
            });
            widgets.to_row.add_suffix(&more);
        }
    }
}

pub(super) fn fold_on_first_map(
    root: &adw::BreakpointBin,
    widgets: &ComposeWidgets,
    sender: &ComponentSender<Compose>,
) {
    // Fold the toolbar once the pane is narrower than the full row (as
    // the reader's header does). Measured on the first map — before
    // anything is hidden, and once the header has a window, since the
    // window controls it carries inline only exist (and measure) with
    // one. A later host move changes that by a few dozen pixels; the
    // first host's threshold is kept, which only ever folds early.
    let s = sender.clone();
    let header: gtk::Widget = widgets.header.clone().upcast();
    let measured = std::rc::Rc::new(std::cell::Cell::new(false));
    root.connect_map(move |root| {
        if measured.replace(true) {
            return;
        }
        // Not the bar's own natural width: a header bar keeps its
        // title centred, so it asks for twice its wider side, and
        // this bar's end row is much wider than its start — every
        // button added there counted double and the fold came far
        // too soon. The centre box's rows, summed, are what has to
        // fit.
        let full = header.measure(gtk::Orientation::Horizontal, -1).1;
        let rows = header_rows_width(&header);
        tracing::debug!("compose fold: header natural {full}, rows {rows:?}");
        let need = rows.unwrap_or(full);
        let threshold = if need <= 0 {
            COMPOSE_ACTIONS_BREAKPOINT
        } else {
            need as f64 + 24.0
        };
        let bp = adw::Breakpoint::new(adw::BreakpointCondition::new_length(
            adw::BreakpointConditionLengthType::MaxWidth,
            threshold,
            adw::LengthUnit::Px,
        ));
        let s1 = s.clone();
        bp.connect_apply(move |_| s1.input_sender().emit(ComposeInput::SetNarrow(true)));
        let s2 = s.clone();
        bp.connect_unapply(move |_| s2.input_sender().emit(ComposeInput::SetNarrow(false)));
        root.add_breakpoint(bp);
    });
}
