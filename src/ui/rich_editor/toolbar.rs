//! The formatting toolbar: its buttons and menus, and the state it mirrors from
//! the document.

use super::AttachCallback;

use super::super::tool_flow::ToolFlow;
use super::colors::{color_glyph, color_menu};
use super::files::deliver_files;
use super::fonts::{
    choose_family, fill_recent_fonts, first_family, font_js, font_label, preview_font, FONTS, SIZES,
};
use super::{exec, js_escape, show_emoji_chooser};
use crate::i18n::{i18n, i18n_noop};
use adw::prelude::*;

/// What the format bar shows of where the caret is: the block toggles,
/// keyed the way `fmtState` in PASTE_SCRIPT reports them (`q` quote, `u`
/// bulleted list, `o` numbered list), and the paragraph-style, font and size
/// menus, whose labels name the caret's block, font and size (#358).
pub(super) struct ToolbarState {
    toggles: Vec<(char, gtk::ToggleButton)>,
    style: gtk::MenuButton,
    font: gtk::MenuButton,
    size: gtk::MenuButton,
}

impl ToolbarState {
    /// Follow a `fmtState` report: `<toggles>|<block tag>|<font>|<points>`.
    pub(super) fn update(&self, report: &str) {
        let mut parts = report.split('|');
        let toggles = parts.next().unwrap_or("");
        for (key, button) in &self.toggles {
            let on = toggles.contains(*key);
            if button.is_active() != on {
                button.set_active(on);
            }
        }
        let block = parts.next().unwrap_or("");
        let style = BLOCK_STYLES
            .iter()
            .find(|(tag, _)| *tag == block)
            .map_or(BLOCK_STYLES[0].1, |(_, l)| *l);
        self.style.set_label(&i18n(style));
        self.font.set_label(&font_label(parts.next().unwrap_or("")));
        let size = parts.next().unwrap_or("");
        if !size.is_empty() {
            self.size.set_label(size);
        }
    }
}

/// The paragraph styles (#358): the `formatBlock` tag and its name.
const BLOCK_STYLES: &[(&str, &str)] = &[
    ("p", i18n_noop("Normal")),
    ("h1", i18n_noop("Heading 1")),
    ("h2", i18n_noop("Heading 2")),
    ("h3", i18n_noop("Heading 3")),
    ("pre", i18n_noop("Preformatted")),
];

/// A JavaScript string literal of `s`.
pub(super) fn js_str(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "''".into())
}

/// One command of the format bar: a flat icon button that never takes the
/// focus, so the editor keeps its selection.
fn bar_button(icon: &str, tip: &str) -> gtk::Button {
    let btn = gtk::Button::from_icon_name(icon);
    btn.set_tooltip_text(Some(i18n(tip).as_str()));
    btn.set_can_focus(false);
    btn.add_css_class("flat");
    btn
}

/// Run `js` in the editor, the caret first put back where it was when a
/// menu of the bar opened, and give the editor the focus again.
pub(super) fn run_restored(webview: &gtk::glib::WeakRef<webkit6::WebView>, js: &str) {
    if let Some(wv) = webview.upgrade() {
        exec(
            &wv,
            &format!("window.__hylkiRestore&&window.__hylkiRestore();{js}"),
        );
        wv.grab_focus();
    }
}

/// A menu of the bar. Opening it keeps the caret, which the popover's own
/// focus would otherwise cost the document. Its content scrolls, so that
/// [`fit_menu`] can keep it to the room there is.
pub(super) fn bar_menu(
    webview: &webkit6::WebView,
    tip: &str,
    content: &impl IsA<gtk::Widget>,
) -> (gtk::MenuButton, gtk::Popover) {
    let scroller = gtk::ScrolledWindow::new();
    scroller.set_hscrollbar_policy(gtk::PolicyType::Never);
    scroller.set_propagate_natural_width(true);
    scroller.set_propagate_natural_height(true);
    scroller.set_child(Some(content));
    let popover = gtk::Popover::new();
    popover.set_child(Some(&scroller));
    popover.add_css_class("menu");
    let weak = webview.downgrade();
    popover.connect_show(move |_| {
        if let Some(wv) = weak.upgrade() {
            exec(&wv, "window.__hylkiSave&&window.__hylkiSave()");
        }
    });
    let btn = gtk::MenuButton::new();
    btn.set_popover(Some(&popover));
    btn.set_tooltip_text(Some(i18n(tip).as_str()));
    btn.set_can_focus(false);
    btn.add_css_class("flat");
    btn.set_create_popup_func(fit_menu);
    (btn, popover)
}

