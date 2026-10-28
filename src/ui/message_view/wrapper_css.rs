//! The wrapper document's stylesheet: the cards, their headers, chips and
//! banners, in the reader's grounds and accent. One `format!` so the bytes
//! stay exactly what the page has always received.

/// The wrapper's stylesheet: the cards, their headers and chips, in the
/// reader's grounds and accent.
pub(super) fn wrapper_css(
    scheme: &str,
    page: &str,
    bg: &str,
    chrome: &str,
    accent: &str,
    toggle_color: &str,
    plain_css: &str,
) -> String {
    format!(
        "\
         :root{{color-scheme:{scheme};}}\
         body{{margin:0;padding:0;background:{page};font:14px/1.55 system-ui,sans-serif;}}\
         body.hylki-conv{{padding:14px;}}\
         iframe.hylki-frame{{width:100%;border:0;display:block;background:{bg};\
         visibility:hidden;}}\
         iframe.hylki-frame.hylki-live{{visibility:visible;}}\
         .hylki-pan{{overflow-x:auto;position:relative;}}\
         iframe.hylki-frame.anim{{transition:height 240ms cubic-bezier(0.4,0,0.2,1);}}\
         @media (prefers-reduced-motion:reduce){{iframe.hylki-frame.anim{{transition:none;}}}}\
         .hylki-msg{{background:{bg};\
         border-radius:12px;overflow:hidden;margin:0 0 14px;}}\
         .hylki-msg:last-child{{margin-bottom:0;}}\
         .hylki-msg{{user-select:none;}}\
         body:not(.hylki-conv) .hylki-msg{{border-radius:0;margin:0;}}\
         .hylki-msg.selected{{box-shadow:0 0 0 2px {accent};}}\
         .hylki-fbar{{display:none;}}\
         .hylki-msg.hylki-folded>.hylki-fbar{{display:flex;align-items:flex-start;gap:12px;\
         padding:14px 18px;cursor:pointer;user-select:none;}}\
         .hylki-fbar-main{{flex:1 1 auto;min-width:0;display:flex;flex-direction:column;gap:2px;}}\
         .hylki-fbar-top{{display:flex;align-items:center;gap:12px;min-height:26px;}}\
         .hylki-fbar>.hylki-ava{{align-self:flex-start;}}\
         .hylki-fbar-prev{{display:-webkit-box;-webkit-box-orient:vertical;overflow:hidden;\
         opacity:0.6;font-size:0.92em;line-height:1.35;overflow-wrap:anywhere;}}\
         .hylki-fbar-from{{flex:1 1 auto;min-width:0;overflow:hidden;text-overflow:ellipsis;\
         white-space:nowrap;opacity:0.85;}}\
         .hylki-msg.unread .hylki-fbar-from{{font-weight:700;opacity:1;}}\
         .hylki-fbar-ico{{flex:none;display:inline-flex;opacity:0.55;line-height:0;}}\
         .hylki-fbar-ico svg{{width:16px;height:16px;}}\
         .hylki-fbar-none{{width:16px;height:16px;}}\
         .hylki-fbar-ico svg,.hylki-fbar-ico svg *{{fill:currentColor;}}\
         .hylki-fbar-date{{flex:none;min-width:6.5em;text-align:end;opacity:0.7;\
         font-size:0.92em;white-space:nowrap;}}\
         .hylki-msg.hylki-folded>:not(.hylki-fbar){{display:none;}}\
         body.hylki-folds .hylki-msg-hdr{{cursor:pointer;}}\
         @media print{{.hylki-msg.hylki-folded>:not(.hylki-fbar){{display:revert !important;}}\
         .hylki-msg.hylki-folded>.hylki-fbar{{display:none !important;}}}}\
         .hylki-msg-hdr{{cursor:pointer;}}\
         .hylki-msg-hdr{{padding:12px 16px;cursor:default;user-select:none;\
         position:sticky;top:0;z-index:1;background-color:{bg};}}\
         /* 8px between items on a line, but only 2px between wrapped\
         lines — the flex gap otherwise pads rows apart too. */\
         .hylki-hdr-line{{display:flex;column-gap:8px;row-gap:2px;\
         align-items:baseline;flex-wrap:nowrap;\
         min-width:0;position:relative;}}\
         .hylki-hdr-meta{{display:flex;gap:8px;align-items:baseline;flex:none;\
         margin-inline-start:auto;}}\
         /* Narrow pane: the meta group (folder chip, recipients chip, date)\
         drops to its own line as one unit — no mid-text wrapping — while\
         the sender and the action palette hold the first line. */\
         @media (max-width:620px){{\
         /* Tighten the right-hand insets (card gutter + header padding)\
         so the palette toggle sits nearer the pane's edge — but the\
         combined inset never drops below 20px. */\
         body.hylki-conv{{padding:10px;}}\
         body.hylki-conv .hylki-msg-hdr{{padding:12px 10px;}}\
         .hylki-hdr-line{{flex-wrap:wrap;}}\
         .hylki-hdr-meta{{order:10;flex-basis:100%;margin-inline-start:34px;\
         justify-content:flex-start;}}\
         /* Below the wrapped meta line the recipients need a touch\
         more air than the wide layout's 2px. */\
         .hylki-rcpt{{margin-top:6px;}}\
         /* With the meta group gone from the first line, the palette\
         pins itself to the corner (the \u{22ef} is already absolute). */\
         body:not([data-hylki-acts=\"toggle\"]) .hylki-acts{{margin-inline-start:auto;}}\
         }}\
         body:not(.hylki-conv) .hylki-msg-hdr{{background-color:{chrome};}}\
         {plain_css}\
         .hylki-ava{{width:26px;height:26px;border-radius:50%;flex:none;align-self:center;\
         display:flex;align-items:center;justify-content:center;color:#fff;\
         font-size:0.8em;font-weight:700;}}\
         /* Zero flex-basis: the name and address only absorb free space\
         (up to their natural width), so their length can never force\
         the palette onto another line. Under pressure the address\
         gives way first (tiny grow share), fading out at its right\
         edge; the name ellipsizes only after that. */\
         .hylki-from{{font-weight:700;flex:99 1 0;max-width:max-content;min-width:0;\
         white-space:nowrap;overflow:hidden;text-overflow:ellipsis;}}\
         .hylki-tags{{display:inline-flex;gap:4px;align-items:center;flex:0 0 auto;}}\
         .hylki-tags:empty{{display:none;}}\
         /* A card's own attachments (#213): chips beneath the body. */\
         .hylki-atts{{display:flex;flex-wrap:wrap;gap:6px;padding:2px 14px 12px;}}\
         .hylki-atts:empty{{display:none;}}\
         /* The Unsubscribe banner between a list message's header and\
         body: a line of text and one button. */\
         .hylki-tr{{display:flex;align-items:center;gap:10px;padding:8px 14px 9px;\
         font-size:0.9em;border-bottom:1px solid rgba(128,128,128,0.25);}}\
         .hylki-tr:empty{{display:none;}}\
         .hylki-tr-text{{flex:1 1 auto;min-width:0;opacity:0.8;}}\
         .hylki-tr-btn{{flex:none;border:none;border-radius:6px;\
         padding:3px 12px;background:color-mix(in srgb,currentColor 10%,transparent);\
         color:inherit;font:inherit;font-size:0.95em;font-weight:600;cursor:pointer;}}\
         .hylki-tr-btn:hover{{background:color-mix(in srgb,currentColor 15%,transparent);}}\
         .hylki-tr-btn:active{{background:color-mix(in srgb,currentColor 30%,transparent);}}\
         .hylki-unsub,.hylki-mdn{{display:flex;align-items:center;gap:10px;padding:8px 14px 9px;\
         font-size:0.9em;border-bottom:1px solid rgba(128,128,128,0.25);}}\
         .hylki-unsub:empty,.hylki-mdn:empty{{display:none;}}\
         .hylki-unsub-text{{flex:1 1 auto;min-width:0;opacity:0.8;}}\
         .hylki-unsub-btn{{flex:none;border:none;border-radius:6px;\
         padding:3px 12px;background:color-mix(in srgb,currentColor 10%,transparent);\
         color:inherit;font:inherit;font-size:0.95em;font-weight:600;cursor:pointer;}}\
         .hylki-unsub-btn:hover{{background:color-mix(in srgb,currentColor 15%,transparent);}}\
         .hylki-unsub-btn:active{{background:color-mix(in srgb,currentColor 30%,transparent);}}\
         /* The invitation banner (#223): the meeting at the top of\
         the card, and the answer buttons under it. */\
         .hylki-inv{{display:flex;gap:10px;padding:10px 14px 11px;font-size:0.9em;\
         border-bottom:1px solid rgba(128,128,128,0.25);}}\
         .hylki-inv:empty{{display:none;}}\
         .hylki-inv-icon{{flex:none;padding-top:1px;opacity:0.7;}}\
         .hylki-inv-icon svg{{width:20px;height:20px;display:block;}}\
         .hylki-inv-icon svg,.hylki-inv-icon svg *{{fill:currentColor;}}\
         .hylki-inv-body{{flex:1 1 auto;min-width:0;}}\
         .hylki-inv-head{{display:flex;align-items:baseline;gap:8px;flex-wrap:wrap;}}\
         .hylki-inv-title{{font-weight:700;font-size:1.05em;}}\
         .hylki-inv-title.off{{text-decoration:line-through;opacity:0.7;}}\
         .hylki-inv-kind{{font-size:0.85em;text-transform:uppercase;\
         letter-spacing:0.04em;opacity:0.6;}}\
         .hylki-inv-row{{opacity:0.8;margin-top:2px;}}\
         .hylki-inv-note{{margin-top:4px;opacity:0.8;font-style:italic;}}\
         .hylki-inv-acts{{display:flex;flex-wrap:wrap;gap:6px;margin-top:8px;}}\
         .hylki-inv-btn{{border:1px solid rgba(128,128,128,0.4);border-radius:6px;\
         padding:3px 12px;background:rgba(128,128,128,0.1);color:inherit;font:inherit;\
         font-size:0.95em;font-weight:600;cursor:pointer;}}\
         .hylki-inv-btn:hover{{background:rgba(128,128,128,0.22);}}\
         /* The answer already given is marked, so a meeting opened\
         again shows where it stands at a glance. */\
         .hylki-inv-btn.on{{border-color:{accent};color:{accent};\
         background:color-mix(in srgb, {accent} 16%, transparent);}}\
         .hylki-attw{{display:inline-flex;align-items:stretch;max-width:100%;\
         border:1px solid rgba(128,128,128,0.35);border-radius:8px;overflow:hidden;\
         background:rgba(128,128,128,0.08);}}\
         .hylki-attc{{display:inline-flex;align-items:center;gap:6px;padding:4px 8px;\
         border:none;background:none;color:inherit;font:inherit;font-size:0.85em;\
         cursor:pointer;min-width:0;max-width:320px;}}\
         .hylki-attc:hover{{background:rgba(128,128,128,0.18);}}\
         .hylki-attc svg{{width:16px;height:16px;flex:none;display:block;}}\
         .hylki-attc svg,.hylki-attc svg *{{fill:currentColor;}}\
         .hylki-attc.ftype-pdf svg{{color:#e01b24;}}\
         .hylki-attc.ftype-doc svg{{color:#3584e4;}}\
         .hylki-attc.ftype-sheet svg{{color:#2ec27e;}}\
         .hylki-attc.ftype-slides svg{{color:#ff7800;}}\
         .hylki-attc.ftype-archive svg{{color:#e5a50a;}}\
         .hylki-attc.ftype-audio svg{{color:#9141ac;}}\
         .hylki-attc.ftype-video svg{{color:#e6689e;}}\
         .hylki-attc.ftype-calendar svg{{color:#009e8f;}}\
         .hylki-attc.ftype-image svg{{color:#1c9bd8;}}\
         .hylki-attc.ftype-generic svg{{color:#8c8c8c;}}\
         .hylki-attn{{white-space:nowrap;overflow:hidden;text-overflow:ellipsis;min-width:0;}}\
         .hylki-attz{{opacity:0.55;white-space:nowrap;flex:none;}}\
         .hylki-attsave{{display:inline-flex;align-items:center;padding:0 7px;border:none;\
         border-inline-start:1px solid rgba(128,128,128,0.35);background:none;color:inherit;\
         cursor:pointer;opacity:0.6;}}\
         .hylki-attsave:hover{{opacity:1;background:rgba(128,128,128,0.18);}}\
         .hylki-attsave svg{{width:14px;height:14px;display:block;}}\
         .hylki-attsave svg,.hylki-attsave svg *{{fill:currentColor;}}\
         .hylki-atts.flash .hylki-attw{{animation:hylki-att-flash 1.2s ease-out;}}\
         @keyframes hylki-att-flash{{0%,40%{{box-shadow:0 0 0 3px rgba(53,132,228,0.7);}}\
         100%{{box-shadow:0 0 0 3px rgba(53,132,228,0);}}}}\
         .hylki-tag{{font-size:0.72em;font-weight:700;line-height:1.4;padding:1px 7px;\
         border-radius:9999px;white-space:nowrap;}}\
         .hylki-label{{max-width:16em;\
         overflow:hidden;text-overflow:ellipsis;}}\
         .hylki-addr{{opacity:0.55;font-size:0.9em;flex:1 1 0;max-width:max-content;\
         min-width:0;white-space:nowrap;overflow:hidden;text-overflow:clip;\
         position:relative;}}\
         /* The fade exists only while something actually collides with\
         the address (`clipped` is set by measurement, below). */\
         .hylki-addr.clipped{{\
         -webkit-mask-image:linear-gradient(to right,#000 calc(100% - 18px),transparent);\
         mask-image:linear-gradient(to right,#000 calc(100% - 18px),transparent);}}\
         /* Hovering a clipped address reveals it in full: an overlay\
         pinned to the span's spot, floating over whatever crowded it\
         out, gone when the cursor leaves. */\
         .hylki-addr.clipped:hover{{-webkit-mask-image:none;mask-image:none;\
         z-index:6;}}\
         /* Addresses act as links: click composes to them, right-click\
         offers Copy / New Message. */\
         /* Sender-authentication seal (#88): invisible until a\
         verdict arrives; a pass wears Bazaar's fixed blue (never the\
         system accent), problems the warning/error reds.\
         Sized in em and anchored to the text baseline, then eased\
         down to the name's optical centre with an em transform\
         (margins can't move it: WebKit synthesizes the missing\
         baseline before they apply) — a fixed centred pixel box sat\
         visibly low whenever a GNOME text scaling factor shrank the\
         type around it. */\
         .hylki-verify{{display:none;background:none;border:none;\
         padding:0 2px;margin-inline-start:2px;cursor:pointer;line-height:0;\
         align-self:baseline;transform:translateY(0.18em);flex:none;}}\
         .hylki-verify.on{{display:inline-flex;}}\
         .hylki-verify svg{{width:0.95em;height:0.95em;display:block;}}\
         .hylki-verify svg,.hylki-verify svg *{{fill:currentColor;}}\
         .hylki-verify.trust-pass{{color:#3584e4;}}\
         .hylki-verify.trust-unverified{{color:currentColor;opacity:0.4;}}\
         .hylki-verify.trust-suspicious{{color:#cd9309;}}\
         .hylki-verify.trust-fail{{color:#c01c28;}}\
         /* The OpenPGP chip (#133), a labelled pill since #300: the\
         verdict reads in words on a wash of its color. Text of its\
         own, so it sits on the name's baseline without the seal's\
         nudge. */\
         .hylki-pgp{{display:none;font:inherit;font-size:0.78em;font-weight:600;\
         border:none;border-radius:999px;padding-block:0.1em;padding-inline:0.45em 0.6em;\
         margin-inline-start:4px;cursor:pointer;align-items:center;gap:0.3em;\
         align-self:baseline;white-space:nowrap;flex:none;color:inherit;\
         background:rgba(128,128,128,0.16);}}\
         .hylki-pgp.on{{display:inline-flex;}}\
         .hylki-pgp svg{{width:1.1em;height:1.1em;display:none;flex:none;}}\
         .hylki-pgp svg,.hylki-pgp svg *{{fill:currentColor;}}\
         .hylki-pgp.enc svg:first-child{{display:block;}}\
         .hylki-pgp.sig svg:nth-child(2){{display:block;}}\
         .hylki-pgp.pgp-good{{color:#26a269;background:rgba(38,162,105,0.16);}}\
         .hylki-pgp.pgp-warn{{color:#cd9309;background:rgba(205,147,9,0.16);}}\
         .hylki-pgp.pgp-bad{{color:#c01c28;background:rgba(192,28,40,0.14);}}\
         .hylki-pgp:hover{{filter:brightness(1.1);}}\
         /* The sensitivity label (#411): the OpenPGP pill's shape in a\
         neutral wash, since a label is a statement, not a verdict.\
         Zero flex-basis and a small share, like the address: the\
         name fills first and a long label ellipsizes before it. */\
         .hylki-sens{{display:none;font-size:0.78em;font-weight:600;\
         border-radius:999px;padding-block:0.1em;padding-inline:0.45em 0.6em;\
         margin-inline-start:4px;align-items:center;gap:0.3em;\
         align-self:baseline;white-space:nowrap;flex:1 1 0;min-width:3.5em;max-width:max-content;\
         background:rgba(128,128,128,0.16);cursor:default;}}\
         .hylki-sens.on{{display:inline-flex;}}\
         .hylki-sens svg{{width:1em;height:1em;flex:none;}}\
         .hylki-sens svg,.hylki-sens svg *{{fill:var(--hylki-sens,currentColor);}}\
         .hylki-sens-text{{overflow:hidden;text-overflow:ellipsis;}}\
         /* The icons alone (Settings, #300): the chip as it was before\
         it had words, sitting on the name's line like the seal. */\
         body[data-hylki-pgpicons] .hylki-pgp{{background:none;padding:0 2px;\
         margin-inline-start:2px;gap:1px;font-size:inherit;line-height:0;\
         transform:translateY(0.18em);}}\
         body[data-hylki-pgpicons] .hylki-pgp svg{{width:0.95em;height:0.95em;}}\
         body[data-hylki-pgpicons] .hylki-pgp-text{{display:none;}}\
         .hylki-mail{{cursor:pointer;}}\
         .hylki-mail:hover{{text-decoration:underline;}}\
         /* Styled after the app's own context menus (context_menu.rs +\
         .context-menu-list in styles.css): same padding, min width,\
         row shape and weight, so both menu families read as one. */\
         .hylki-mailmenu-scrim{{position:fixed;left:0;top:0;right:0;bottom:0;\
         z-index:98;}}\
         .hylki-mailmenu{{position:fixed;z-index:99;background:{bg};\
         border:1px solid rgba(128,128,128,0.2);border-radius:12px;\
         box-shadow:0 6px 24px rgba(0,0,0,0.22);padding:6px;\
         min-width:190px;font-size:0.95em;}}\
         .hylki-mailmenu button{{display:block;width:100%;text-align:start;\
         background:none;border:none;padding:8px 10px;border-radius:6px;\
         color:inherit;cursor:pointer;font:inherit;font-weight:normal;}}\
         .hylki-mailmenu button:hover{{background:rgba(128,128,128,0.14);}}\
         .hylki-addr.clipped:hover::after{{content:attr(data-addr);\
         position:absolute;inset-inline-start:-6px;top:50%;transform:translateY(-50%);\
         background:{bg};border-radius:6px;padding:1px 6px;\
         white-space:nowrap;text-decoration:underline;}}\
         body:not(.hylki-conv) .hylki-addr.clipped:hover::after{{\
         background:{chrome};}}\
         .hylki-date{{opacity:0.55;font-size:0.85em;flex:none;white-space:nowrap;}}\
         .hylki-read{{display:none;flex:none;align-self:center;margin-inline-end:-3px;}}\
         .hylki-read.asked,.hylki-read.read{{display:inline-flex;}}\
         .hylki-read svg{{width:1.05em;height:1.05em;display:block;}}\
         .hylki-read svg,.hylki-read svg *{{fill:currentColor;}}\
         .hylki-read.asked{{opacity:0.55;}}\
         .hylki-read.read{{color:#26a269;}}\
         .hylki-dot{{width:8px;height:8px;border-radius:50%;background:#3584e4;\
         flex:none;align-self:center;}}\
         .hylki-end{{height:1px;}}\
         .hylki-quote{{display:block;margin:0 16px 12px;padding:0 7px;\
         font:inherit;font-size:0.7em;line-height:1.45;letter-spacing:0.06em;\
         color:inherit;opacity:0.6;background:rgba(128,128,128,0.16);\
         border:0;border-radius:999px;cursor:pointer;}}\
         .hylki-quote:hover{{opacity:0.95;background:rgba(128,128,128,0.28);}}\
         .hylki-quote.open{{opacity:0.95;position:absolute;inset-inline-start:0;z-index:2;}}\
         .hylki-acts{{display:flex;gap:2px;flex:none;align-self:center;margin-inline-start:4px;}}\
         /* Read-toggle: the icon showing is the ACTION (read envelope\
         means mark-as-read); the section's unread class decides. */\
         .hylki-act .tr-when-unread,.hylki-act .tr-when-read{{display:none;line-height:0;}}\
         .hylki-msg.unread .hylki-act[data-act=\"toggleread\"] .tr-when-unread{{display:inline-flex;}}\
         .hylki-msg:not(.unread) .hylki-act[data-act=\"toggleread\"] .tr-when-read{{display:inline-flex;}}\
         .hylki-act{{color:inherit;background:none;border:none;border-radius:6px;\
         padding:5px 8px;cursor:pointer;opacity:0.7;\
         display:inline-flex;align-items:center;justify-content:center;\
         vertical-align:middle;line-height:0;\
         transition:opacity 120ms ease,background 120ms ease;}}\
         .hylki-act:hover{{opacity:1;background:rgba(128,128,128,0.18);}}\
         .hylki-act:active{{background:rgba(128,128,128,0.3);}}\
         .hylki-act svg{{width:14px;height:14px;display:block;}}\
         .hylki-act svg,.hylki-act svg *{{fill:currentColor;}}\
         /* A set star: the one state that carries color. */\
         .hylki-act.on{{color:#e5a50a;opacity:1;}}\
         .hylki-acts-toggle{{display:none;}}\
         /* Behind-the-\u{22ef} mode: the palette overlays the header line as an\
         absolute strip anchored at the toggle, expanding leftward over\
         the text beneath (never pushing it), its left edge fading in\
         from transparent. The \u{22ef} itself never moves. */\
         body[data-hylki-acts=\"toggle\"] .hylki-acts{{position:absolute;\
         inset-inline-end:34px;top:13px;transform:translateY(-50%) scaleX(0.6);\
         transform-origin:right center;margin:0;\
         padding-block:2px;padding-inline:30px 6px;border-radius:8px;\
         background:linear-gradient(to right,transparent,{bg} 26px);\
         opacity:0;pointer-events:none;\
         transition:opacity 140ms ease,transform 180ms ease;}}\
         body[data-hylki-acts=\"toggle\"] .hylki-acts.open{{opacity:1;\
         pointer-events:auto;transform:translateY(-50%) scaleX(1);}}\
         body:not(.hylki-conv)[data-hylki-acts=\"toggle\"] .hylki-acts{{\
         background:linear-gradient(to right,transparent,{chrome} 26px);}}\
         /* Always visible (not just on card hover), and OUT OF FLOW:\
         absolutely pinned to the header's right edge, so no amount of\
         header content can push or wrap it. The line reserves the\
         button's footprint as fixed right padding instead. */\
         body[data-hylki-acts=\"toggle\"] .hylki-hdr-line{{padding-inline-end:38px;}}\
         /* Anchored to the FIRST row's centre (the 26px avatar/sender\
         line), not the line box's — when the meta row wraps beneath,\
         the box grows downward but the button must not move. */\
         body[data-hylki-acts=\"toggle\"] .hylki-acts-toggle{{display:block;\
         position:absolute;inset-inline-end:0;top:13px;transform:translateY(-50%);\
         color:{toggle_color};background:none;border:none;border-radius:6px;\
         padding:4px 8px;margin:0;cursor:pointer;\
         opacity:0.65;transition:opacity 120ms ease,background 120ms ease;}}\
         /* `svg *` too: the embedded symbolic paths carry their own fill\
         attribute, which a rule on the svg element alone can't beat. */\
         .hylki-acts-toggle svg{{width:14px;height:14px;display:block;}}\
         .hylki-acts-toggle svg,.hylki-acts-toggle svg *{{fill:currentColor;}}\
         body[data-hylki-acts=\"toggle\"] .hylki-acts-toggle:hover{{opacity:1;\
         background:rgba(128,128,128,0.18);}}\
         body[data-hylki-acts=\"toggle\"] .hylki-acts-toggle.open{{opacity:1;}}\
         body[data-hylki-acts=\"hover\"] .hylki-acts{{opacity:0;pointer-events:none;\
         transition:opacity 300ms ease var(--acts-delay,0ms);}}\
         body[data-hylki-acts=\"hover\"] .hylki-msg:hover .hylki-acts{{opacity:1;\
         pointer-events:auto;transition:opacity 300ms ease 0ms;}}\
         .hylki-folder{{padding:0.05em 0.45em;border-radius:0.7em;\
         font-size:0.78em;opacity:0.75;border:1px solid currentColor;\
         white-space:nowrap;flex:none;}}\
         .hylki-rcpt-toggle{{font:inherit;font-size:0.78em;color:inherit;\
         background:color-mix(in srgb,currentColor 10%,transparent);\
         border:none;border-radius:6px;padding:0.15em 0.6em;cursor:pointer;\
         white-space:nowrap;flex:none;transition:background 120ms ease;}}\
         .hylki-rcpt-toggle:hover{{background:color-mix(in srgb,currentColor 15%,transparent);}}\
         .hylki-rcpt-toggle.open{{background:color-mix(in srgb,currentColor 20%,transparent);}}\
         /* Indented past the avatar (26px + 8px gap), so the recipients\
         align with the sender's name — as does the wrapped meta line. */\
         .hylki-rcpt{{font-size:0.85em;opacity:0.75;\
         user-select:text;overflow-wrap:anywhere;margin:2px 0 0;margin-inline-start:34px;}}\
         .hylki-rcpt div{{margin-top:2px;}}\
         .hylki-loading{{opacity:0.5;padding:16px;}}\
         /* The Copied pill after Ctrl+C: a toast the page draws itself,\
         since the reader has no toast overlay of its own. */\
         .hylki-copied{{position:fixed;left:50%;bottom:22px;transform:translateX(-50%) translateY(8px);\
         padding:8px 18px;border-radius:999px;background:rgba(40,40,40,0.92);color:#fff;\
         font:inherit;font-size:0.9em;box-shadow:0 2px 10px rgba(0,0,0,0.25);\
         opacity:0;pointer-events:none;transition:opacity 180ms ease,transform 180ms ease;z-index:50;}}\
         .hylki-copied.on{{opacity:1;transform:translateX(-50%) translateY(0);}}\
         /* Right to left (#366): what logical properties cannot say. */\
         html[dir=rtl] .hylki-addr.clipped{{\
         -webkit-mask-image:linear-gradient(to left,#000 calc(100% - 18px),transparent);\
         mask-image:linear-gradient(to left,#000 calc(100% - 18px),transparent);}}\
         html[dir=rtl] .hylki-addr.clipped:hover{{-webkit-mask-image:none;mask-image:none;}}\
         html[dir=rtl] body[data-hylki-acts=\"toggle\"] .hylki-acts{{\
         transform-origin:left center;\
         background:linear-gradient(to left,transparent,{bg} 26px);}}\
         html[dir=rtl] body:not(.hylki-conv)[data-hylki-acts=\"toggle\"] .hylki-acts{{\
         background:linear-gradient(to left,transparent,{chrome} 26px);}}\
         /* Each chip's words in their own direction, as a GTK label\
         has them: an English 1 recipient in a right-to-left line\
         otherwise reads recipient 1. */\
         html[dir=rtl] :is(.hylki-rcpt-toggle,.hylki-date,.hylki-fbar-date,\
         .hylki-folder,.hylki-rcpt div){{unicode-bidi:plaintext;}}\
         html[dir=rtl] .hylki-fbar-date{{text-align:left;}}\
         "
    )
}
