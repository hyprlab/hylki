//! The sidebar's icon rail and the peek that slides the full sidebar over
//! the list.

use adw::prelude::*;
use relm4::prelude::*;

use crate::ui::sidebar::SidebarInput;

use super::model::AppModel;
use super::msg::AppMsg;
use super::sidebar::set_sidebar_header_compact;
use super::AppModelWidgets;

pub(super) const SIDEBAR_RAIL_WIDTH: f64 = 80.0;

impl AppModel {
    /// Pin the floating hover-peek open as the normal side-by-side sidebar:
    /// the arrow inside the peek, clicked at a width that can host the full
    /// sidebar, persists the expanded state: drop the panel, expand the
    /// docked sidebar's rows, and save.
    fn pin_sidebar_from_peek(&mut self) {
        tracing::info!("peek: pinned to side-by-side");
        self.sidebar_peek = false;
        if !self.sidebar_layout_borrowed() {
            self.sidebar_collapsed = false;
        }
        self.rail_active = false;
        if let Some(peek) = self.peek_split.clone() {
            self.peek_transition.set(true);
            peek.set_show_sidebar(false);
            self.peek_transition.set(false);
        }
        self.sidebar.emit(SidebarInput::SetCollapsed(false));
        // Side-by-side again: settle at the normal expanded width.
        self.animate_sidebar(false);
        self.compact_sidebar_header(false);
        if !self.sidebar_layout_borrowed() {
            self.save_sidebar_state();
        }
    }

    /// Close the floating sidebar overlay if it is open — navigation picked in
    /// it is done with it (mirrors how GNOME's own adaptive sidebars behave).
    pub(super) fn close_sidebar_peek(&mut self) {
        if self.sidebar_peek {
            self.rail_active = true;
            self.set_sidebar_peek(false, true);
        }
    }

    /// Hide or show the sidebar header's title and window controls (icon rail).
    fn compact_sidebar_header(&self, compact: bool) {
        if let (Some(header), Some(title), Some(menu)) = (
            self.sidebar_header.as_ref(),
            self.app_title.as_ref(),
            self.sidebar_menu.as_ref(),
        ) {
            set_sidebar_header_compact(header, title, menu, &self.sidebar_refresh, compact);
        }
    }

    /// Open or close the narrow-window sidebar *peek*: the expanded panel
    /// (the second sidebar instance, with its own header) sliding out over
    /// the panes from the rail's right edge, through the nested split view's
    /// overlay mode. The docked rail is not involved: it keeps its rows,
    /// header and width throughout and stays clickable beside the panel.
    /// `animate` is advisory — libadwaita animates the overlay itself.
    fn set_sidebar_peek(&mut self, open: bool, animate: bool) {
        let Some(peek) = self.peek_split.clone() else {
            return;
        };
        tracing::info!(
            "peek: set open={open} animate={animate} (was peek={}, shown={})",
            self.sidebar_peek,
            peek.shows_sidebar()
        );
        self.sidebar_peek = open;
        // The notify our own transition emits must not read as a dismissal.
        self.peek_transition.set(true);
        peek.set_show_sidebar(open);
        self.peek_transition.set(false);
    }

    /// Send a state update to both sidebar instances — the docked rail and
    /// the peek panel show the same accounts, counts and rows.
    pub(super) fn sidebars_emit(&self, msg: SidebarInput) {
        self.peek_sidebar.emit(msg.clone());
        self.sidebar.emit(msg);
    }

    /// Highlight what the app now shows in both sidebar instances. The one
    /// the user clicked already has it (its guard makes this a no-op), the
    /// other follows; neither reports back.
    pub(super) fn mirror_selection(&self, sel: crate::ui::sidebar::Sel) {
        self.sidebars_emit(SidebarInput::MirrorSelection(sel));
    }

    /// Smoothly animate the sidebar rail between its expanded width and the
    /// narrow icon-only width by interpolating the split view's pinned width.
    fn animate_sidebar(&mut self, collapsing: bool) {
        let Some(split) = self.sidebar_split.clone() else {
            return;
        };
        // Start from the current on-screen width; fall back to sensible defaults.
        let from = split
            .sidebar()
            .map(|w| w.width() as f64)
            .filter(|w| *w > 1.0)
            .unwrap_or(if collapsing {
                256.0
            } else {
                SIDEBAR_RAIL_WIDTH
            });
        let expanded = (0.2 * self.window.width() as f64).clamp(220.0, 280.0);
        let to = if collapsing {
            SIDEBAR_RAIL_WIDTH
        } else {
            expanded
        };

        let s = split.clone();
        let target = adw::CallbackAnimationTarget::new(move |v| {
            s.set_min_sidebar_width(v);
            s.set_max_sidebar_width(v);
        });
        let anim = adw::TimedAnimation::new(&split, from, to, 200, target);
        anim.set_easing(adw::Easing::EaseOutCubic);
        if !collapsing {
            // Restore responsive sizing once expanded, so window resizes track again.
            let s2 = split.clone();
            anim.connect_done(move |_| {
                s2.set_min_sidebar_width(180.0);
                s2.set_max_sidebar_width(280.0);
                s2.set_sidebar_width_fraction(0.2);
            });
        }
        anim.play();
        self.sidebar_anim = Some(anim);
    }

