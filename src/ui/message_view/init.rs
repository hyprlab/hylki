//! Building the reader: its first state, and the handlers it hangs on the
//! WebView and the desktop. `init` connects them in this file's order, after
//! the ones `build_webview` registers itself; the order matters (the
//! right-click point is recorded in the capture phase before the menu asks
//! for it).

use super::bridge::{parse_bridge, Bridge};
use super::chrome::ClassRules;
use super::links::{is_launchable_uri, link_destination, CLICK_LINK_PROBE};
use super::menu::{menu_lookup, menu_skipped, MenuHit};
use super::page_fade::PageFade;
use super::types::MessageViewInput;
use super::MessageView;
use adw::prelude::*;
use relm4::prelude::*;
use webkit6::prelude::WebViewExt;

/// Browser-style link preview: a small plaque in the bottom-left corner of
/// the body showing exactly where the link under the pointer goes. GTK
/// tooltips are unreliable over a WebView (WebKit handles motion events
/// itself, so GTK's hover timer often never starts), and a phishing check
/// that only sometimes appears is worse than none.
pub(super) fn link_preview_plaque(webview: &webkit6::WebView) -> gtk::Label {
    let link_preview = gtk::Label::new(None);
    link_preview.add_css_class("link-preview");
    link_preview.set_halign(gtk::Align::Start);
    link_preview.set_valign(gtk::Align::End);
    link_preview.set_visible(false);
    link_preview.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
    link_preview.set_max_width_chars(90);
    link_preview.set_can_target(false); // never intercept clicks meant for the page
    webview.connect_mouse_target_changed({
        let label = link_preview.clone();
        move |_view, hit, _modifiers| {
            let uri = hit.context_is_link().then(|| hit.link_uri()).flatten();
            match uri {
                Some(uri) => {
                    label.set_text(&link_destination(&uri, hit.link_label().as_deref()));
                    label.set_visible(true);
                }
                None => label.set_visible(false),
            }
        }
    });
    link_preview
}

impl MessageView {
    /// The reader before anything is shown.
    pub(super) fn initial(webview: webkit6::WebView, link_preview: &gtk::Label) -> Self {
        MessageView {
            always_show_recipients: false,
            single_message_card: false,
            pgp_labels: false,
            card_atts: std::collections::HashMap::new(),
            card_atts_shown: true,
            drawer_on: true,
            current: None,
            thread: Vec::new(),
            tags: Vec::new(),
            header_tags_rendered: std::cell::RefCell::new(None),
            folder_labels: std::collections::HashMap::new(),
            blocked: false,
            no_autoread: std::collections::HashSet::new(),
            reader_style: crate::config::ReaderStyle::NONE,
            sender_style: std::collections::HashSet::new(),
            reader_mode: false,
            reader_switch_shown: true,
            theme_switch_shown: false,
            theme_picks: std::collections::HashMap::new(),
            zoom: 100,
            zoom_default: 100,
            reader_default: crate::config::ReaderDefault::Remember,
            read_mark: crate::config::ReadMark::default(),
            checked: (0, false),
            show_banner: crate::config::load_privacy().show_remote_banner,
            show_spoof_banner: crate::config::load_privacy().show_spoof_banner,
            card_actions_hover: crate::config::load_privacy().card_actions_hover,
            card_actions_auto: crate::config::load_privacy().card_actions_auto,
            palette_collapse_secs: crate::config::load_privacy().card_palette_collapse_secs,
            card_palette_menu: crate::config::load_privacy().card_palette_menu,
            remote_allowed: false,
            account_name: None,
            account_color: None,
            chip_rules: ClassRules::new(),
            cover_rules: ClassRules::new(),
            account_chip: None,
            cover: None,
            shown_fingerprint: None,
            did_autoscroll: false,
            autoscroll_lone: false,
            saved_anchor: None,
            anchor_gutter: false,
            skip_fade: false,
            drafts_view: false,
            template_folders: std::collections::HashSet::new(),
            frame_heights: std::collections::HashMap::new(),
            instant: false,
            selected_cards: Vec::new(),
            focus_gen: std::rc::Rc::new(std::cell::Cell::new(0)),
            loading: false,
            webview_ready: false,
            find_open: false,
            under_split: false,
            find_matches: None,
            find_entry: None,
            find_return: None,
            find_bottom: false,
            find_bar: None,
            pointer: Default::default(),
            loading_page: None,
            focus_held: std::cell::Cell::new(false),
            find_closed_at: None,
            webview,
            sender_check: None,
            member_checks: std::collections::HashMap::new(),
            unsubscribed: std::collections::HashMap::new(),
            receipt_marks: Default::default(),
            unsub_state: std::collections::HashMap::new(),
            fold_messages: crate::config::FoldMessages::Never,
            fold_preview_lines: 1,
            painted_hidden: false,
            scroll_owed: false,
            sent: std::collections::HashSet::new(),
            folds: std::collections::HashMap::new(),
            translations: std::collections::HashMap::new(),
            detected: std::cell::RefCell::new(std::collections::HashMap::new()),
            identities: std::collections::HashMap::new(),
            invite_answers: std::collections::HashMap::new(),
            link_preview: link_preview.clone(),
            seq: std::cell::Cell::new(0),
            pending_load: Default::default(),
            page_fade: PageFade::new(),
            content_dark: None,
        }
    }

