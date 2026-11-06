//! The wrapper document's script policy, remote detection and links.

use super::*;
use webkit6::prelude::WebViewExt;

#[test]
fn the_wrapper_only_runs_its_own_script() {
    let mut a = msg_for_print();
    a.from_name = "Ada".into();
    let mut b = msg_for_print();
    b.id = 2;
    let doc = MessageView::conversation_document(
        &[a, b],
        &Default::default(),
        &Default::default(),
        &Default::default(),
        &[],
        "#3584e4",
        true,
        false,
        false,
        false,
        &crate::config::ReaderStyle::NONE,
        &Default::default(),
        false,
        100,
    );

    // A nonce'd CSP, so an injected `<script>` or `onerror=` is refused by
    // the engine even if the escaping above ever regresses.
    // The wrapper's own policy — not one of the frames', which are embedded
    // in this same string as escaped `srcdoc` payloads.
    let policy = doc
        .split("<meta http-equiv=\"Content-Security-Policy\" content=\"")
        .nth(1)
        .and_then(|r| r.split('"').next())
        .expect("the wrapper declares a CSP");
    let nonce = policy
        .split("script-src 'nonce-")
        .nth(1)
        .and_then(|r| r.split('\'').next())
        .expect("wrapper CSP declares a nonce");
    assert!(nonce.len() >= 16, "nonce is {} chars", nonce.len());
    assert!(
        doc.contains(&format!("<script nonce=\"{nonce}\">")),
        "{doc}"
    );
    // The sizing script is the *only* script, and it carries the nonce.
    assert_eq!(doc.matches("<script").count(), 1, "{doc}");
    // No `default-src` here: a wrapper policy is inherited by the srcdoc
    // frames, and one would re-block content the user has allowed. Each
    // frame brings its own `default-src 'none'`.
    assert!(!policy.contains("default-src"), "{policy}");
    // Two renders never share a nonce.
    let again = MessageView::conversation_document(
        &[msg_for_print(), msg_for_print()],
        &Default::default(),
        &Default::default(),
        &Default::default(),
        &[],
        "#3584e4",
        true,
        false,
        false,
        false,
        &crate::config::ReaderStyle::NONE,
        &Default::default(),
        false,
        100,
    );
    assert!(!again.contains(nonce), "nonce was reused across renders");
}

#[test]
fn the_remote_detector_sees_past_the_obvious_spellings() {
    // Every row here loaded a tracking pixel with no banner shown, because
    // the old detector matched fixed substrings like `src="http`.
    for html in [
        // Protocol-relative: resolves against https://hylki.localhost.
        r#"<img src="//tracker.example/p.gif">"#,
        // HTML permits whitespace around `=`.
        r#"<img src = "http://tracker.example/p.gif">"#,
        // Neither `<svg><image href>` nor `poster` was in the attribute list.
        r#"<svg><image href="http://tracker.example/p.gif"/></svg>"#,
        r#"<video poster="http://tracker.example/p.gif">"#,
        // `@import` was only matched in its `url(http…)` spelling.
        r#"<style>@import url(//tracker.example/x.css)</style>"#,
        r#"<style>@import "https://tracker.example/x.css"</style>"#,
        // Case: the detector lowercased, the stripper did not.
        r#"<IMG SRC="HTTP://tracker.example/p.gif">"#,
        // Unquoted, and quoted with single quotes.
        r#"<img src=http://tracker.example/p.gif>"#,
        r#"<img src='//tracker.example/p.gif'>"#,
        r#"<div style="background:url('//tracker.example/p.gif')">"#,
        r#"<img srcset="//tracker.example/p.gif 1x, /local.gif 2x">"#,
        r#"<link rel="stylesheet" href="//tracker.example/x.css">"#,
    ] {
        assert!(has_remote_resources(html), "not detected: {html}");
        let stripped = strip_remote(html);
        assert!(
            !stripped.contains("tracker.example"),
            "not stripped: {html} -> {stripped}"
        );
    }
}

