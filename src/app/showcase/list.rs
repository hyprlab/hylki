//! Showcase hooks for the list, the reader and the sidebar: opening rows,
//! menus and palettes, swipes, the gallery, the rail and the peek panel.

use adw::prelude::*;
use relm4::prelude::*;

use crate::app::model::AppModel;
use crate::app::msg::AppMsg;
use crate::app::AppModelWidgets;
use crate::models::FolderKind;
use crate::ui::attachments_gallery::GalleryInput;
use crate::ui::message_list::MessageListInput;
use crate::ui::message_view::MessageViewInput;
use crate::ui::showcase::showcase_capture;
use crate::ui::sidebar::SidebarInput;

impl AppModel {
    /// HYLKI_SHOWCASE_HEIGHTS=1 logs the allocated height of the
    /// sidebar's section headings and their rows just before the
    /// capture, for checking pill geometry without a pointer.
    pub(super) fn stage_heights(&self, delay: u32) {
        if std::env::var("HYLKI_SHOWCASE_HEIGHTS").is_ok() {
            let sidebar = self.sidebar.widget().clone().upcast::<gtk::Widget>();
            gtk::glib::timeout_add_seconds_local_once(delay.saturating_sub(1).max(1), move || {
                fn walk(w: &gtk::Widget, root: &gtk::Widget) {
                    let classes = w.css_classes();
                    let heading = classes.iter().any(|c| c == "unified-folders-toggle");
                    let child_row = w.is::<gtk::ListBoxRow>()
                        && w.parent().is_some_and(|p| {
                            p.css_classes().iter().any(|c| c == "section-child-list")
                        });
                    if heading || child_row {
                        let y = w.compute_bounds(root).map(|b| b.y() as i32).unwrap_or(-1);
                        tracing::info!(
                            "showcase heights: {} y={y} h={} margins={}/{} classes={:?}",
                            if heading { "heading" } else { "row" },
                            w.height(),
                            w.margin_top(),
                            w.margin_bottom(),
                            classes,
                        );
                    }
                    let mut c = w.first_child();
                    while let Some(ch) = c {
                        walk(&ch, root);
                        c = ch.next_sibling();
                    }
                }
                walk(&sidebar, &sidebar);
            });
        }
    }

    /// Moves the list's selection onto its first row at 3 s, which opens
    /// the demo conversation.
    pub(super) fn stage_select_first_row(&self) {
        let list = self.message_list.sender().clone();
        gtk::glib::timeout_add_seconds_local_once(3, move || {
            let _ = list.send(MessageListInput::MoveSelection(1));
        });
    }

    /// HYLKI_SHOWCASE_PALETTE=N opens row N's actions palette
    /// (so a capture can verify the floating palette's look).
    pub(super) fn stage_palette(&self, root: &adw::ApplicationWindow) {
        if let Some(Ok(idx)) = std::env::var("HYLKI_SHOWCASE_PALETTE")
            .ok()
            .map(|v| v.parse())
        {
            let list = self.message_list.sender().clone();
            let burst = std::env::var("HYLKI_SHOWCASE_BURST").ok();
            let win = root.clone();
            {
                // An occluded window's frame clock is suspended
                // and animations cannot progress — raise it well
                // ahead so the slide actually runs for the burst.
                let win = win.clone();
                gtk::glib::timeout_add_seconds_local_once(5, move || {
                    win.present();
                });
            }
            gtk::glib::timeout_add_seconds_local_once(7, move || {
                let _ = list.send(MessageListInput::DebugOpenPalette(idx));
                // TEMP: frame burst right after the open, to see
                // the slide animation (or its absence) in stills.
                if let Some(base) = burst.clone() {
                    for (i, ms) in [40u64, 90, 140, 240].iter().enumerate() {
                        let win = win.clone();
                        let path = format!("{base}.{i}.png");
                        gtk::glib::timeout_add_local_once(
                            std::time::Duration::from_millis(*ms),
                            move || {
                                showcase_capture(win.upcast_ref::<gtk::Widget>(), &path);
                            },
                        );
                    }
                }
            });
        }
    }

    /// Highlights the conversation's mid-thread card (account 1, message
    /// 2) at 6 s.
    pub(super) fn stage_open_card(&self) {
        let view = self.message_view.sender().clone();
        gtk::glib::timeout_add_seconds_local_once(6, move || {
            let _ = view.send(crate::ui::message_view::MessageViewInput::CardClicked {
                account_id: 1,
                id: 2,
                mode: crate::ui::message_view::SelectMode::Plain,
            });
        });
    }

