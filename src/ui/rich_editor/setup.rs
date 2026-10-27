//! Building a [`RichEditor`]: the web view and everything wired to it, in
//! the order the handlers have always been connected.
//!
//! Every closure here holds the view weakly. A strong clone in one of them
//! is a reference cycle GObject never breaks, which kept a closed
//! composer's 300 MB web process alive (#221).

use adw::prelude::*;
use webkit6::prelude::WebViewExt;

use super::context_menu::connect_context_menu;
use super::documents::{document, image_policy_meta, reground};
use super::files::{attach_all, deliver_files};
use super::spelling::apply_spellcheck;
use super::toolbar::build_toolbar;
use super::{
    exec, fade_in, history_key, show_emoji_chooser, AttachCallback, HistoryCallback, RichEditor,
    SourceKind, SpellLangsCallback, ThemeHandlerGuard, PLACE_CARET,
};

impl RichEditor {
    /// `remote_images` is `None` when the editor may load any picture. A
    /// quoted original whose remote content the reader has not loaded gives
    /// the list the editor is limited to instead (the user's own signature
    /// pictures): opening a reply must not fetch what reading the message
    /// did not (#295). The markup keeps every address, so the recipient
    /// gets the quote whole.
    pub(crate) fn new(initial_html: &str, remote_images: Option<&[String]>, plain: bool) -> Self {
        let plain = std::rc::Rc::new(std::cell::Cell::new(plain));
        let image_policy: std::rc::Rc<str> = image_policy_meta(remote_images).into();
        let (webview, ucm) = editor_view();
        let spinner = loading_spinner(&webview);
        crate::desktop::web_fonts::load_html(
            &webview,
            &document(initial_html, &webview, &image_policy, plain.get()),
            Some("https://hylki.localhost/editor"),
        );
        let theme_handler = follow_theme(&webview);

        // WebKitGTK never turns Ctrl+Z into an editing command: its key
        // binding translator forwards the key to a hidden GtkTextView and
        // listens for that widget's *signals*, and undo is a GtkTextView
        // action rather than a signal — so nothing comes back, and its own
        // custom table (bold, italic, tab, the newlines, Ctrl+Shift+V) has
        // no undo entry. The document's `execCommand('undo')` is refused
        // from script for the same reason it would be in a browser. Only
        // the widget API reaches the page's history, so the editor drives
        // it itself, and mirrors what it can do for the host to read.
        let text_history = std::rc::Rc::new(std::cell::Cell::new((false, false)));
        let history_cb: HistoryCallback = std::rc::Rc::new(std::cell::RefCell::new(None));
        mirror_history(&webview, &text_history, &history_cb);
        bind_history_keys(&webview);

        let attach_cb: AttachCallback = std::rc::Rc::new(std::cell::RefCell::new(None));
        // Files to put in the text that arrived before the document did (a
        // message started from files dropped on the main window): they go
        // in once it has loaded, and not into the document being replaced.
        let loaded = std::rc::Rc::new(std::cell::Cell::new(false));
        let pending_files: std::rc::Rc<std::cell::RefCell<Vec<std::path::PathBuf>>> =
            std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        deliver_pending_on_load(&webview, &loaded, &pending_files, &attach_cb, &plain);
        // Rich to begin with; [`RichEditor::set_source`] moves it.
        let source: std::rc::Rc<std::cell::Cell<Option<SourceKind>>> =
            std::rc::Rc::new(std::cell::Cell::new(None));
        let spell_langs_cb: SpellLangsCallback = Default::default();
        // The stock editable menu's single "Paste" hides the plain/rich choice
        // behind the preference; the menu offers both, always, in its place.
        // Over an image the menu leads with image actions — cut, copy, and
        // demoting an inline picture to an ordinary attachment — acting on
        // the exact node the right-click landed on (`__hylkiCtxImg`).
        connect_context_menu(
            &webview,
            attach_cb.clone(),
            text_history.clone(),
            plain.clone(),
            spell_langs_cb.clone(),
        );

        // Files dropped from a file manager never reach the document: WebKitGTK
        // hands the page a uri-list it then refuses to serve, so the drop
        // handler in PASTE_SCRIPT sees an empty `DataTransfer.files` and lets
        // WebKit insert the path as a link. Only the widget can see them, so
        // the widget takes them — declaring just `GdkFileList` leaves every
        // other drag (text, an image dragged out of the reader) to WebKit.
        add_file_drop_target(&webview, &attach_cb, &source, &plain);

        // The editor and (later) its preview are pages of one stack, so a
        // preview swaps in without the editor document being torn down and
        // rebuilt — which would cost the undo history and the caret.
        let stack = gtk::Stack::new();
        stack.set_vexpand(true);
        let edit = gtk::Overlay::new();
        edit.set_child(Some(&webview));
        edit.add_overlay(&spinner);
        stack.add_named(&edit, Some("edit"));

        let expanded = crate::config::load_privacy().toolbar_expanded;
        let (toolbar, toolbar_commands, toolbar_end, bar_state) =
            build_toolbar(&webview, &stack, &attach_cb, expanded);
        // `fmtState` in PASTE_SCRIPT posts where the caret is: the block
        // kinds it sits in (`q` quote, `u` bulleted list, `o` numbered
        // list), its paragraph style and its font. A set toggle also means a
        // click on it leaves that block instead of opening another (issue
        // #137, EmmanuelP's follow-up).
        ucm.connect_script_message_received(Some("hylkiFormat"), move |_, value| {
            bar_state.update(&value.to_str());
        });

        let frame = editor_frame(&stack);

        bind_emoji_keys(&webview, &stack);

        let bx = gtk::Box::new(gtk::Orientation::Vertical, 6);
        bx.append(&toolbar);
        bx.append(&frame);

        RichEditor {
            widget: bx,
            webview,
            toolbar_commands,
            toolbar_end,
            stack,
            preview: std::rc::Rc::new(std::cell::RefCell::new(None)),
            source: source.clone(),
            plain,
            attach_cb,
            loaded,
            pending_files,
            text_history,
            history_cb,
            spell_langs_cb,
            _theme_handler: std::rc::Rc::new(ThemeHandlerGuard(Some(theme_handler))),
            image_policy,
            zoom: std::rc::Rc::new(std::cell::Cell::new(1.0)),
        }
    }
}