#[test]
fn a_stylesheet_does_not_hide_what_follows_it() {
    // The walk has to resume after `</style>`, not treat the closing tag as
    // opening another stylesheet and take the rest of the document for CSS.
    let html = "<html><head><style>.a{background:url(https://cdn.example/b.png)}</style>\
                </head><body><img src=\"https://cdn.example/i.png\"></body></html>";
    let out = strip_remote(html);
    assert!(!out.contains("cdn.example"), "{out}");
    assert_eq!(out.matches("blocked://").count(), 2, "{out}");
}

#[test]
fn the_detector_leaves_self_contained_messages_alone() {
    // A false banner on ordinary mail teaches people to ignore the real one.
    for html in [
        r#"<img src="data:image/png;base64,iVBOR">"#,
        r#"<img src="cid:part1@example.com">"#,
        r#"<p>plain text, no resources at all</p>"#,
        // A link is a destination the user must click, not a fetch.
        r#"<a href="https://example.com/read-more">more</a>"#,
        r#"<div style="color:#333;font-weight:bold">styled</div>"#,
    ] {
        assert!(!has_remote_resources(html), "false positive: {html}");
        assert_eq!(strip_remote(html), html, "needlessly rewritten: {html}");
    }
}

#[test]
fn blocking_follows_the_users_choice_not_the_detector() {
    // The point of the split: even for a body the detector says nothing
    // about, a frame built while remote content is disallowed carries the
    // restrictive policy. A detector miss costs a banner, not the blocking.
    let sneaky = r#"<img data-x="y" src="//tracker.example/p.gif">"#;
    let frame = message_frame(
        sneaky,
        true,
        false,
        (1, 1),
        None,
        &crate::config::ReaderStyle::NONE,
        "#3584e4",
        false,
        100,
    );
    assert!(frame.contains("img-src data: cid:"), "{frame}");
    assert!(!frame.contains("img-src http:"), "{frame}");
    assert!(!frame.contains("tracker.example"), "{frame}");

    // And once the user allows it, the same body renders untouched.
    let allowed = message_frame(
        sneaky,
        false,
        false,
        (1, 1),
        None,
        &crate::config::ReaderStyle::NONE,
        "#3584e4",
        false,
        100,
    );
    assert!(allowed.contains("img-src http: https:"), "{allowed}");
    assert!(allowed.contains("tracker.example"), "{allowed}");
}

#[test]
fn only_web_and_mail_links_reach_the_desktop() {
    // An HTML body keeps its own `href` values, so a message can name any
    // scheme a third-party application has registered.
    for uri in [
        "http://example.com/",
        "https://example.com/",
        "MAILTO:a@b.c",
    ] {
        assert!(is_launchable_uri(uri), "{uri}");
    }
    for uri in [
        "file:///etc/passwd",
        "smb://host/share",
        "nfs://host/export",
        "javascript:alert(1)",
        "data:text/html,<script>x</script>",
        "vscode://file/etc/passwd",
        "https:",
        "no-scheme-at-all",
    ] {
        assert!(!is_launchable_uri(uri), "{uri}");
    }
}