    /// HYLKI_SHOWCASE_SWIPE=N[:left|right] swipes row N fully and
    /// releases it at 7s, with HYLKI_SHOWCASE_BURST=<path> writing
    /// stills through the commit exit — the only way to see the
    /// gesture's animation here (no input injection).
    pub(super) fn stage_swipe(&self, root: &adw::ApplicationWindow) {
        if let Ok(spec) = std::env::var("HYLKI_SHOWCASE_SWIPE") {
            let (idx, side) = spec.split_once(':').unwrap_or((spec.as_str(), "left"));
            let index: usize = idx.trim().parse().unwrap_or(0);
            let left = side != "right";
            let list = self.message_list.sender().clone();
            let burst = std::env::var("HYLKI_SHOWCASE_BURST").ok();
            let win = root.clone();
            {
                // An occluded window's frame clock is suspended, so
                // raise it well before the animation runs.
                let win = win.clone();
                gtk::glib::timeout_add_seconds_local_once(5, move || win.present());
            }
            gtk::glib::timeout_add_seconds_local_once(7, move || {
                let _ = list.send(MessageListInput::DebugSwipe { index, left });
                if let Some(base) = burst.clone() {
                    for (i, ms) in [40u64, 120, 200, 300, 500].iter().enumerate() {
                        let win = win.clone();
                        let path = format!("{base}.{i}.png");
                        gtk::glib::timeout_add_local_once(
                            std::time::Duration::from_millis(*ms),
                            move || {
                                showcase_capture(win.upcast_ref::<gtk::Widget>(), &path);
                            },
                        );
                    }
                }
            });
        }
    }

    /// HYLKI_SHOWCASE_ROW=N selects the list's row N at 4s (with
    /// HYLKI_SHOWCASE_STAGE=0), for capturing or probing one message.
    pub(super) fn stage_row(&self) {
        if let Some(Ok(n)) = std::env::var("HYLKI_SHOWCASE_ROW")
            .ok()
            .map(|v| v.parse::<u32>())
        {
            let list = self.message_list.sender().clone();
            gtk::glib::timeout_add_seconds_local_once(4, move || {
                for _ in 0..=n {
                    let _ = list.send(MessageListInput::MoveSelection(1));
                }
            });
        }
    }

    /// HYLKI_SHOWCASE_UNSUB=ask|go presses the open card's
    /// Unsubscribe button at 7s (after HYLKI_SHOWCASE_ROW opened
    /// it): `ask` stops at the confirmation, `go` skips it.
    pub(super) fn stage_unsubscribe(&self) {
        if std::env::var("HYLKI_SHOWCASE_UNSUB").is_ok() {
            let view = self.message_view.sender().clone();
            gtk::glib::timeout_add_seconds_local_once(7, move || {
                let _ = view.send(MessageViewInput::Unsubscribe {
                    account_id: 1,
                    id: 10,
                });
            });
        }
    }

    /// HYLKI_SHOWCASE_INVITE=accept|maybe|decline|calendar presses
    /// that button on the open card's invitation banner at 7s
    /// (after HYLKI_SHOWCASE_ROW=8 opened the meeting request).
    pub(super) fn stage_invite(&self) {
        if let Ok(which) = std::env::var("HYLKI_SHOWCASE_INVITE") {
            use crate::models::Rsvp;
            use crate::ui::message_view::InviteAction;
            let action = match which.as_str() {
                "maybe" => InviteAction::Answer(Rsvp::Tentative),
                "decline" => InviteAction::Answer(Rsvp::Declined),
                "calendar" => InviteAction::AddToCalendar,
                _ => InviteAction::Answer(Rsvp::Accepted),
            };
            let view = self.message_view.sender().clone();
            gtk::glib::timeout_add_seconds_local_once(7, move || {
                let _ = view.send(MessageViewInput::InviteAction {
                    account_id: 1,
                    id: 13,
                    action,
                });
            });
        }
    }

