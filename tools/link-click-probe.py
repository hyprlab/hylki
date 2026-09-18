#!/usr/bin/env python3
# Probe: does a link click inside the reader's sandboxed srcdoc frame reach
# decide-policy, and does GIO's launch work from here? Mirrors message_view.rs.
import sys, gi
gi.require_version('Gtk','4.0'); gi.require_version('WebKit','6.0')
from gi.repository import Gtk, WebKit, GLib, Gio
LAUNCH = '--launch' in sys.argv
inner = ('<a id="a" href="https://example.com/plain">plain</a> '
         '<a id="b" target="_blank" href="https://example.com/blank">blank</a> '
         '<a id="c" href="mailto:x@example.com">mail</a>')
esc = inner.replace('&','&amp;').replace('"','&quot;').replace('<','&lt;').replace('>','&gt;')
html = ('<!doctype html><html><head><meta http-equiv="Content-Security-Policy" '
        'content="script-src \'nonce-abc\'; object-src \'none\'; base-uri \'none\'"></head><body>'
        f'<div class="hylki-pan"><iframe id="f" sandbox="allow-same-origin allow-popups" srcdoc="{esc}"></iframe></div>'
        '</body></html>')
seen = []
app = Gtk.Application(application_id='co.hyprlab.Hylki.LinkProbe')
def activate(a):
    w = Gtk.Window(application=a); v = WebKit.WebView(); w.set_child(v); w.set_default_size(400,300); w.present()
    def policy(view, decision, dtype):
        if dtype in (WebKit.PolicyDecisionType.NAVIGATION_ACTION, WebKit.PolicyDecisionType.NEW_WINDOW_ACTION):
            nav = decision.get_navigation_action()
            uri = nav.get_request().get_uri()
            print('policy', dtype.value_nick, nav.get_navigation_type().value_nick, uri, 'main=', decision.props.frame_name if hasattr(decision.props,'frame_name') else '?', flush=True)
            if uri.startswith('https://hylki.localhost') or uri.startswith('about:'):
                return False
            seen.append(uri)
            if LAUNCH:
                try:
                    Gio.AppInfo.launch_default_for_uri(uri, None); print('launch ok', flush=True)
                except Exception as e:
                    print('launch FAILED', e, flush=True)
            decision.ignore(); return True
        return False
    v.connect('decide-policy', policy)
    def loaded(view, ev):
        if ev != WebKit.LoadEvent.FINISHED: return
        def click(i):
            v.evaluate_javascript(f"document.getElementById('f').contentDocument.getElementById('{i}').click(); 'done'", -1, None, None, None, lambda vw,res: print('clicked', i, flush=True))
        GLib.timeout_add(600, lambda: (click('a'), False)[1])
        GLib.timeout_add(1400, lambda: (click('b'), False)[1])
        GLib.timeout_add(2200, lambda: (click('c'), False)[1])
        def finish():
            print('RESULT policy fired for:', seen, flush=True); a.quit(); return False
        GLib.timeout_add(3200, finish)
    v.connect('load-changed', loaded); v.load_html(html, 'https://hylki.localhost/message/1')
app.connect('activate', activate); app.run([])
