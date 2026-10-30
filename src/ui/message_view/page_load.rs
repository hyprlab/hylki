//! Loading a document without a flash: the pending load.

use super::MessageView;

use adw::prelude::*;
use relm4::prelude::*;
use webkit6::prelude::WebViewExt;

/// A document waiting for the WebView to be ready for it (`load_after_paint`).
#[derive(Default)]
pub(super) struct PendingLoad {
    /// The newest document and its base URI. Only the newest is kept.
    doc: Option<(String, String)>,
    /// Bumped per wait, so a superseded wait's fallback cannot load the next
    /// document before that one's own wait is over.
    wait: u64,
    /// The WebView's size when the last document was loaded.
    size: (i32, i32),
}

impl MessageView {
    /// Load a document without the WebView flashing black on the way.
    ///
    /// Opening a message often changes the WebView's size in the same turn:
    /// the attachment drawer comes or goes, a banner slides in.
    /// With WebKit's GPU (DMA-BUF) renderer, a page that commits before the
    /// web process has drawn at the new size has nothing to show at it, and
    /// the pane is black for a frame or two: the flash between messages
    /// that only real hardware showed. So the load waits for the frame that
    /// lays out the new size, and, when the size did change, for the old
    /// page to be drawn at it (two animation frames in the page), then
    /// replaces a page that is fully drawn. An unchanged size costs a frame.
    pub(super) fn load_after_paint(&self, html: String, uri: String) {
        let state = self.pending_load.clone();
        let wait = {
            let mut s = state.borrow_mut();
            // A wait already under way loads the newest document.
            if s.doc.replace((html, uri)).is_some() {
                return;
            }
            s.wait = s.wait.wrapping_add(1);
            s.wait
        };
        let load = std::rc::Rc::new({
            let view = self.webview.clone();
            move || {
                let mut s = state.borrow_mut();
                // A later wait has taken over: this one's document is loaded.
                if s.wait != wait {
                    return;
                }
                if let Some((html, uri)) = s.doc.take() {
                    s.size = (view.width(), view.height());
                    drop(s);
                    crate::desktop::web_fonts::load_html(&view, &html, Some(&uri));
                }
            }
        });
        let clock = self
            .webview
            .frame_clock()
            .filter(|_| self.webview.is_mapped());
        let Some(clock) = clock else {
            // Nothing on screen, so no frame to wait for or to flash.
            load();
            return;
        };
        let handler = std::rc::Rc::new(std::cell::Cell::new(None));
        let id = {
            let handler = handler.clone();
            let load = load.clone();
            let view = self.webview.clone();
            let size = self.pending_load.clone();
            clock.connect_after_paint(move |clock| {
                if let Some(id) = handler.take() {
                    clock.disconnect(id);
                }
                if size.borrow().size == (view.width(), view.height()) {
                    load();
                    return;
                }
                let load = load.clone();
                view.call_async_javascript_function(
                    "await new Promise(r => requestAnimationFrame(() => requestAnimationFrame(r)));",
                    None,
                    None,
                    None,
                    None::<&gtk::gio::Cancellable>,
                    move |_| load(),
                );
            })
        };
        handler.set(Some(id));
        clock.request_phase(gtk::gdk::FrameClockPhase::AFTER_PAINT);
        // A window that stops drawing (hidden, minimised) stops its clock and
        // the page's animation frames; the message must load all the same.
        gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(150), move || {
            if let Some(id) = handler.take() {
                clock.disconnect(id);
            }
            load();
        });
    }
}

impl MessageView {
    /// While a document loads, keep the keyboard on the loading page rather
    /// than in the web view, and give it back once the document is shown.
    pub(super) fn hold_focus_while_loading(&self) {
        let Some(page) = &self.loading_page else {
            return;
        };
        let web: &gtk::Widget = self.webview.upcast_ref();
        let focus = web.root().and_then(|r| r.focus());
        if self.body_page() == "loading" {
            if focus.is_some_and(|f| f == *web || f.is_ancestor(web)) {
                page.grab_focus();
                self.focus_held.set(true);
            }
        } else if self.focus_held.replace(false)
            && focus.is_some_and(|f| f == *page.upcast_ref::<gtk::Widget>())
        {
            self.webview.grab_focus();
        }
    }

    /// Which body-stack page to show: spinner while fetching, themed cover while
    /// the WebView loads, then the rendered message(s).
    pub(super) fn body_page(&self) -> &'static str {
        // A document that has been handed over but hasn't painted yet is still
        // loading as far as the reader is concerned. Showing the previous
        // message in the meantime, or a bare cover between two spinners, is the
        // flash this avoids. Under the pane's still nothing shows anyway, and
        // the page loads in view instead: hidden behind the spinner it is
        // neither laid out nor drawn, and would only be once revealed.
        if self.loading || (!self.webview_ready && !self.swap.held()) {
            "loading"
        } else {
            "body"
        }
    }
}