    /// HYLKI_SHOWCASE_UNIFIED=sent|starred|drafts|archive|junk|trash|snoozed|smart opens that unified
    /// row at 3s, All Inboxes at 6s and the row again at 9s, so the
    /// timing logs show a cold and a warm open. starred@N picks the
    /// Nth account's row under Starred instead.
    /// HYLKI_SHOWCASE_UNIFIED_AT=<s> starts that at s seconds rather
    /// than 3, once a real profile's accounts have loaded.
    pub(super) fn stage_unified(&self) {
        if let Ok(which) = std::env::var("HYLKI_SHOWCASE_UNIFIED") {
            let at: u32 = std::env::var("HYLKI_SHOWCASE_UNIFIED_AT")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(3);
            let sub = which
                .strip_prefix("starred@")
                .and_then(|n| n.parse::<i32>().ok());
            let pick = move || match which.as_str() {
                _ if let Some(index) = sub => SidebarInput::KindSubRowSelected {
                    kind: FolderKind::Starred,
                    index,
                },
                "starred" => SidebarInput::UnifiedKindRowSelected(FolderKind::Starred),
                "drafts" => SidebarInput::UnifiedKindRowSelected(FolderKind::Drafts),
                "archive" => SidebarInput::UnifiedKindRowSelected(FolderKind::Archive),
                "junk" => SidebarInput::UnifiedKindRowSelected(FolderKind::Junk),
                "trash" => SidebarInput::UnifiedKindRowSelected(FolderKind::Trash),
                "snoozed" => SidebarInput::UnifiedKindRowSelected(FolderKind::Snoozed),
                "filtered" => SidebarInput::UnifiedFilteredSelected,
                "favorites" => SidebarInput::UnifiedFavoritesSelected,
                "tags" => SidebarInput::UnifiedTagsSelected,
                "smart" => SidebarInput::SmartRowSelected(0),
                _ => SidebarInput::UnifiedKindRowSelected(FolderKind::Sent),
            };
            let sb = self.sidebar.sender().clone();
            let p = pick.clone();
            gtk::glib::timeout_add_seconds_local_once(at, move || {
                let _ = sb.send(p());
            });
            let sb = self.sidebar.sender().clone();
            gtk::glib::timeout_add_seconds_local_once(at + 3, move || {
                let _ = sb.send(SidebarInput::SelectUnifiedRow);
            });
            let sb = self.sidebar.sender().clone();
            gtk::glib::timeout_add_seconds_local_once(at + 6, move || {
                let _ = sb.send(pick());
            });
        }
    }

    /// HYLKI_SHOWCASE_ROW_MENU=1 opens the first row's context
    /// menu at 5s (pair with HYLKI_SHOWCASE_MENU to capture it).
    pub(super) fn stage_row_menu(&self) {
        if std::env::var("HYLKI_SHOWCASE_ROW_MENU").is_ok() {
            let ml = self.message_list.sender().clone();
            gtk::glib::timeout_add_seconds_local_once(5, move || {
                let _ = ml.send(MessageListInput::DebugRowMenu);
            });
        }
    }
}

/// HYLKI_SHOWCASE_APPLY_FILTERS=inboxes|<account>:<folder>
/// starts a manual filter run at 5s (#198), the way Apply Now
/// and the folder menu's Apply Filters do, and the run's
/// report lands in the status bar a beat later.
pub(super) fn stage_apply_filters(sender: &ComponentSender<AppModel>) {
    if let Ok(spec) = std::env::var("HYLKI_SHOWCASE_APPLY_FILTERS") {
        let s = sender.clone();
        gtk::glib::timeout_add_seconds_local_once(5, move || {
            let targets = spec
                .split_once(':')
                .and_then(|(a, f)| Some(vec![(a.parse().ok()?, f.parse().ok()?)]))
                .unwrap_or_default();
            s.input(AppMsg::ApplyFilters(targets));
        });
    }
}

/// HYLKI_SHOWCASE_TOOLBAR_GAP=<zone>:<index> opens a drop gap
/// in the Settings toolbar editor at 6s (pair with
/// HYLKI_SHOWCASE_SETTINGS=appearance), as a hovering drag
/// would.
pub(super) fn stage_toolbar_gap(sender: &ComponentSender<AppModel>) {
    if let Ok(spec) = std::env::var("HYLKI_SHOWCASE_TOOLBAR_GAP") {
        if let Some((z, i)) = spec.split_once(':') {
            if let (Ok(zone), Ok(index)) = (z.parse::<usize>(), i.parse::<usize>()) {
                let s = sender.input_sender().clone();
                gtk::glib::timeout_add_seconds_local_once(6, move || {
                    let _ = s.send(AppMsg::ShowcaseToolbarGap { zone, index });
                });
            }
        }
    }
}

/// HYLKI_SHOWCASE_TOOLBAR_MENU=1 opens the header's right-click
/// menu (Customize Toolbar…) at 5s, as a click on its empty
/// middle would.
pub(super) fn stage_toolbar_menu(widgets: &AppModelWidgets, sender: &ComponentSender<AppModel>) {
    if std::env::var("HYLKI_SHOWCASE_TOOLBAR_MENU").is_ok() {
        let s = sender.input_sender().clone();
        let header: gtk::Widget = widgets.reader_header.clone().upcast();
        gtk::glib::timeout_add_seconds_local_once(5, move || {
            let _ = s.send(AppMsg::ReaderToolbarMenu {
                x: header.width() as f64 / 2.0,
                y: header.height() as f64 / 2.0,
            });
        });
    }
}

