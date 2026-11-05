//! Where the cards and the editor's text are, measured in the web views by
//! script.

use super::Card;
use gtk::graphene;

/// The script that lists the reader document's cards: each one's key, its
/// box in the view's own pixels, and its header's height, with the view's
/// width in the document's pixels to scale them by.
pub(super) const CARDS_JS: &str = concat!("(function(){", first_text_js!(), "var r=[];var ms=document.querySelectorAll('.hylki-msg');\
for(var i=0;i<ms.length;i++){var b=ms[i].getBoundingClientRect();\
var h=ms[i].querySelector('.hylki-msg-hdr');\
var f=ms[i].querySelector('iframe.hylki-frame'),t=null;\
try{var q=f&&f.contentDocument?ft(f.contentDocument):null;\
if(q){var fr=f.getBoundingClientRect();t=[fr.left+q[0],fr.top+q[1],q[2]];}}catch(_){}\
r.push([ms[i].dataset.key||'',b.left,b.top,b.width,b.height,h?h.getBoundingClientRect().height:0,t]);}\
var fs=document.querySelectorAll('iframe.hylki-frame'),live=true;\
for(var j=0;j<fs.length;j++){if(!fs[j].classList.contains('hylki-live')&&!fs[j].hasAttribute('data-hylki-srcdoc'))live=false;}\
return JSON.stringify({w:window.innerWidth,y:window.scrollY,live:live,cards:r});})()");

/// The script that finds where the editor's first line of text begins.
pub(super) const EDITOR_TEXT_JS: &str = concat!(
    "(function(){",
    first_text_js!(),
    "return JSON.stringify({w:window.innerWidth,t:ft(document)});})()"
);

/// `ft(doc)`: the top-left corner of the first text a document shows, or
/// null. Text that is not laid out (styles, a folded quote) has no box.
macro_rules! first_text_js {
    () => {
        "function ft(d){var w=d.createTreeWalker(d.body,4),n;\
         while((n=w.nextNode())){if(!n.textContent.trim())continue;\
         var g=d.createRange();g.selectNodeContents(n);var c=g.getClientRects();\
         for(var i=0;i<c.length;i++){if(c[i].width>0){var z=parseFloat(d.documentElement.style.zoom)||1;\
         return [c[i].left,c[i].top,parseFloat(d.defaultView.getComputedStyle(n.parentElement).fontSize)*z];}}}\
         return null;}"
    };
}

use first_text_js;

/// Where [`EDITOR_TEXT_JS`] found the first line, in stage coordinates.
pub(super) fn parse_text(json: &str, view: &graphene::Rect) -> Option<(graphene::Point, f32)> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let css_w = v.get("w")?.as_f64()? as f32;
    let k = if css_w > 0.0 {
        view.width() / css_w
    } else {
        1.0
    };
    let t = v.get("t")?.as_array()?;
    let n = |i: usize| t.get(i).and_then(|x| x.as_f64()).map(|x| x as f32);
    Some((
        graphene::Point::new(view.x() + n(0)? * k, view.y() + n(1)? * k),
        n(2).unwrap_or(0.0) * k,
    ))
}

/// One card as [`CARDS_JS`] reports it, in stage coordinates: its key, its
/// box and header height `[x, y, width, height, head]`, and where its first
/// line of text begins `(x, y, size)` when it has one.
type MeasuredCard = (String, [f32; 5], Option<(f32, f32, f32)>);

/// What [`CARDS_JS`] reported, placed in stage coordinates given the web
/// view's box there.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Measured {
    pub(super) live: bool,
    /// The document's scroll, which must hold still too.
    scroll: f64,
    pub(super) cards: Vec<MeasuredCard>,
}

impl Measured {
    pub(super) fn parse(json: &str, view: &graphene::Rect) -> Option<Measured> {
        let v: serde_json::Value = serde_json::from_str(json).ok()?;
        let css_w = v.get("w")?.as_f64()? as f32;
        let k = if css_w > 0.0 {
            view.width() / css_w
        } else {
            1.0
        };
        let cards = v
            .get("cards")?
            .as_array()?
            .iter()
            .filter_map(|c| {
                let c = c.as_array()?;
                let key = c.first()?.as_str()?.to_string();
                let n = |i: usize| c.get(i).and_then(|x| x.as_f64()).map(|x| x as f32);
                let text = c.get(6).and_then(|t| t.as_array()).and_then(|t| {
                    let n = |i: usize| t.get(i).and_then(|x| x.as_f64()).map(|x| x as f32);
                    Some((
                        view.x() + n(0)? * k,
                        view.y() + n(1)? * k,
                        n(2).unwrap_or(0.0) * k,
                    ))
                });
                Some((
                    key,
                    [
                        view.x() + n(1)? * k,
                        view.y() + n(2)? * k,
                        n(3)? * k,
                        n(4)? * k,
                        n(5)? * k,
                    ],
                    text,
                ))
            })
            .collect();
        Some(Measured {
            live: v.get("live").and_then(|l| l.as_bool()).unwrap_or(false),
            scroll: v.get("y").and_then(|y| y.as_f64()).unwrap_or(0.0),
            cards,
        })
    }

    pub(super) fn cards(&self) -> Vec<Card> {
        self.cards
            .iter()
            .map(|(key, b, t)| Card {
                key: key.clone(),
                rect: graphene::Rect::new(b[0], b[1], b[2], b[3]),
                head: b[4],
                text: t.map(|(x, y, size)| (graphene::Point::new(x, y), size)),
            })
            .collect()
    }
}