    /// Reveal the page once a load has finished, with a backstop for a page that
    /// never says its frames have settled.
    pub(super) fn watch_loads(&self, sender: &ComponentSender<Self>) {
        // The document being loaded is not the same as it being ready to look at:
        // each message body is a frame that loads afterwards and is then sized
        // from its placeholder height, so revealing here shows every card at the
        // wrong size and then jumping. The page says when its frames have
        // settled; this is only the backstop for a page that never does.
        let ready_sender = sender.clone();
        // Debug hook: HYLKI_SHOWCASE_CLICK_LINK=1 clicks the first web link
        // in the first message that has one, once, a few seconds after a
        // conversation is in, and logs what the click landed on. Exercises
        // the whole link path (frame → policy decision → launcher, #202)
        // without a pointer.
        let click_link = std::env::var("HYLKI_SHOWCASE_CLICK_LINK").is_ok();
        let clicked = std::rc::Rc::new(std::cell::Cell::new(false));
        let page_fade = self.page_fade.clone();
        self.webview.connect_load_changed(move |view, event| {
            if event == webkit6::LoadEvent::Finished {
                page_fade.release(view);
                let s = ready_sender.clone();
                gtk::glib::timeout_add_local_once(
                    std::time::Duration::from_millis(600),
                    move || {
                        s.input_sender().emit(MessageViewInput::Rendered);
                    },
                );
                if click_link && !clicked.get() {
                    let view = view.clone();
                    let clicked = clicked.clone();
                    gtk::glib::timeout_add_local_once(
                        std::time::Duration::from_secs(4),
                        move || {
                            view.evaluate_javascript(
                                CLICK_LINK_PROBE,
                                None,
                                None,
                                None::<&gtk::gio::Cancellable>,
                                move |r| match r {
                                    Ok(v) => {
                                        let text = v.to_str().to_string();
                                        if text.starts_with("clicked") {
                                            clicked.set(true);
                                        }
                                        tracing::info!("showcase: click-link probe: {text}");
                                    }
                                    Err(e) => {
                                        tracing::warn!("showcase: click-link probe failed: {e}")
                                    }
                                },
                            );
                        },
                    );
                }
            }
        });
    }