/// HYLKI_SHOWCASE_READER_MENU=1 opens the reader header's ⋯
/// overflow menu at 5s (pair with HYLKI_SHOWCASE_MENU to
/// capture it; the window must be narrow enough to collapse).
pub(super) fn stage_reader_menu(sender: &ComponentSender<AppModel>) {
    if std::env::var("HYLKI_SHOWCASE_READER_MENU").is_ok() {
        let s = sender.input_sender().clone();
        gtk::glib::timeout_add_seconds_local_once(5, move || {
            let _ = s.send(AppMsg::ReaderOverflowMenu);
        });
    }
}

/// HYLKI_SHOWCASE_PRINT=1 opens the print preview of the open
/// message at 6s (#359); HYLKI_SHOWCASE_TOP captures it.
pub(super) fn stage_print(sender: &ComponentSender<AppModel>) {
    if std::env::var("HYLKI_SHOWCASE_PRINT").is_ok() {
        let s = sender.input_sender().clone();
        gtk::glib::timeout_add_seconds_local_once(6, move || {
            let _ = s.send(AppMsg::PrintPreview);
        });
    }
}

/// HYLKI_SHOWCASE_POPOUT=1 opens the open message in a window
/// of its own at 6s; HYLKI_SHOWCASE_TOP captures it.
pub(super) fn stage_popout(sender: &ComponentSender<AppModel>) {
    if std::env::var("HYLKI_SHOWCASE_POPOUT").is_ok() {
        let s = sender.input_sender().clone();
        gtk::glib::timeout_add_seconds_local_once(6, move || {
            let _ = s.send(AppMsg::ShowcasePopout);
        });
    }
}

/// HYLKI_SHOWCASE_EDIT_AS_NEW=1 copies the open message into
/// the composer at 6s (#232), as its menu entry does.
pub(super) fn stage_edit_as_new(sender: &ComponentSender<AppModel>) {
    if std::env::var("HYLKI_SHOWCASE_EDIT_AS_NEW").is_ok() {
        let s = sender.input_sender().clone();
        gtk::glib::timeout_add_seconds_local_once(6, move || {
            let _ = s.send(AppMsg::EditAsNewCurrent);
        });
    }
}

/// HYLKI_SHOWCASE_MOVE=1 opens the Move To… picker at 5s
/// (it captures itself a second later).
pub(super) fn stage_move(sender: &ComponentSender<AppModel>) {
    if std::env::var("HYLKI_SHOWCASE_MOVE").is_ok() {
        let s = sender.input_sender().clone();
        gtk::glib::timeout_add_seconds_local_once(5, move || {
            let _ = s.send(AppMsg::MoveToMenu);
        });
    }
}

impl AppModel {
    /// HYLKI_SHOWCASE_GALLERY=1 opens the attachments gallery at
    /// 3s; add HYLKI_SHOWCASE_GALLERY_FOLDERS=1 to drop its folder
    /// popover open a second later, or HYLKI_SHOWCASE_GALLERY_ACCOUNT=<row>
    /// to pick that row of its account filter (0 = all accounts).
    pub(super) fn stage_gallery(&self, sender: &ComponentSender<AppModel>) {
        if std::env::var("HYLKI_SHOWCASE_GALLERY").is_ok() {
            let s = sender.input_sender().clone();
            let g = self.gallery.sender().clone();
            gtk::glib::timeout_add_seconds_local_once(3, move || {
                let _ = s.send(AppMsg::ShowAttachments);
                let row: Option<u32> = std::env::var("HYLKI_SHOWCASE_GALLERY_ACCOUNT")
                    .ok()
                    .and_then(|v| v.parse().ok());
                let folders = std::env::var("HYLKI_SHOWCASE_GALLERY_FOLDERS").is_ok();
                // HYLKI_SHOWCASE_GALLERY_SEARCH=<text> types into the
                // search box and reports whether it keeps the focus.
                let search = std::env::var("HYLKI_SHOWCASE_GALLERY_SEARCH").ok();
                // HYLKI_SHOWCASE_GALLERY_MORE=<n> pages down n times,
                // as scrolling to the end would.
                let more: u32 = std::env::var("HYLKI_SHOWCASE_GALLERY_MORE")
                    .ok()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0);
                gtk::glib::timeout_add_seconds_local_once(1, move || {
                    if let Some(row) = row {
                        let _ = g.send(GalleryInput::SetAccountFilter(row));
                    }
                    for i in 0..more {
                        let g = g.clone();
                        gtk::glib::timeout_add_seconds_local_once(1 + i, move || {
                            let _ = g.send(GalleryInput::LoadMore);
                        });
                    }
                    if let Some(text) = search {
                        let _ = g.send(GalleryInput::ShowcaseSearch(text));
                    }
                    if folders {
                        let _ = g.send(GalleryInput::ShowcaseFolders);
                    }
                });
            });
        }
    }

    /// HYLKI_SHOWCASE_RAIL=1 collapses the sidebar to the rail
    /// at 3s (the user's own toggle, fold-ups and all).
    pub(super) fn stage_rail(&self) {
        if std::env::var("HYLKI_SHOWCASE_RAIL").is_ok() {
            let sb = self.sidebar.sender().clone();
            gtk::glib::timeout_add_seconds_local_once(3, move || {
                let _ = sb.send(SidebarInput::ToggleCollapsed);
            });
        }
    }
}