/// Keep a bar menu to the room above or below its button, whichever is
/// larger, scrolling past that. A popover taller than the room there is
/// never opened at all (#358): the font menu, with its recent fonts, did
/// not open from a composer near the foot of the screen.
fn fit_menu(btn: &gtk::MenuButton) {
    let Some(scroller) = btn
        .popover()
        .and_then(|p| p.child())
        .and_downcast::<gtk::ScrolledWindow>()
    else {
        return;
    };
    let Some(root) = btn.root() else { return };
    let root = root.upcast_ref::<gtk::Widget>();
    let Some(at) = btn.compute_point(root, &gtk::graphene::Point::new(0.0, 0.0)) else {
        return;
    };
    let above = at.y() as i32;
    let below = root.height() - above - btn.height();
    // The popover's arrow, frame and padding, and a margin from the edge.
    scroller.set_max_content_height((above.max(below) - 40).max(120));
}

/// A list of menu items, each running its command in the editor.
fn menu_items(
    webview: &webkit6::WebView,
    popover: &gtk::glib::WeakRef<gtk::Popover>,
    items: &[(String, String, Option<&str>)],
) -> gtk::Box {
    let list = gtk::Box::new(gtk::Orientation::Vertical, 0);
    for (label, js, class) in items {
        let text = gtk::Label::new(Some(label));
        text.set_xalign(0.0);
        if let Some(c) = class {
            text.add_css_class(c);
        }
        let item = gtk::Button::new();
        item.set_child(Some(&text));
        item.add_css_class("flat");
        item.add_css_class("context-menu-item");
        let (weak, pop, js) = (webview.downgrade(), popover.clone(), js.clone());
        item.connect_clicked(move |_| {
            if let Some(p) = pop.upgrade() {
                p.popdown();
            }
            run_restored(&weak, &js);
        });
        list.append(&item);
    }
    list
}

/// The format bar: the character styles, lists, quote, link and Clear
/// formatting, all of which Markdown can say, then a chevron that shows
/// paragraph styles and fonts, colors, indents, emoji and pictures (#358),
/// shown from the start when `expanded`. Its groups wrap onto a second row
/// in a narrow composer.
pub(super) fn build_toolbar(
    webview: &webkit6::WebView,
    anchor: &gtk::Stack,
    attach_cb: &AttachCallback,
    expanded: bool,
) -> (ToolFlow, ToolFlow, gtk::Box, ToolbarState) {
    // The formatting commands flow and wrap across the bar, and a host can
    // hide them (a message written as source has no use for them) while the
    // bar itself stays: the format chooser holds its top right corner, and
    // only the first row stops short of it.
    let group = ToolFlow::new();
    group.add_css_class("toolbar");
    group.add_css_class("format-bar");
    group.set_hexpand(true);
    let end = gtk::Box::new(gtk::Orientation::Horizontal, 2);
    let (styles, style, font, size) = style_menus(webview);

    let chars = cluster();
    simple_buttons(
        webview,
        &chars,
        &[
            (
                "format-text-bold-symbolic",
                i18n_noop("Bold"),
                "document.execCommand('bold')",
            ),
            (
                "format-text-italic-symbolic",
                i18n_noop("Italic"),
                "document.execCommand('italic')",
            ),
            (
                "format-text-underline-symbolic",
                i18n_noop("Underline"),
                "document.execCommand('underline')",
            ),
            (
                "format-text-strikethrough-symbolic",
                i18n_noop("Strikethrough"),
                "document.execCommand('strikeThrough')",
            ),
        ],
    );
    group.append(&chars);

    let colors = color_menus(webview);

    // The block commands are toggles: the document reports whether the
    // caret is inside one, and each command both opens and leaves its block
    // (the lists through execCommand's own toggling, the quote through
    // __hylkiQuote). A toggle shows the pressed look while the caret is in
    // its block; the click flips it at once and the document's next state
    // report settles it, so it never sticks wrong.
    let mut toggles = Vec::new();
    let lists = cluster();
    lists.append(&block_toggle(
        webview,
        "view-list-bullet-symbolic",
        i18n_noop("Bulleted list"),
        "document.execCommand('insertUnorderedList')",
        'u',
        &mut toggles,
    ));
    lists.append(&block_toggle(
        webview,
        "view-list-ordered-symbolic",
        i18n_noop("Numbered list"),
        "document.execCommand('insertOrderedList')",
        'o',
        &mut toggles,
    ));
    group.append(&lists);
    // Inside a list, indenting nests the items under the one above.
    let indents = cluster();
    simple_buttons(
        webview,
        &indents,
        &[
            (
                "format-indent-less-symbolic",
                i18n_noop("Decrease indent"),
                "window.__hylkiIndent(false)",
            ),
            (
                "format-indent-more-symbolic",
                i18n_noop("Increase indent"),
                "window.__hylkiIndent(true)",
            ),
        ],
    );

    let inserts = cluster();
    // Adwaita has no blockquote glyph; the bubble with its quote marks
    // reads as one.
    inserts.append(&block_toggle(
        webview,
        "format-quote-symbolic",
        i18n_noop("Quote"),
        "window.__hylkiQuote()",
        'q',
        &mut toggles,
    ));
    let link = bar_button("insert-link-symbolic", i18n_noop("Insert link"));
    {
        let weak = webview.downgrade();
        link.connect_clicked(move |b| {
            if let Some(wv) = weak.upgrade() {
                prompt_link(&wv, b);
            }
        });
    }
    inserts.append(&link);
    group.append(&inserts);
    let media = cluster();
    let emoji = bar_button("face-smile-symbolic", i18n_noop("Insert emoji"));
    {
        let (weak, anchor) = (webview.downgrade(), anchor.downgrade());
        emoji.connect_clicked(move |_| {
            if let (Some(wv), Some(anchor)) = (weak.upgrade(), anchor.upgrade()) {
                show_emoji_chooser(&wv, &anchor);
            }
        });
    }
    media.append(&emoji);
    let picture = bar_button("insert-image-symbolic", i18n_noop("Insert picture"));
    {
        let (weak, cb) = (webview.downgrade(), attach_cb.clone());
        picture.connect_clicked(move |b| {
            let Some(wv) = weak.upgrade() else { return };
            pick_pictures(&wv, b, &cb);
        });
    }
    media.append(&picture);

    let tail = cluster();
    simple_buttons(
        webview,
        &tail,
        &[(
            "edit-clear-symbolic",
            i18n_noop("Clear formatting"),
            "document.execCommand('removeFormat')",
        )],
    );
    group.append(&tail);

    let more = chevron_tools(
        &group,
        &tail,
        [&styles, &colors, &indents, &media],
        expanded,
    );
    showcase_toolbar_toggle(&more);

    group.set_end(&end);
    (
        group.clone(),
        group,
        end,
        ToolbarState {
            toggles,
            style,
            font,
            size,
        },
    )
}

