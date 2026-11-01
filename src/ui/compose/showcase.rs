//! Development hooks the capture scripts drive.

use super::messages::ComposeInput;
use super::Compose;
use crate::ui::rich_editor::RichEditor;
use adw::prelude::*;
use relm4::prelude::*;

/// Development hooks for the capture scripts (`HYLKI_SHOWCASE_*`, most of
/// them only with `HYLKI_DEMO`).
pub(super) fn showcase_hooks(
    root: &adw::BreakpointBin,
    editor: &RichEditor,
    sender: &ComponentSender<Compose>,
) {
    // HYLKI_SHOWCASE_CLOUD_DIALOG opens the cloud upload dialog on a
    // stand-in file two seconds after the composer is up (demo only),
    // for a capture of this upload's terms.
    if std::env::var_os("HYLKI_SHOWCASE_CLOUD_DIALOG").is_some()
        && std::env::var_os("HYLKI_DEMO").is_some()
    {
        let s = sender.clone();
        gtk::glib::timeout_add_seconds_local_once(2, move || {
            s.input_sender()
                .emit(ComposeInput::CloudPicked(vec![std::path::PathBuf::from(
                    "/tmp/Q3 report.pdf",
                )]));
        });
    }

    // HYLKI_SHOWCASE_MERGE opens Mail Merge five seconds after the
    // composer opens (demo only); see `ui::merge_dialog`.
    if std::env::var_os("HYLKI_SHOWCASE_MERGE").is_some()
        && std::env::var_os("HYLKI_DEMO").is_some()
    {
        let s = sender.input_sender().clone();
        gtk::glib::timeout_add_seconds_local_once(5, move || {
            let _ = s.send(ComposeInput::MailMerge);
        });
    }

    // HYLKI_SHOWCASE_EDITOR_JS=<file> runs that script in the body three
    // seconds after the composer opens, and logs the body's HTML a
    // second later (demo only), to drive the format bar's commands
    // without a pointer (#358).
    if let (Some(path), Some(_)) = (
        std::env::var_os("HYLKI_SHOWCASE_EDITOR_JS"),
        std::env::var_os("HYLKI_DEMO"),
    ) {
        if let Ok(js) = std::fs::read_to_string(&path) {
            let ed = editor.clone();
            gtk::glib::timeout_add_seconds_local_once(3, move || {
                ed.run_js(&js);
                gtk::glib::timeout_add_seconds_local_once(1, move || {
                    ed.eval("document.body.innerHTML", |html| {
                        tracing::info!("showcase editor body: {html}");
                    });
                });
            });
        }
    }

    // HYLKI_SHOWCASE_ATTACH=<file>[:<file>…] attaches those files a
    // second after the composer opens (demo only), for a capture of the
    // chips (#299); HYLKI_SHOWCASE_ATTACH_SHOT=<png>[:<seconds>]
    // captures the composer that long after it opens.
    if let (Some(v), Some(_)) = (
        std::env::var_os("HYLKI_SHOWCASE_ATTACH"),
        std::env::var_os("HYLKI_DEMO"),
    ) {
        let paths: Vec<_> = std::env::split_paths(&v).collect();
        // A composer made ahead of time and dropped unused is gone by then.
        let s = sender.input_sender().clone();
        gtk::glib::timeout_add_seconds_local_once(1, move || {
            let _ = s.send(ComposeInput::AddAttachments(paths));
        });
        if let Ok(v) = std::env::var("HYLKI_SHOWCASE_ATTACH_SHOT") {
            // <png>[:<seconds>], three seconds unless given.
            let given = v
                .rsplit_once(':')
                .and_then(|(p, n)| Some((p.to_string(), n.parse::<u32>().ok()?)));
            let (shot, at) = given.unwrap_or((v, 3));
            let host = root.clone().upcast::<gtk::Widget>();
            gtk::glib::timeout_add_seconds_local_once(at, move || {
                if host.is_mapped() {
                    crate::ui::showcase::showcase_capture(&host, &shot);
                }
            });
        }
    }
}