/// HYLKI_SHOWCASE_PEEK=<seconds>[,<seconds>…] works the header's
/// sidebar toggle at each of those moments (1 or anything
/// unparseable means 8 s), with the window raised first so the
/// slide really animates; HYLKI_SHOWCASE_BURST writes stills
/// through the first one. In a narrow window (a window.toml
/// under the 1094px breakpoint) that is the floating peek; at
/// full width, or under Focus Mode's rail, it is the rail
/// expanding and folding again.
pub(super) fn stage_peek(root: &adw::ApplicationWindow, sender: &ComponentSender<AppModel>) {
    if let Ok(spec) = std::env::var("HYLKI_SHOWCASE_PEEK") {
        let mut at: Vec<f64> = spec
            .split(',')
            .filter_map(|p| p.trim().parse::<f64>().ok())
            .filter(|s| *s > 1.5)
            .collect();
        if at.is_empty() {
            at.push(8.0);
        }
        let burst = std::env::var("HYLKI_SHOWCASE_BURST").ok();
        let win = root.clone();
        {
            let win = win.clone();
            let first = at[0];
            gtk::glib::timeout_add_local_once(
                std::time::Duration::from_millis(((first - 1.5) * 1000.0) as u64),
                move || win.present(),
            );
        }
        for (n, secs) in at.into_iter().enumerate() {
            let s = sender.clone();
            let win = win.clone();
            let burst = (n == 0).then(|| burst.clone()).flatten();
            gtk::glib::timeout_add_local_once(
                std::time::Duration::from_millis((secs * 1000.0) as u64),
                move || {
                    s.input(AppMsg::ToggleSidebar);
                    if let Some(base) = burst {
                        for (i, ms) in [60u64, 120, 200, 400].iter().enumerate() {
                            let win = win.clone();
                            let path = format!("{base}.{i}.png");
                            gtk::glib::timeout_add_local_once(
                                std::time::Duration::from_millis(*ms),
                                move || {
                                    showcase_capture(win.upcast_ref::<gtk::Widget>(), &path);
                                },
                            );
                        }
                    }
                },
            );
        }
    }
}

impl AppModel {
    /// HYLKI_SHOWCASE_TOGGLE=starred|sent|drafts toggles that
    /// unified row's list at 5s (what a long-press does).
    pub(super) fn stage_toggle(&self) {
        if let Ok(which) = std::env::var("HYLKI_SHOWCASE_TOGGLE") {
            let kind = match which.as_str() {
                "sent" => FolderKind::Sent,
                "drafts" => FolderKind::Drafts,
                "archive" => FolderKind::Archive,
                _ => FolderKind::Starred,
            };
            let sb = self.sidebar.sender().clone();
            gtk::glib::timeout_add_seconds_local_once(5, move || {
                let _ = sb.send(SidebarInput::ToggleKindExpand(kind));
            });
        }
    }

    /// HYLKI_SHOWCASE_FOLD_FILTERED folds All Inboxes' Filtered
    /// Folders section, to check its folded header.
    pub(super) fn stage_fold_filtered(&self) {
        if std::env::var("HYLKI_SHOWCASE_FOLD_FILTERED").is_ok() {
            let sb = self.sidebar.sender().clone();
            gtk::glib::timeout_add_seconds_local_once(3, move || {
                let _ = sb.send(SidebarInput::ToggleFilteredExpand(
                    crate::ui::sidebar::Slot::Unified,
                ));
            });
        }
    }
}