/// A group of tools that wraps on its own, so a composer narrower than a
/// group takes its buttons one by one rather than growing past its pane.
fn cluster() -> adw::WrapBox {
    let c = adw::WrapBox::new();
    c.set_child_spacing(2);
    c.set_line_spacing(2);
    c
}

/// Paragraph style, font and size, each a menu whose label follows the
/// caret.
fn style_menus(
    webview: &webkit6::WebView,
) -> (
    adw::WrapBox,
    gtk::MenuButton,
    gtk::MenuButton,
    gtk::MenuButton,
) {
    let styles = cluster();
    let style_items: Vec<(String, String, Option<&str>)> = BLOCK_STYLES
        .iter()
        .map(|(tag, label)| {
            let class = match *tag {
                "h1" => Some("title-2"),
                "h2" => Some("title-3"),
                "h3" => Some("heading"),
                "pre" => Some("monospace"),
                _ => None,
            };
            (i18n(label), format!("window.__hylkiBlock('{tag}')"), class)
        })
        .collect();
    let style_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
    style_box.add_css_class("context-menu-list");
    let (style, style_pop) = bar_menu(webview, i18n_noop("Paragraph style"), &style_box);
    style_box.append(&menu_items(webview, &style_pop.downgrade(), &style_items));
    style.set_label(&i18n(BLOCK_STYLES[0].1));
    style.set_always_show_arrow(true);
    let font_items: Vec<(String, String, Option<&str>)> = FONTS
        .iter()
        .map(|(_, family, label)| {
            (
                i18n(label),
                format!("window.__hylkiStyled('fontName',{})", js_str(family)),
                None,
            )
        })
        .collect();
    let font_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
    font_box.add_css_class("context-menu-list");
    let (font, font_pop) = bar_menu(webview, i18n_noop("Font"), &font_box);
    let font_list = menu_items(webview, &font_pop.downgrade(), &font_items);
    let mut item = font_list.first_child();
    for (_, family, _) in FONTS {
        let Some(button) = item else { break };
        if let Some(label) = button
            .downcast_ref::<gtk::Button>()
            .and_then(|b| b.child())
            .and_downcast::<gtk::Label>()
        {
            preview_font(&label, family);
        }
        item = button.next_sibling();
    }
    font_box.append(&font_list);
    // Any installed font. The recipient may not have it, so the family's
    // generic kind follows it.
    let other_font = gtk::Button::new();
    let other_label = gtk::Label::new(Some(&i18n("Other Font…")));
    other_label.set_xalign(0.0);
    other_font.set_child(Some(&other_label));
    other_font.add_css_class("flat");
    other_font.add_css_class("context-menu-item");
    {
        let (weak, pop) = (webview.downgrade(), font_pop.downgrade());
        other_font.connect_clicked(move |b| {
            if let Some(p) = pop.upgrade() {
                p.popdown();
            }
            let parent = b.root().and_downcast::<gtk::Window>();
            let weak = weak.clone();
            choose_family(parent.as_ref(), move |family| match family {
                Some(family) => {
                    let font = crate::config::RecentFont {
                        name: family.name().to_string(),
                        mono: family.is_monospace(),
                    };
                    run_restored(&weak, &font_js(&font));
                    crate::config::note_recent_font(font);
                }
                None => run_restored(&weak, ""),
            });
        });
    }
    font_box.append(&other_font);
    // The fonts picked there last (#358), at the foot of the menu, read
    // afresh each time it opens so that every composer offers the same.
    let recent_fonts = gtk::Box::new(gtk::Orientation::Vertical, 0);
    {
        let (weak, pop, recent) = (
            webview.downgrade(),
            font_pop.downgrade(),
            recent_fonts.clone(),
        );
        font.set_create_popup_func(move |btn| {
            fill_recent_fonts(&recent, &weak, &pop);
            fit_menu(btn);
        });
    }
    font_box.append(&recent_fonts);
    let prefs = crate::config::load_privacy();
    font.set_label(&font_label(first_family(&prefs.compose_font)));
    font.set_always_show_arrow(true);
    let size_items: Vec<(String, String, Option<&str>)> =
        std::iter::once((i18n("Default"), "window.__hylkiSize(0)".to_string(), None))
            .chain(
                SIZES
                    .iter()
                    .map(|pt| (pt.to_string(), format!("window.__hylkiSize({pt})"), None)),
            )
            .collect();
    let size_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
    size_box.add_css_class("context-menu-list");
    let (size, size_pop) = bar_menu(webview, i18n_noop("Font size"), &size_box);
    size_box.append(&menu_items(webview, &size_pop.downgrade(), &size_items));
    // The size new messages start in (the editor's own 14px by default),
    // until the caret reports.
    size.set_label(&match prefs.compose_font_size {
        0 => "11".to_string(),
        pt => pt.to_string(),
    });
    size.set_always_show_arrow(true);
    styles.append(&style);
    styles.append(&font);
    styles.append(&size);
    (styles, style, font, size)
}