    /// The reader's right-click menu (see the comment inside).
    pub(super) fn watch_context_menu(&self, sender: &ComponentSender<Self>) {
        self.watch_pointer(sender);
        // A right-click on a card (its header, or its body frame, which
        // never reports events to the page) shows the message's own menu,
        // the same one the list row has; the ground around the cards shows
        // none. WebKit says what the click hit but not where the pointer
        // is, so a capture-phase gesture records the point first; the page
        // is then asked which card holds it. Links,
        // pictures and selected text used to get WebKit's own short menu
        // instead; their entries now lead the message's.
        {
            let point = std::rc::Rc::new(std::cell::Cell::new((0.0f64, 0.0f64)));
            let click = gtk::GestureClick::new();
            click.set_button(3);
            click.set_propagation_phase(gtk::PropagationPhase::Capture);
            {
                let point = point.clone();
                click.connect_pressed(move |_, _, x, y| point.set((x, y)));
            }
            self.webview.add_controller(click);
            let menu_sender = sender.clone();
            self.webview.connect_context_menu(move |view, _menu, hit| {
                // Always the message's menu, wherever the click lands (a
                // link, a picture or selected text only adds its own entries
                // at the top), so the menu never depends on what the mail's
                // HTML happens to have under the pointer.
                let mut target = MenuHit {
                    link: hit
                        .context_is_link()
                        .then(|| hit.link_uri())
                        .flatten()
                        .map(|u| u.to_string())
                        .filter(|u| is_launchable_uri(u)),
                    image: hit
                        .context_is_image()
                        .then(|| hit.image_uri())
                        .flatten()
                        .map(|u| u.to_string()),
                    selection: None,
                    translate: None,
                    folds: None,
                };
                let selected = hit.context_is_selection();
                let (x, y) = point.get();
                let width = view.width().max(1);
                let s = menu_sender.clone();
                view.evaluate_javascript(
                    &format!("window.__hylkiMenuAt({x},{y},{width})"),
                    None,
                    None,
                    None::<&gtk::gio::Cancellable>,
                    move |r| {
                        let json = r.map(|v| v.to_str().to_string()).unwrap_or_default();
                        if menu_skipped(&json) {
                            return;
                        }
                        let (key, text) = menu_lookup(&json);
                        if key.is_none() {
                            tracing::debug!("reader menu: no card at {x:.0},{y:.0} ({json:?})");
                        }
                        if selected {
                            target.selection = text;
                        }
                        s.input_sender().emit(MessageViewInput::CardMenuAt {
                            key,
                            x,
                            y,
                            hit: target,
                        });
                    },
                );
                true
            });
        }
    }

    /// The `hylki` script-message handler.
    pub(super) fn watch_bridge(&self, sender: &ComponentSender<Self>) {
        // Everything the page posts to the `hylki` handler (see bridge.rs).
        if let Some(ucm) = self.webview.user_content_manager() {
            let open_sender = sender.clone();
            ucm.connect_script_message_received(
                Some("hylki"),
                move |_ucm, value| match parse_bridge(&value.to_str()) {
                    Some(Bridge::Input(input)) => open_sender.input_sender().emit(input),
                    Some(Bridge::Clipboard(text)) => {
                        if let Some(display) = gtk::gdk::Display::default() {
                            display.clipboard().set_text(&text);
                        }
                    }
                    None => {}
                },
            );
        }
    }

    /// Render again when the map state, the light/dark preference or the
    /// appearance theme changes.
    pub(super) fn watch_theme(&self, sender: &ComponentSender<Self>) {
        // Re-render the body when the light/dark preference changes so unstyled
        // content tracks the theme live.
        {
            let s = sender.input_sender().clone();
            self.webview.connect_map(move |_| {
                let _ = s.send(MessageViewInput::Mapped);
            });
        }
        // The style manager outlives a message window's reader, so the
        // handler takes itself off once the reader is gone (#488).
        {
            let s = sender.input_sender().clone();
            let handler = std::rc::Rc::new(std::cell::Cell::new(None));
            let id = adw::StyleManager::default().connect_dark_notify({
                let handler = handler.clone();
                move |manager| {
                    if s.send(MessageViewInput::ThemeChanged).is_err() {
                        if let Some(id) = handler.take() {
                            manager.disconnect(id);
                        }
                    }
                }
            });
            handler.set(Some(id));
        }
        // The same for an appearance theme, which moves the colors without
        // touching the light/dark preference. A closed message window's
        // reader drops its watcher the next time the theme changes.
        {
            let s = sender.input_sender().clone();
            crate::desktop::theme::connect_changed(move || {
                s.send(MessageViewInput::ThemeChanged).is_ok()
            });
        }
    }
}
