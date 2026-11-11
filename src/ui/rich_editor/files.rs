//! Pictures and files coming into the editor: pasted, dropped, picked or lifted
//! out of the document, and handed to the composer when they are not inline
//! images.

use super::AttachCallback;

use super::{exec, js_escape};
use adw::prelude::*;
use webkit6::prelude::WebViewExt;

/// "Send as Attachment Instead": pull the right-clicked image out of the
/// document, decode its data: URI to a temp file, and hand the path to the
/// host's callback (which adds it to the attachment list). The node is
/// removed in the same script that reads it, so the demotion is atomic even
/// if the same picture was pasted twice.
pub(super) fn detach_ctx_image(webview: &webkit6::WebView, cb: AttachCallback) {
    webview.evaluate_javascript(
        "(function(){var n=window.__hylkiCtxImg;\
          if(!n){var sel=getSelection();\
            if(sel.rangeCount===1){var r=sel.getRangeAt(0);\
              if(r.startContainer===r.endContainer&&r.endOffset-r.startOffset===1){\
                var c=r.startContainer.childNodes[r.startOffset];\
                if(c&&c.tagName==='IMG')n=c;}}}\
          if(!n||n.tagName!=='IMG')return 'E:none';\
          var s=n.src;if(s.indexOf('data:image/')!==0)return 'E:src:'+s.slice(0,40);\
          n.remove();window.__hylkiCtxImg=null;return s;})()",
        None,
        None,
        gtk::gio::Cancellable::NONE,
        move |res| {
            let v = match res {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!("img-attach: script failed: {e}");
                    return;
                }
            };
            let src = v.to_str();
            if let Some(err) = src.strip_prefix("E:") {
                tracing::warn!("img-attach: script declined: {err}");
                return;
            }
            let Some(path) = data_uri_to_temp_file(&src) else {
                tracing::warn!("img-attach: not a liftable data: image");
                return;
            };
            match cb.borrow().as_ref() {
                Some(f) => f(path),
                None => tracing::warn!("img-attach: no attach callback set on this editor"),
            }
        },
    );
}

/// Write a `data:image/...;base64,...` URI's bytes to a temp file the send
/// path can read like any chosen attachment. The name it gets is the name
/// that goes out on the wire.
fn data_uri_to_temp_file(uri: &str) -> Option<std::path::PathBuf> {
    let rest = uri.strip_prefix("data:")?;
    let (meta, payload) = rest.split_once(',')?;
    let mime = meta.strip_suffix(";base64")?;
    if !mime.starts_with("image/") {
        return None;
    }
    let data = crate::encoding::base64_decode(payload)?;
    let ext = match mime {
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/svg+xml" => "svg",
        _ => "png",
    };
    let dir = std::env::temp_dir().join("vireo-inline-images");
    std::fs::create_dir_all(&dir).ok()?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let path = dir.join(format!("image-{stamp}.{ext}"));
    std::fs::write(&path, &data).ok()?;
    Some(path)
}

/// What a paste keeps of what the clipboard holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PasteAs {
    /// The formatting, as WebKit pastes it.
    Formatted,
    /// The text and nothing else.
    Text,
    /// The text, but a spreadsheet's cells as their table (#623): Ctrl+V
    /// with Paste as plain text set, where the tab-separated text of the
    /// cells is no use in a message.
    TextOrCells,
}

impl PasteAs {
    /// Ctrl+V's, from the "Paste as plain text" preference.
    pub(crate) fn preferred(paste_plain: bool) -> Self {
        if paste_plain {
            Self::TextOrCells
        } else {
            Self::Formatted
        }
    }

    /// The value of `__hylkiPasteOnce` the paste handler reads.
    fn js(self) -> &'static str {
        match self {
            Self::Formatted => "true",
            Self::Text => "false",
            Self::TextOrCells => "'cells'",
        }
    }
}

/// One paste in an explicit mode: arm the document's one-shot flag, then run
/// the editing command once the flag is in place (the callback orders the two,
/// since both travel to the web process asynchronously). The DOM paste handler
/// consumes the flag — see [`PASTE_SCRIPT`].
///
/// A file copied in a file manager is taken first, for the same reason the
/// drop target exists: the clipboard offers `GdkFileList`, but by the time
/// WebKit has turned it into a paste the document sees an empty
/// `clipboardData` and the bare path arrives as text. The formats are checked
/// synchronously so an ordinary text paste is not delayed by a read that was
/// always going to fail.
pub(super) fn paste_into(
    webview: &webkit6::WebView,
    mode: PasteAs,
    cb: &AttachCallback,
    plain: bool,
) {
    if webview
        .clipboard()
        .formats()
        .contains_type(gtk::gdk::FileList::static_type())
    {
        let v = webview.clone();
        let cb = cb.clone();
        webview.clipboard().read_value_async(
            gtk::gdk::FileList::static_type(),
            gtk::glib::Priority::DEFAULT,
            gtk::gio::Cancellable::NONE,
            move |res| {
                let taken = res
                    .ok()
                    .and_then(|val| val.get::<gtk::gdk::FileList>().ok())
                    .is_some_and(|list| deliver_files(&v, &list.files(), None, &cb, !plain));
                // Nothing usable on the clipboard after all: the ordinary
                // paste still owes the user whatever else is on it.
                if !taken {
                    paste_via_webkit(&v, mode);
                }
            },
        );
        return;
    }
    paste_via_webkit(webview, mode);
}