/// Plain command buttons into `cluster`: (icon, tooltip, execCommand
/// snippet).
fn simple_buttons(
    webview: &webkit6::WebView,
    cluster: &adw::WrapBox,
    commands: &[(&str, &str, &str)],
) {
    for (icon, tip, cmd) in commands {
        let btn = bar_button(icon, tip);
        // Weak: the format-state handler the view's content manager
        // owns reaches these buttons, so a strong view here would be a
        // cycle.
        let weak = webview.downgrade();
        let cmd = cmd.to_string();
        btn.connect_clicked(move |_| {
            if let Some(wv) = weak.upgrade() {
                exec(&wv, &cmd);
            }
        });
        cluster.append(&btn);
    }
}

/// The text color and highlight menus.
fn color_menus(webview: &webkit6::WebView) -> adw::WrapBox {
    let colors = cluster();
    let text_chosen = std::rc::Rc::new(std::cell::RefCell::new(None));
    colors.append(&color_menu(
        webview,
        i18n_noop("Text color"),
        i18n_noop("Automatic"),
        "foreColor",
        "inherit",
        color_glyph(false, text_chosen.clone()),
        text_chosen,
    ));
    let hl_chosen = std::rc::Rc::new(std::cell::RefCell::new(None));
    colors.append(&color_menu(
        webview,
        i18n_noop("Highlight"),
        i18n_noop("No highlight"),
        "hiliteColor",
        "transparent",
        color_glyph(true, hl_chosen.clone()),
        hl_chosen,
    ));
    colors
}