/// The editor's web view, on the shared context, with its message
/// handlers and settings.
fn editor_view() -> (webkit6::WebView, webkit6::UserContentManager) {
    // The shared document-viewer context (issue #106): a default-context
    // view would bring up a second web process with browser-sized caches.
    // Spell checking rides the same context; refreshing it here keeps a
    // just-changed preference honored without a restart.
    apply_spellcheck();
    // The message handler carries the word under the caret out for a
    // mid-word spelling verdict (see the paste script's caretWord).
    let ucm = webkit6::UserContentManager::new();
    ucm.register_script_message_handler("hylkiSpell", None);
    // The block state under the caret (quote, bulleted or numbered list)
    // comes out through this one, to light the matching toolbar button.
    ucm.register_script_message_handler("hylkiFormat", None);
    let webview = webkit6::WebView::builder()
        .web_context(&crate::ui::message_view::shared_web_context())
        .user_content_manager(&ucm)
        .build();
    crate::desktop::memory_report::register_web_view(&webview, "editor");
    crate::ui::message_view::keep_tab_out_of_key_translator(&webview);
    // Everything the view itself owns (its content manager's handlers,
    // its event controllers, its toolbar's buttons via the format-state
    // handler) holds the view WEAKLY. A strong clone in any of those
    // closures is a reference cycle GObject never breaks: the editor of
    // a closed composer stayed alive that way, and with it its 300 MB
    // WebKitWebProcess, for the rest of the session (#221).
    {
        let weak = webview.downgrade();
        ucm.connect_script_message_received(Some("hylkiSpell"), move |_, value| {
            let Some(v) = weak.upgrade() else { return };
            let word = value.to_str().to_string();
            let bad = crate::desktop::spell::word_is_misspelled(&word);
            exec(&v, &format!("window.__hylkiSpellMark({bad})"));
        });
    }
    let settings = webkit6::Settings::new();
    settings.set_enable_javascript(true);
    settings.set_enable_developer_extras(false);
    // A script error in the editor document (all our own code) is
    // otherwise completely silent.
    settings.set_enable_write_console_messages_to_stdout(true);
    webview.set_settings(&settings);
    // Transparent until the document paints: a fresh WebView otherwise
    // renders an opaque default surface (black) for its first frames,
    // which flashes while the inline composer slides over the reader.
    // The document itself paints `Canvas` below, so the editor's normal
    // ground appears with the first real frame.
    webview.set_background_color(&gtk::gdk::RGBA::new(0.0, 0.0, 0.0, 0.0));
    // The document paints asynchronously: the host panel slides in
    // smoothly and the editor's content would land a beat later as a
    // pop. Start the view invisible and fade it in when its load
    // finishes — content arrives as a fade into the settled panel.
    webview.set_opacity(0.0);
    (webview, ucm)
}