/// Renders the real wrapper document in a real WebView and reports what
/// actually happened in the engine.
///
/// Ignored by default: it needs a display and a WebKit process, which a CI
/// runner has no business requiring. Run it by hand after touching the
/// wrapper's CSP or its sizing script:
///
/// ```text
/// cargo test -- --ignored the_wrapper_in_a_real_engine
/// ```
#[test]
#[ignore = "needs a display and a WebKit process"]
fn the_wrapper_in_a_real_engine_sizes_frames_and_refuses_injected_script() {
    use gtk::prelude::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    gtk::init().expect("a display");

    let mut a = msg_for_print();
    // If this ever executes, the assertions below see it.
    a.from_name = "<script>window.__pwned = 1</script>".into();
    a.body = "<p style=\"height:400px\">first</p>".into();
    let mut b = msg_for_print();
    b.id = 2;
    b.body = "<p style=\"height:300px\">second</p>".into();
    let html = MessageView::conversation_document(
        &[a, b],
        &Default::default(),
        &Default::default(),
        &Default::default(),
        &[],
        "#3584e4",
        true,
        false,
        false,
        false,
        &crate::config::ReaderStyle::NONE,
        &Default::default(),
        false,
        100,
    );

    let view = webkit6::WebView::new();
    let settings = webkit6::Settings::new();
    settings.set_enable_javascript(true);
    view.set_settings(&settings);
    // Off-screen is enough; WebKit still lays out and runs scripts.
    let win = gtk::Window::new();
    win.set_child(Some(&view));
    win.set_default_size(800, 600);
    win.present();
    view.load_html(&html, Some("https://hylki.localhost/message/1"));

    let answer: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let ctx = gtk::glib::MainContext::default();
    // Run the main loop until `done`, yielding when there is nothing to
    // dispatch. Spinning on a non-blocking iteration instead starves the
    // WebKit web process on a busy machine, which made this fail at random.
    let pump = |done: &dyn Fn() -> bool| {
        while !done() && std::time::Instant::now() < deadline {
            if !ctx.iteration(false) {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        }
    };

    // Give the load and the two deferred `s(f)` passes time to run.
    let ready = Rc::new(std::cell::Cell::new(false));
    let r = ready.clone();
    view.connect_load_changed(move |_, ev| {
        if ev == webkit6::LoadEvent::Finished {
            r.set(true);
        }
    });
    pump(&|| ready.get());
    assert!(ready.get(), "the document never finished loading");
    // The sizing script measures on DOMContentLoaded and again on deferred
    // passes, so poll for the result rather than sampling once — sampling
    // made this fail under load, which is a flaky test, not a finding.
    // `ours` — not the frame heights — is what says the nonce'd script ran.
    // Heights come from `scrollHeight`, which is 0 until the window is
    // mapped and laid out, and whether a compositor has got round to that
    // has nothing to do with the CSP. The sizing script's top-level
    // functions become properties of `window` the moment it executes.
    let probe = "JSON.stringify({\
           pwned: typeof window.__pwned !== 'undefined',\
           ours: ['s','init','all'].every(f => typeof window[f] === 'function'),\
           heights: [...document.querySelectorAll('iframe.hylki-frame')]\
                      .map(f => f.style.height),\
           readable: (function(){ try { \
             return document.querySelector('iframe.hylki-frame')\
                      .contentDocument.body.innerText.trim(); \
           } catch (e) { return 'blocked'; } })(),\
           from: document.querySelector('.hylki-from').textContent\
         })";
    let mut json = String::new();
    while std::time::Instant::now() < deadline {
        answer.replace(None);
        let out = answer.clone();
        view.evaluate_javascript(probe, None, None, gtk::gio::Cancellable::NONE, move |res| {
            *out.borrow_mut() = Some(match res {
                Ok(v) => v.to_str().to_string(),
                Err(e) => format!("ERROR {e}"),
            });
        });
        pump(&|| answer.borrow().is_some());
        json = answer.borrow().clone().expect("the engine answered");
        if json.contains("\"ours\":true") {
            break;
        }
        let pause = std::time::Instant::now() + std::time::Duration::from_millis(250);
        pump(&|| std::time::Instant::now() >= pause);
    }

    // The injected script is inert: it survives as text in the header and
    // never runs.
    assert!(
        json.contains("\"pwned\":false"),
        "injected script ran: {json}"
    );
    assert!(
        json.contains("<script>window.__pwned = 1</script>"),
        "the display name should read back as text: {json}"
    );
    // And our own nonce'd script did run.
    assert!(
        json.contains("\"ours\":true"),
        "the sizing script did not run, so the CSP nonce is not working: {json}"
    );
    // The frames are still same-origin, which is what the sizing relies on.
    assert!(json.contains("first"), "frame content unreadable: {json}");
}

/// A picture lifted out of a message's body (see
/// [`crate::services::inline_images`]) in a real reader view: the frame shows
/// it, and a page can fetch it, as the composer does with a pasted one.
///
/// ```text
/// cargo test -- --ignored a_lifted_picture_in_a_real_engine
/// ```
///
/// GTK starts on one thread per process, and every test gets a thread of its
/// own: run it alone, apart from the other real-engine test.
#[test]
#[ignore = "needs a display and a WebKit process"]
fn a_lifted_picture_in_a_real_engine_shows_and_fetches() {
    use gtk::prelude::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    gtk::init().expect("a display");

    // Noise, so the PNG is too big to stay in the page.
    let pixels: Vec<u8> = (0u32..48 * 48 * 4)
        .map(|i| (i.wrapping_mul(2_654_435_761) >> 24) as u8)
        .collect();
    let texture = gtk::gdk::MemoryTexture::new(
        48,
        48,
        gtk::gdk::MemoryFormat::R8g8b8a8,
        &gtk::glib::Bytes::from_owned(pixels),
        48 * 4,
    );
    let png = texture.save_to_png_bytes();
    let b64 = crate::encoding::base64_encode(&png);
    let mut m = msg_for_print();
    m.body = format!(r#"<p>pictured</p><img src="data:image/png;base64,{b64}">"#);
    let html = MessageView::conversation_document(
        &[m],
        &Default::default(),
        &Default::default(),
        &Default::default(),
        &[],
        "#3584e4",
        true,
        false,
        false,
        false,
        &crate::config::ReaderStyle::NONE,
        &Default::default(),
        false,
        100,
    );
    assert!(!html.contains(&b64), "the picture is still in the page");
    assert!(html.contains("hylki-inline://picture/"));

    let view = super::super::webview::new_webview();
    let win = gtk::Window::new();
    win.set_child(Some(&view));
    win.set_default_size(800, 600);
    win.present();
    view.load_html(&html, Some("https://hylki.localhost/message/1"));

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let ctx = gtk::glib::MainContext::default();
    let pump = |done: &dyn Fn() -> bool| {
        while !done() && std::time::Instant::now() < deadline {
            if !ctx.iteration(false) {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        }
    };
    let answer: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
    let ask = |script: &'static str| -> String {
        answer.replace(None);
        let out = answer.clone();
        view.call_async_javascript_function(
            script,
            None,
            None,
            None,
            gtk::gio::Cancellable::NONE,
            move |res| {
                *out.borrow_mut() = Some(match res {
                    Ok(v) => v.to_str().to_string(),
                    Err(e) => format!("ERROR {e}"),
                });
            },
        );
        pump(&|| answer.borrow().is_some());
        answer.borrow().clone().unwrap_or_default()
    };

    // The frame's picture, once it has loaded: its width says it decoded.
    let shown = "try { const im = document.querySelector('iframe.hylki-frame')\
                   .contentDocument.images[0];\
                 return im && im.complete ? String(im.naturalWidth) : 'waiting'; }\
               catch (e) { return 'waiting'; }";
    let mut width = String::new();
    while std::time::Instant::now() < deadline {
        width = ask(shown);
        if width != "waiting" {
            break;
        }
        let pause = std::time::Instant::now() + std::time::Duration::from_millis(100);
        pump(&|| std::time::Instant::now() >= pause);
    }
    assert_eq!(width, "48", "the frame did not show the lifted picture");

    let fetched = ask(
        "const im = document.querySelector('iframe.hylki-frame').contentDocument.images[0];\
         const r = await fetch(im.src); return String((await r.blob()).size);",
    );
    assert_eq!(
        fetched,
        png.len().to_string(),
        "the picture could not be fetched"
    );
}