/// A block command as a toggle, recorded in `toggles` under the key the
/// document reports it by.
fn block_toggle(
    webview: &webkit6::WebView,
    icon: &str,
    tip: &str,
    cmd: &str,
    key: char,
    toggles: &mut Vec<(char, gtk::ToggleButton)>,
) -> gtk::ToggleButton {
    let t = gtk::ToggleButton::new();
    t.set_icon_name(icon);
    t.set_tooltip_text(Some(i18n(tip).as_str()));
    t.set_can_focus(false);
    t.add_css_class("flat");
    let weak = webview.downgrade();
    let cmd = cmd.to_string();
    t.connect_clicked(move |_| {
        if let Some(wv) = weak.upgrade() {
            exec(&wv, &cmd);
        }
    });
    toggles.push((key, t.clone()));
    t
}

/// Put `tools` behind the chevron at the end of `group`, after `tail`, open
/// to begin with when `expanded`. Returns the chevron.
fn chevron_tools(
    group: &ToolFlow,
    tail: &adw::WrapBox,
    tools: [&adw::WrapBox; 4],
    expanded: bool,
) -> gtk::ToggleButton {
    // The rest (#358) behind a chevron, after everything above so showing
    // them moves nothing: paragraph style and font, colors, indents, emoji
    // and pictures. They slide out from the chevron, which rides along to
    // their end, and slide back into it. Settings → Composing → Formatting
    // toolbar says how a new message starts; the chevron changes it for
    // this one.
    // A wrapping box, so in a narrow composer the groups take further rows
    // one by one rather than holding the toolbar, and the window, wider
    // than the pane.
    let extras = adw::WrapBox::new();
    extras.set_child_spacing(10);
    extras.set_line_spacing(2);
    for e in tools {
        extras.append(e);
    }
    let reveal = gtk::Revealer::new();
    reveal.set_transition_type(gtk::RevealerTransitionType::SlideRight);
    reveal.set_transition_duration(250);
    reveal.set_child(Some(&extras));
    // Where the tools do not fit beside the rest, they take the next row.
    // A growing revealer would start on this row and jump to the next
    // partway, so this spacer fills the rest of the row for the length of
    // the slide and the tools slide out on the next row from the start.
    let breaker = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    breaker.set_visible(false);
    // A folded revealer is still a child of the bar, and the bar's spacing
    // would show either side of it: it leaves the row once it has closed.
    {
        let breaker = breaker.clone();
        reveal.connect_child_revealed_notify(move |r| {
            breaker.set_visible(false);
            if !r.reveals_child() {
                r.set_visible(false);
            }
        });
    }
    let more = gtk::ToggleButton::new();
    more.set_can_focus(false);
    more.add_css_class("flat");
    let show_more = {
        let (reveal, breaker, group, tail, extras) = (
            reveal.clone(),
            breaker.clone(),
            group.clone(),
            tail.clone(),
            extras.clone(),
        );
        move |b: &gtk::ToggleButton| {
            let on = b.is_active();
            b.set_icon_name(if on {
                "pan-start-symbolic"
            } else {
                "pan-end-symbolic"
            });
            b.set_tooltip_text(Some(
                if on {
                    i18n("Fewer formatting tools")
                } else {
                    i18n("More formatting tools")
                }
                .as_str(),
            ));
            // The room left on the row after Clear formatting, against what
            // the tools and the chevron need.
            let gap = 10.0;
            let spare = group.room_after(tail.upcast_ref()).unwrap_or(0) as f32;
            let need =
                extras.measure(gtk::Orientation::Horizontal, -1).1 as f32 + gap + b.width() as f32;
            if reveal.transition_duration() > 0 && group.width() > 0 && need > spare {
                breaker.set_size_request((spare - 1.0).max(0.0) as i32, -1);
                breaker.set_visible(true);
            }
            if on {
                reveal.set_visible(true);
            }
            reveal.set_reveal_child(on);
        }
    };
    // How a new message starts: already open, with no slide.
    reveal.set_transition_duration(0);
    more.set_active(expanded);
    show_more(&more);
    reveal.set_visible(expanded);
    reveal.set_transition_duration(250);
    more.connect_toggled(show_more);
    // The chevron rides with the tools it shows, wrapping under them only
    // when the row truly has no room. A wrapping box of its own keeps the
    // toolbar's narrowest width that of the tools alone: a plain box would
    // add the chevron to it and push a narrow composer past its pane.
    let ride = adw::WrapBox::new();
    ride.set_child_spacing(10);
    ride.set_line_spacing(2);
    ride.append(&reveal);
    ride.append(&more);
    group.append(&breaker);
    group.append(&ride);
    more
}