/// The spinner shown while the editor's web process starts, and the fade
/// that brings the view in once its document has loaded.
fn loading_spinner(webview: &webkit6::WebView) -> gtk::Spinner {
    // Starting the editor's web process takes a moment, longest the
    // first time: past a glance, a spinner says it is coming (#392).
    let spinner = gtk::Spinner::new();
    spinner.set_size_request(32, 32);
    spinner.set_halign(gtk::Align::Center);
    spinner.set_valign(gtk::Align::Center);
    spinner.set_can_target(false);
    spinner.set_visible(false);
    let shown = std::rc::Rc::new(std::cell::Cell::new(false));
    {
        let (sp, shown) = (spinner.downgrade(), shown.clone());
        gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(150), move || {
            if let Some(sp) = sp.upgrade().filter(|_| !shown.get()) {
                sp.set_visible(true);
                sp.start();
            }
        });
    }
    let sp = spinner.downgrade();
    webview.connect_load_changed(move |v, ev| {
        if ev == webkit6::LoadEvent::Finished {
            shown.set(true);
            if let Some(sp) = sp.upgrade() {
                sp.stop();
                sp.set_visible(false);
            }
            fade_in(v);
            // Focus given before the document was there reached the
            // view but no caret: place it now.
            let root_focus = v.root().and_then(|r| r.focus());
            if root_focus.is_some_and(|f| f == *v.upcast_ref::<gtk::Widget>() || f.is_ancestor(v)) {
                exec(v, PLACE_CARET);
            }
        }
    });
    {
        // Failsafe: whatever happens to the load, the editor must never
        // stay invisible.
        let (v, sp) = (webview.clone(), spinner.downgrade());
        gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(1200), move || {
            if v.opacity() < 1.0 {
                fade_in(&v);
            }
            if let Some(sp) = sp.upgrade() {
                sp.stop();
                sp.set_visible(false);
            }
        });
    }
    spinner
}

/// Re-ground the document when the theme changes; the handler is
/// disconnected with the editor's last handle.
fn follow_theme(webview: &webkit6::WebView) -> gtk::glib::SignalHandlerId {
    // A live theme flip re-grounds the open document (#148): the scheme
    // and the ground are baked into the document at load, so without
    // this the editor stays in the scheme it was opened in. Deferred to
    // the next main-loop pass — the theme's named colors are only
    // re-resolved after the signal fires.
    let handler = {
        let weak = webview.downgrade();
        adw::StyleManager::default().connect_dark_notify(move |sm| {
            reground(&weak, sm.is_dark());
        })
    };
    // An appearance theme moves the same colors with no scheme flip, so
    // the editor re-grounds for that too — and drops out of the list
    // once its view is gone.
    {
        let weak = webview.downgrade();
        crate::desktop::theme::connect_changed(move || {
            reground(&weak, adw::StyleManager::default().is_dark())
        });
    }
    handler
}

/// Mirror WebKit's undo and redo availability into `history` and tell
/// `cb` when it changes.
fn mirror_history(
    webview: &webkit6::WebView,
    text_history: &std::rc::Rc<std::cell::Cell<(bool, bool)>>,
    history_cb: &HistoryCallback,
) {
    if let Some(state) = webview.editor_state() {
        let cell = text_history.clone();
        let cb = history_cb.clone();
        // Seeded before the first change so a host that asks early is
        // not told the document can undo when it cannot.
        cell.set((state.is_undo_available(), state.is_redo_available()));
        state.connect_changed(move |state| {
            let now = (state.is_undo_available(), state.is_redo_available());
            if cell.replace(now) == now {
                return;
            }
            if let Some(f) = cb.borrow().as_ref() {
                f(now.0, now.1);
            }
        });
    }
}

/// Undo and redo keys over the body.
fn bind_history_keys(webview: &webkit6::WebView) {
    // Ctrl+Z, Ctrl+Shift+Z and Ctrl+Y over the body, in the capture
    // phase so they are decided before WebKit sees the key at all.
    // A host with a wider history of its own — the composer, whose
    // undo also covers its attachments — catches them further up,
    // where capture reaches it first, and this never runs.
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    let weak = webview.downgrade();
    keys.connect_key_pressed(move |_, keyval, _, state| {
        let Some(v) = weak.upgrade() else {
            return gtk::glib::Propagation::Proceed;
        };
        match history_key(keyval, state) {
            Some(redo) => {
                v.execute_editing_command(if redo {
                    webkit6::EDITING_COMMAND_REDO
                } else {
                    webkit6::EDITING_COMMAND_UNDO
                });
                gtk::glib::Propagation::Stop
            }
            None => gtk::glib::Propagation::Proceed,
        }
    });
    webview.add_controller(keys);
}