/// The stock paste: arm the one-shot mode flag, then let WebKit paste.
fn paste_via_webkit(webview: &webkit6::WebView, mode: PasteAs) {
    let v = webview.clone();
    webview.evaluate_javascript(
        &format!("window.__hylkiPasteOnce={};", mode.js()),
        None,
        None,
        gtk::gio::Cancellable::NONE,
        move |_| v.execute_editing_command("Paste"),
    );
}

/// Images inline, everything else attached. `at` is the drop point in widget
/// coordinates (which are the document's viewport coordinates), placing the
/// caret where the picture was let go; a paste keeps the caret it has.
/// Returns whether any file was taken, so a caller can fall back. With
/// `inline` off (a message written as plain text) every file is attached.
///
/// Reading and encoding happen here rather than in the document because the
/// document is never given the file in the first place — see [`paste_into`].
pub(super) fn deliver_files(
    webview: &webkit6::WebView,
    files: &[gtk::gio::File],
    at: Option<(f64, f64)>,
    cb: &AttachCallback,
    inline: bool,
) -> bool {
    if !inline {
        return attach_all(files, cb);
    }
    let mut took = false;
    let mut point = at;
    for file in files {
        let Some(path) = file.path() else { continue };
        match read_image_for_insert(file, &path) {
            Some((b64, mime)) => {
                let (x, y) = point
                    .take()
                    .map_or(("null".into(), "null".into()), |(x, y)| {
                        (format!("{x}"), format!("{y}"))
                    });
                // The base64 goes in unescaped on purpose: its alphabet has
                // nothing a JS string literal cares about, and running the
                // escape over tens of megabytes of it costs real time. Only
                // the MIME type comes from outside.
                let mime = js_escape(&mime);
                let name = js_escape(&path.file_name().unwrap_or_default().to_string_lossy());
                exec(
                    webview,
                    &format!(
                        "window.__hylkiInsertImageURL(\
                         'data:{mime};base64,{b64}','{mime}','{name}',{x},{y})"
                    ),
                );
                took = true;
            }
            // Not a picture, or too big to carry in the body: the composer's
            // attachment list is where it belongs. Inline images are lifted
            // into `cid:` parts at send time either way, so an attachment is
            // the same journey with a different disposition.
            None => match cb.borrow().as_ref() {
                Some(f) => {
                    f(path);
                    took = true;
                }
                None => tracing::warn!("editor drop: no attach callback set on this editor"),
            },
        }
    }
    took
}

/// An image small enough to inline, as base64 of its bytes plus its MIME type.
/// Anything else — a document, an unreadable file, or a picture so large that
/// base64 of it would be a burden to ferry into the web process — returns
/// `None` and becomes an attachment instead. The cap is deliberately generous:
/// the document downscales to 1600px on insert, so the size that matters to
/// the recipient is decided there, not here.
fn read_image_for_insert(
    file: &gtk::gio::File,
    path: &std::path::Path,
) -> Option<(String, String)> {
    let mime = inline_mime(file)?;
    let data = std::fs::read(path).ok()?;
    Some((crate::encoding::base64_encode(&data), mime))
}

/// The MIME type of a picture that can go in the text, or `None`.
fn inline_mime(file: &gtk::gio::File) -> Option<String> {
    const MAX_INLINE_BYTES: u64 = 32 * 1024 * 1024;
    let info = file
        .query_info(
            "standard::content-type,standard::size",
            gtk::gio::FileQueryInfoFlags::NONE,
            gtk::gio::Cancellable::NONE,
        )
        .ok()?;
    let mime = info.content_type()?.to_string();
    // Deliberately a list, not `image/*`: a type the engine cannot decode
    // would insert a broken <img> carrying the whole file as base64 — a RAW
    // photo or an HEIC off a phone is tens of megabytes of nothing. A
    // renderable format missing from this list merely becomes an attachment,
    // which is the harmless way to be wrong.
    const INLINE_MIMES: &[&str] = &[
        "image/png",
        "image/jpeg",
        "image/gif",
        "image/webp",
        "image/bmp",
        "image/svg+xml",
        "image/avif",
    ];
    (INLINE_MIMES.contains(&mime.as_str()) && info.size() as u64 <= MAX_INLINE_BYTES)
        .then_some(mime)
}

/// Whether the file is a picture the editor would place in the text rather
/// than attach: what the composer's drop surfaces offer "Insert in Text" for.
pub(crate) fn is_inline_image(path: &std::path::Path) -> bool {
    inline_mime(&gtk::gio::File::for_path(path)).is_some()
}

/// Every dropped file straight onto the attachment list.
pub(super) fn attach_all(files: &[gtk::gio::File], cb: &AttachCallback) -> bool {
    let mut took = false;
    for path in files.iter().filter_map(|f| f.path()) {
        match cb.borrow().as_ref() {
            Some(f) => {
                f(path);
                took = true;
            }
            None => tracing::warn!("editor drop: no attach callback set on this editor"),
        }
    }
    took
}
