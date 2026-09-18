#!/usr/bin/env python3
# Probe: make one OpenURI request the way src/ui/launch.rs does (own
# handle_token, Response watched before the call) and print the response
# code. Run inside the sandbox to check the portal road:
#   flatpak run --command=python3 co.hyprlab.Hylki $PWD/tools/portal-openuri-probe.py [--ask]
import sys, os, gi
from gi.repository import Gio, GLib
ask = '--ask' in sys.argv
uri = 'https://example.com/hylki-portal-probe'
conn = Gio.bus_get_sync(Gio.BusType.SESSION, None)
token = 'hylki' + str(os.getpid())
sender = conn.get_unique_name()[1:].replace('.', '_')
path = f'/org/freedesktop/portal/desktop/request/{sender}/{token}'
loop = GLib.MainLoop()
def on_response(c, s, p, i, sig, params):
    print('Response', params.get_child_value(0).get_uint32(), flush=True); loop.quit()
conn.signal_subscribe('org.freedesktop.portal.Desktop', 'org.freedesktop.portal.Request', 'Response', path, None, Gio.DBusSignalFlags.NONE, on_response)
opts = {'handle_token': GLib.Variant('s', token)}
if ask: opts['ask'] = GLib.Variant('b', True)
params = GLib.Variant('(ssa{sv})', ('', uri, opts))
def done(c, res):
    try: print('call ok', c.call_finish(res).print_(True), flush=True)
    except Exception as e: print('call FAILED', e, flush=True); loop.quit()
conn.call('org.freedesktop.portal.Desktop', '/org/freedesktop/portal/desktop', 'org.freedesktop.portal.OpenURI', 'OpenURI', params, None, Gio.DBusCallFlags.NONE, 10000, None, done)
GLib.timeout_add_seconds(25, lambda: (print('timeout, no Response'), loop.quit()))
loop.run()