/// HYLKI_SHOWCASE_TOOLBAR_TOGGLE=<ms>: press the chevron that long after
/// the bar is built, so frames of the slide can be captured.
fn showcase_toolbar_toggle(more: &gtk::ToggleButton) {
    if let Some(ms) = std::env::var("HYLKI_SHOWCASE_TOOLBAR_TOGGLE")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
    {
        let more = more.downgrade();
        gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(ms), move || {
            if let Some(m) = more.upgrade() {
                tracing::info!("showcase: chevron pressed");
                m.set_active(!m.is_active());
                // HYLKI_SHOWCASE_TOOLBAR_SHOTS=<png prefix>:<ms>,<ms>…
                // captures the window that long after the press.
                if let Some((prefix, times)) = std::env::var("HYLKI_SHOWCASE_TOOLBAR_SHOTS")
                    .ok()
                    .and_then(|v| {
                        v.rsplit_once(':')
                            .map(|(p, t)| (p.to_string(), t.to_string()))
                    })
                {
                    for t in times.split(',').filter_map(|t| t.parse::<u64>().ok()) {
                        let (root, path) = (m.root(), format!("{prefix}{t}.png"));
                        gtk::glib::timeout_add_local_once(
                            std::time::Duration::from_millis(t),
                            move || {
                                if let Some(r) = root {
                                    crate::ui::showcase::showcase_capture(
                                        r.upcast_ref::<gtk::Widget>(),
                                        &path,
                                    );
                                }
                            },
                        );
                    }
                }
            }
        });
    }
}

/// Choose pictures to put in the message at the caret (#358), the way a
/// drop or a paste puts them there: a file too big to carry in the body,
/// or not a picture, joins the attachments instead.
fn pick_pictures(webview: &webkit6::WebView, anchor: &gtk::Button, attach_cb: &AttachCallback) {
    exec(webview, "window.__hylkiSave&&window.__hylkiSave()");
    let filter = gtk::FileFilter::new();
    filter.set_name(Some(&i18n("Pictures")));
    filter.add_mime_type("image/*");
    let filters = gtk::gio::ListStore::new::<gtk::FileFilter>();
    filters.append(&filter);
    let dialog = gtk::FileDialog::builder()
        .title(i18n("Insert Picture"))
        .filters(&filters)
        .build();
    let parent = anchor.root().and_downcast::<gtk::Window>();
    let (weak, cb) = (webview.downgrade(), attach_cb.clone());
    dialog.open_multiple(parent.as_ref(), gtk::gio::Cancellable::NONE, move |res| {
        let Some(wv) = weak.upgrade() else { return };
        let Ok(list) = res else { return };
        let files: Vec<gtk::gio::File> = (0..list.n_items())
            .filter_map(|i| list.item(i).and_downcast::<gtk::gio::File>())
            .collect();
        exec(&wv, "window.__hylkiRestore&&window.__hylkiRestore()");
        deliver_files(&wv, &files, None, &cb, true);
        wv.grab_focus();
    });
}

/// Prompt for a URL and turn the current selection into a link.
#[allow(deprecated)] // adw::MessageDialog: AlertDialog would change how it looks
fn prompt_link(webview: &webkit6::WebView, anchor: &gtk::Button) {
    let parent = anchor.root().and_downcast::<gtk::Window>();
    let dialog = crate::ui::dialog::message_dialog(
        parent.as_ref(),
        Some(i18n("Insert Link").as_str()),
        None,
    );
    dialog.add_response("cancel", &i18n("Cancel"));
    dialog.add_response("ok", &i18n("Insert"));
    dialog.set_default_response(Some("ok"));
    dialog.set_response_appearance("ok", adw::ResponseAppearance::Suggested);
    let entry = gtk::Entry::new();
    entry.set_input_purpose(gtk::InputPurpose::Url);
    entry.set_placeholder_text(Some("https://example.com"));
    entry.set_activates_default(true);
    dialog.set_extra_child(Some(&entry));
    let wv = webview.clone();
    dialog.connect_response(None, move |_, resp| {
        if resp == "ok" {
            let url = entry.text().to_string();
            let url = url.trim();
            if !url.is_empty() {
                exec(
                    &wv,
                    &format!(
                        "document.execCommand('createLink',false,'{}')",
                        js_escape(url)
                    ),
                );
            }
        }
    });
    dialog.present();
}
