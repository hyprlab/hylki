#!/usr/bin/env python3
# Regression check for the reader's quote folding (SIZE_SCRIPT `quote()` in
# src/ui/message_view.rs): extracts the script from the source, loads it in a
# real WebKitGTK view with one iframe per case, and reports which bodies get a
# ••• button. Needs a display and python3-gobject with WebKit 6.0.
#
#   tools/test-quote-fold.py            # prints ok/FAIL per case, exits 1 on any FAIL
import re, json, sys, os, gi
gi.require_version('Gtk','4.0'); gi.require_version('WebKit','6.0')
from gi.repository import Gtk, WebKit, GLib
src=open(os.path.join(os.path.dirname(os.path.abspath(__file__)),'..','src','ui','message_view.rs'),encoding='utf-8').read()
m=re.search(r'const SIZE_SCRIPT: &str = "(.*?)";\n', src, re.S)
raw=m.group(1).replace('\\\n','')
# decode rust string escapes
js=re.sub(r'\\u\{([0-9a-fA-F]+)\}', lambda x: chr(int(x.group(1),16)), raw)
js=js.replace('\\"','"').replace('\\\\','\\')
# cut before DOMContentLoaded handler: keep function defs only
js=js[:js.index('document.addEventListener(\'DOMContentLoaded\'')]
stubs="var follow=null,hold=null;function chase(){}function pin(){}function reportPos(){}function markClipped(){}function selAll(){}function copySel(){}\n"
cases={
 'top_post':('<p>Reply</p><blockquote>quote</blockquote>',True),
 'interleaved':('<p>Hi John,</p><blockquote>John a ecrit: Hello bob, yada</blockquote><p>I dont understand what you mean...</p>',False),
 'top_post_sig_after':('<p>Reply</p><blockquote>q</blockquote><p>-- <br>Bob</p>',True),
 'outlook_header':('<p>Reply</p><div id="divRplyFwdMsg">From: x</div><div>original body text here</div>',True),
 'bottom_post':('<blockquote>q</blockquote><p>reply</p>',False),
 'gmail':('<div>reply</div><div class="gmail_quote">On x wrote:<blockquote class="gmail_quote">q</blockquote></div><div><br></div>',True),
 'two_quotes_then_sigclass':('<p>r</p><blockquote>q</blockquote><br><blockquote>q2</blockquote><div class="moz-signature">-- Bob</div>',True),
 'list_footer':('<p>r</p><blockquote>q</blockquote><p>_______________________________________________<br>foo mailing list</p>',True),
 'interleaved_multi':('<p>Hi</p><blockquote>q1</blockquote><p>a1</p><blockquote>q2</blockquote><p>a2</p>',False),
 'nbsp_after':('<p>r</p><blockquote>q</blockquote><p>&nbsp;</p>',True),
 'thunderbird_interleaved':('<div class="moz-cite-prefix">John wrote:</div><blockquote type="cite">q</blockquote><p>answer</p>',False),
 'text_node_after':('<p>r</p><blockquote>q</blockquote>plain reply text after',False),
}
frames=''.join(f'<iframe class="hylki-frame" id="{k}" srcdoc="{v[0].replace("&","&amp;").replace(chr(34),"&quot;")}"></iframe>' for k,v in cases.items())
html=f'<!doctype html><html><body>{frames}<script>{stubs}{js}</script></body></html>'

app=Gtk.Application(application_id='co.hyprlab.QuoteTest')
def activate(a):
    w=Gtk.Window(application=a); v=WebKit.WebView(); w.set_child(v); w.set_default_size(400,300); w.present()
    def loaded(view,ev):
        if ev!=WebKit.LoadEvent.FINISHED: return
        def run():
            v.evaluate_javascript("""(function(){var r={};var fs=document.querySelectorAll('iframe.hylki-frame');for(var i=0;i<fs.length;i++){var f=fs[i];quote(f);r[f.id]=!!(f.nextSibling&&f.nextSibling.className==='hylki-quote');}return JSON.stringify(r);})()""",-1,None,None,None,done)
        def done(view,res):
            val=view.evaluate_javascript_finish(res); r=json.loads(val.to_string()); bad=0
            for k,(h,exp) in cases.items():
                ok=r.get(k)==exp; bad+= (not ok); print(('ok  ' if ok else 'FAIL'),k,'hidden=',r.get(k),'expected=',exp)
            print('RESULT', 'PASS' if not bad else f'{bad} FAILED'); a.quit(); sys.exit(1 if bad else 0)
        GLib.timeout_add(600,run)
    v.connect('load-changed',loaded); v.load_html(html,'file:///')
app.connect('activate',activate); app.run([])