    /// Whether the icon rail marks unread folders with a dot instead of a
    /// count: Focus Mode's switch while it is on, else the setting.
    pub(super) fn rail_dots_now(&self) -> bool {
        if self.focus.enabled {
            self.focus.rail_dots
        } else {
            self.rail_dots
        }
    }

    /// Whether the rail belongs on screen: the window is too narrow for the
    /// full sidebar, or Focus Mode's switch says so while it is on, or else
    /// the user collapsed it themselves. Only the last of those is ever saved.
    pub(super) fn rail_wanted(&self) -> bool {
        self.auto_rail
            || if self.focus.enabled {
                self.focus.rail_sidebar
            } else {
                self.sidebar_collapsed
            }
    }

    /// Whether the sidebar's layout is Focus Mode's for the while: collapsing
    /// or expanding it is a look for as long as the mode lasts and never the
    /// layout the user keeps. Nothing is saved while this holds, and ending
    /// the mode gives back exactly the sidebar it took over — whatever was
    /// done to it meanwhile.
    fn sidebar_layout_borrowed(&self) -> bool {
        self.focus.enabled
    }

    /// Put the rail up or take it down to match [`Self::rail_wanted`],
    /// animating the split the way the toggle does. Persists nothing.
    pub(super) fn sync_rail(&mut self) {
        let want = self.rail_wanted();
        // A floating peek is the expanded sidebar by another name: fold it
        // before the rail goes up under it.
        if want && self.sidebar_peek {
            self.set_sidebar_peek(false, true);
        }
        if want != self.rail_active {
            self.rail_active = want;
            self.sidebar.emit(SidebarInput::SetCollapsed(want));
            self.animate_sidebar(want);
            self.compact_sidebar_header(want);
        }
    }

    /// The messages the router sends here (see `AppModel::route`).
    pub(super) fn on_sidebar_peek(&mut self, msg: AppMsg) {
        match msg {
            AppMsg::SidebarCollapsed(collapsed) => {
                tracing::info!(
                    "peek: sidebar reports collapsed={collapsed} (peek={} auto_rail={} rail_active={})",
                    self.sidebar_peek, self.auto_rail, self.rail_active
                );
                // The sidebar component has already switched its own rows; this
                // is the app-side reaction (split widths, header, persistence).
                // At a width that can host the full sidebar, the arrow inside a
                // floating peek PINS it: the sidebar becomes the normal
                // side-by-side pane (persisted) and hover-expand goes dormant
                // until it is collapsed again. In the narrow window the toggle
                // only opens/closes the overlay — there is no room to pin.
                if self.sidebar_peek && !self.auto_rail && collapsed {
                    self.pin_sidebar_from_peek();
                } else if self.auto_rail {
                    // Narrow window: the rail never expands in place — put
                    // its rows straight back and float the peek panel out
                    // (or fold it) instead. Only a showcase toggle gets here;
                    // the header button goes through AppMsg::ToggleSidebar.
                    self.sidebar.emit(SidebarInput::SetCollapsed(true));
                    self.rail_active = true;
                    self.set_sidebar_peek(!collapsed, true);
                } else {
                    // Focus Mode holding the rail borrows the sidebar's
                    // layout: expanding it for a moment is a look, not a
                    // choice, so only what is on screen moves.
                    if !self.sidebar_layout_borrowed() {
                        self.sidebar_collapsed = collapsed;
                    }
                    self.rail_active = collapsed;
                    self.animate_sidebar(collapsed);
                    self.compact_sidebar_header(collapsed);
                    if !self.sidebar_layout_borrowed() {
                        self.save_sidebar_state();
                    }
                }
            }

            AppMsg::AutoRail(on) => {
                tracing::info!("peek: auto-rail {on} (peek={})", self.sidebar_peek);
                self.auto_rail = on;
                if !on && self.sidebar_peek {
                    // Widened with the panel open: fold it back before the
                    // split view returns to side-by-side.
                    self.set_sidebar_peek(false, false);
                    self.rail_active = true;
                }
                // The rail wins while the window is narrow; the user's own
                // choice comes back the moment there is room again. Nothing is
                // persisted here — this is the window's shape, not a preference.
                let want = self.rail_wanted();
                if want != self.rail_active {
                    self.rail_active = want;
                    self.sidebar.emit(SidebarInput::SetCollapsed(want));
                    self.animate_sidebar(want);
                    self.compact_sidebar_header(want);
                }
            }

            AppMsg::ToggleSidebar => self.toggle_sidebar(),

            AppMsg::SidebarPeekDismissed => {
                tracing::info!("peek: dismissed (peek={})", self.sidebar_peek);
                if self.sidebar_peek {
                    self.rail_active = true;
                    self.set_sidebar_peek(false, true);
                }
            }

            AppMsg::SidebarHoverEnter => {
                // Hover-expand (preference): the icon rail floats the full
                // sidebar out without a click — whether the rail comes from
                // the narrow-window breakpoint or the user's own collapse.
                // The same peek the expand button opens, dismissed the same
                // ways (navigation, or a click outside the panel).
                let rail_up = self.rail_wanted();
                let hover_expand = if self.focus.enabled {
                    self.focus.hover_expand
                } else {
                    self.sidebar_hover_expand
                };
                if hover_expand && rail_up && !self.sidebar_peek {
                    self.rail_active = false;
                    self.set_sidebar_peek(true, true);
                }
            }
            _ => unreachable!("not routed to on_sidebar_peek"),
        }
    }