/// Put files that arrived before the document into it once it loads.
fn deliver_pending_on_load(
    webview: &webkit6::WebView,
    loaded: &std::rc::Rc<std::cell::Cell<bool>>,
    pending_files: &std::rc::Rc<std::cell::RefCell<Vec<std::path::PathBuf>>>,
    attach_cb: &AttachCallback,
    plain: &std::rc::Rc<std::cell::Cell<bool>>,
) {
    let (loaded, pending, cb) = (loaded.clone(), pending_files.clone(), attach_cb.clone());
    let plain = plain.clone();
    webview.connect_load_changed(move |v, ev| match ev {
        webkit6::LoadEvent::Started => loaded.set(false),
        webkit6::LoadEvent::Finished => {
            loaded.set(true);
            let paths = std::mem::take(&mut *pending.borrow_mut());
            if !paths.is_empty() {
                let files: Vec<_> = paths.iter().map(gtk::gio::File::for_path).collect();
                deliver_files(v, &files, None, &cb, !plain.get());
            }
        }
        _ => {}
    });
}

/// Take files dropped from a file manager, which WebKit would insert as
/// links.
fn add_file_drop_target(
    webview: &webkit6::WebView,
    attach_cb: &AttachCallback,
    source: &std::rc::Rc<std::cell::Cell<Option<SourceKind>>>,
    plain: &std::rc::Rc<std::cell::Cell<bool>>,
) {
    let drop = gtk::DropTarget::new(
        gtk::gdk::FileList::static_type(),
        gtk::gdk::DragAction::COPY,
    );
    drop.set_propagation_phase(gtk::PropagationPhase::Capture);
    let weak = webview.downgrade();
    let cb = attach_cb.clone();
    let src = source.clone();
    let plain = plain.clone();
    drop.connect_drop(move |_, value, x, y| {
        let Some(v) = weak.upgrade() else {
            return false;
        };
        let Ok(list) = value.get::<gtk::gdk::FileList>() else {
            return false;
        };
        // Source mode has no document to hold a picture: everything
        // dropped becomes an attachment, and the user writes the
        // reference to it themselves.
        if src.get().is_some() {
            return attach_all(&list.files(), &cb);
        }
        deliver_files(&v, &list.files(), Some((x, y)), &cb, !plain.get())
    });
    webview.add_controller(drop);
}

/// The card the editor sits in.
fn editor_frame(stack: &gtk::Stack) -> gtk::Frame {
    let frame = gtk::Frame::new(None);
    frame.set_vexpand(true);
    // Same card treatment as the address fields' boxed-list: shadow and
    // radius from libadwaita, with the WebView clipped to the corners.
    frame.add_css_class("card");
    // Found by name when a sent reply's editor turns into its card in
    // the conversation (send_morph.rs).
    frame.add_css_class("compose-editor-card");
    frame.set_overflow(gtk::Overflow::Hidden);
    frame.set_child(Some(stack));
    frame
}

/// Ctrl+. and Ctrl+; open the emoji chooser at the caret.
fn bind_emoji_keys(webview: &webkit6::WebView, stack: &gtk::Stack) {
    // Ctrl+. and Ctrl+; open WebKitGTK's own emoji chooser, but
    // what is picked in it never reaches the page (#348; WebKitGTK
    // 2.50 to 2.54 at least, a bare WebView does the same). The
    // editor opens GTK's chooser itself, at the caret, and types
    // the emoji in.
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    let weak = webview.downgrade();
    let anchor = stack.downgrade();
    keys.connect_key_pressed(move |_, keyval, _, state| {
        use gtk::gdk::{Key, ModifierType};
        let mods = state
            & (ModifierType::CONTROL_MASK | ModifierType::SHIFT_MASK | ModifierType::ALT_MASK);
        if mods != ModifierType::CONTROL_MASK || !matches!(keyval, Key::period | Key::semicolon) {
            return gtk::glib::Propagation::Proceed;
        }
        let (Some(v), Some(anchor)) = (weak.upgrade(), anchor.upgrade()) else {
            return gtk::glib::Propagation::Proceed;
        };
        show_emoji_chooser(&v, &anchor);
        gtk::glib::Propagation::Stop
    });
    webview.add_controller(keys);
}