    /// The message-pane header's sidebar button: flip the sidebar between the
    /// expanded pane and the icon rail. Routed through the sidebar component so
    /// its CollapsedChanged output drives the same peek/pin/persist logic as
    /// ever (see `SidebarCollapsed`).
    fn toggle_sidebar(&mut self) {
        tracing::info!(
            "peek: toggle button (peek={} auto_rail={} rail_active={})",
            self.sidebar_peek,
            self.auto_rail,
            self.rail_active
        );
        // Self-healing: with no peek open, the split should always be
        // un-collapsed and showing (rail or side-by-side). If it ended
        // hidden — a lost race in the peek machinery left users with no
        // sidebar and a dead button until relaunch (seen on 1.17.1) —
        // the button's job is to bring the sidebar back, not to feed
        // a toggle into state that already lost the plot: repair to
        // the rail first, then toggle normally from there.
        if self.sidebar_peek {
            if self.auto_rail {
                // Narrow window: the button folds the floating peek
                // back to the rail.
                self.rail_active = true;
                self.set_sidebar_peek(false, true);
            } else {
                // A hover peek at a width with room for the full
                // sidebar: the button pins it side by side.
                self.pin_sidebar_from_peek();
            }
        } else {
            if let Some(split) = self.sidebar_split.clone() {
                if split.is_collapsed() || !split.shows_sidebar() {
                    tracing::info!(
                        "peek: toggle found split hidden (collapsed={} shown={}) — repairing",
                        split.is_collapsed(),
                        split.shows_sidebar()
                    );
                    self.rail_active = true;
                    self.sidebar.emit(SidebarInput::SetCollapsed(true));
                    self.compact_sidebar_header(true);
                    self.peek_transition.set(true);
                    split.set_min_sidebar_width(SIDEBAR_RAIL_WIDTH);
                    split.set_max_sidebar_width(SIDEBAR_RAIL_WIDTH);
                    split.set_collapsed(false);
                    split.set_show_sidebar(true);
                    self.peek_transition.set(false);
                }
            }
            if self.auto_rail {
                // Narrow window: the toggle floats the peek panel
                // out beside the rail; the rail itself never expands.
                self.rail_active = false;
                self.set_sidebar_peek(true, true);
            } else {
                self.sidebar.emit(SidebarInput::ToggleCollapsed);
            }
        }
    }

    /// Pointer tracking drives the hover peek: with the hover-expand
    /// preference on, moving the pointer over the docked rail floats the
    /// panel out. The peek never folds back on its own — the pointer
    /// leaving used to arm a one-second dismissal, but the panel's menu
    /// popover is its own surface, so merely opening it counted as
    /// leaving and the panel slid away under the menu. Now only a click
    /// outside the panel (the scrim), a swipe, or a navigation closes it.
    /// The handler fires in every mode — the guards in the AppMsg handler
    /// keep it meaningless outside a rail.
    pub(super) fn init_hover_peek(
        &self,
        widgets: &AppModelWidgets,
        sender: &ComponentSender<Self>,
    ) {
        if let Some(rail) = widgets.sidebar_split.sidebar() {
            let motion = gtk::EventControllerMotion::new();
            let armed = std::rc::Rc::new(std::cell::Cell::new(false));
            {
                let armed = armed.clone();
                motion.connect_enter(move |_, _, _| armed.set(true));
            }
            // Hover-open waits for the pointer to actually move over the
            // rail. GTK also synthesises an "enter" when the rail reappears
            // under a resting pointer as the panel slides away — opening on
            // that would fold and float forever.
            let s = sender.input_sender().clone();
            motion.connect_motion(move |_, _, _| {
                if armed.replace(false) {
                    let _ = s.send(AppMsg::SidebarHoverEnter);
                }
            });
            rail.add_controller(motion);
        }
    }
}
