
(function(){
  window.__hylkiPasteOnce = null;
  var urlRe = /((?:https?:\/\/|www\.)[^\s<>()]+[^\s<>().,;:!?'"])/gi;
  function esc(s){return s.replace(/&/g,'&amp;').replace(/</g,'&lt;').replace(/>/g,'&gt;');}
  /* Inline images (#113): a pasted or dropped image lands in the text as an
     <img src="data:..."> at the caret; the send path lifts it into a cid:
     part. Oversized images are downscaled on insert — a phone screenshot is
     thousands of pixels the recipient's pane will never show. */
  var MAXPX = 1600;
  function insertImageFile(f){
    if(!f) return;
    var r = new FileReader();
    r.onload = function(){
      scaleUrl(String(r.result), f.type, function(url){
        document.execCommand('insertHTML', false, '<img src="' + url + '" style="max-width:100%">');
      });
    };
    r.readAsDataURL(f);
  }
  function scaleUrl(url, type, done){
    var im = new Image();
    im.onload = function(){
      var w = im.naturalWidth, h = im.naturalHeight;
      if(w > MAXPX || h > MAXPX){
        var s = Math.min(MAXPX / w, MAXPX / h);
        var c = document.createElement('canvas');
        c.width = Math.round(w * s); c.height = Math.round(h * s);
        c.getContext('2d').drawImage(im, 0, 0, c.width, c.height);
        url = type === 'image/jpeg' ? c.toDataURL('image/jpeg', 0.85) : c.toDataURL('image/png');
      }
      done(url);
    };
    /* A picture the engine can't decode still has to let go of the queue
       below, or every image after it waits forever. */
    im.onerror = function(){ done(url); };
    im.src = url;
  }
  /* The way in for a file the widget read for us: dropped or pasted from a
     file manager, which the document itself never gets to see (the app's
     drop target and paste path explain why). One queue keeps a batch in the
     order it was dropped — each insert waits on an image decode, so parallel
     calls would race and land backwards. `x`/`y` place the caret at the drop
     point and are given for the first of a batch only; the rest follow the
     caret the previous insert left behind. */
  var insertQ = Promise.resolve();
  window.__hylkiInsertImageURL = function(url, type, name, x, y){
    insertQ = insertQ.then(function(){
      return new Promise(function(done){
        if(x !== null && document.caretRangeFromPoint){
          var r = document.caretRangeFromPoint(x, y);
          if(r){ var s = getSelection(); s.removeAllRanges(); s.addRange(r); }
        }
        /* No drop point and no caret in the text (the composer's drop
           surfaces, with the focus in the To row): the line written on
           first, at the top, rather than nowhere. */
        var cur = getSelection();
        if(!cur.rangeCount || !document.body.contains(cur.anchorNode)){
          var top = document.createRange();
          top.setStart(document.body.firstChild || document.body, 0);
          top.collapse(true);
          cur.removeAllRanges(); cur.addRange(top);
        }
        scaleUrl(url, type, function(u){
          /* The file's own name rides along as alt, which is the only place
             left to keep it: the picture is a data: URI by now, and the send
             path reads this back to name the cid: part. It is also what an
             alt attribute is for, so a recipient whose client blocks images
             reads the filename rather than nothing. */
          var a = name ? ' alt="' + esc(name).replace(/"/g, '&quot;') + '"' : '';
          document.execCommand('insertHTML', false,
            '<img src="' + u + '"' + a + ' style="max-width:100%">');
          done();
        });
      });
    });
  };
  /* WebKit's own paste inserts images as blob: URLs — invisible to
     clipboardData.items, dead on the wire, and never downscaled. Convert
     every blob: image that appears, from any entry path, into the same
     scaled data: URI a handled paste produces. */
  function adoptBlobImage(img){
    fetch(img.src).then(function(r){ return r.blob(); }).then(function(b){
      var rd = new FileReader();
      rd.onload = function(){
        scaleUrl(String(rd.result), b.type, function(url){
          img.src = url;
          img.style.maxWidth = '100%';
        });
      };
      rd.readAsDataURL(b);
    }).catch(function(){});
  }
  new MutationObserver(function(muts){
    muts.forEach(function(m){
      Array.prototype.forEach.call(m.addedNodes || [], function(n){
        if(n.nodeType !== 1) return;
        var imgs = n.tagName === 'IMG' ? [n]
          : (n.querySelectorAll ? n.querySelectorAll('img') : []);
        Array.prototype.forEach.call(imgs, function(im){
          if(im.src && im.src.indexOf('blob:') === 0) adoptBlobImage(im);
        });
      });
    });
  }).observe(document.documentElement, {childList: true, subtree: true});
  var hl = window.__hylkiLines, LINE = hl.LINE, bare = hl.bare, quoted = hl.quoted,
    lineBlock = hl.lineBlock, blockOf = hl.blockOf, joinLines = hl.joinLines;
  /* WebKit only marks a word's spelling once the caret leaves it. The word
     under the caret is checked by us instead: after a typing pause it goes
     to the app (which asks the same enchant the body's checker uses), and a
     misspelling is drawn through the CSS Custom Highlight API — no DOM
     mutation, so the caret, the undo stack, and the outgoing HTML are all
     untouched. The mark clears the moment typing resumes or the caret
     leaves the word, where WebKit's own marking takes over. */
  var spellT = null, composing = false, spellRange = null;
  var spellHl = (window.Highlight && window.CSS && CSS.highlights)
    ? new Highlight() : null;
  if(spellHl) CSS.highlights.set('hylki-misspell', spellHl);
  function caretWord(){
    var sel = getSelection();
    if(!sel.rangeCount || !sel.isCollapsed) return null;
    var n = sel.focusNode, off = sel.focusOffset;
    if(!n || n.nodeType !== 3) return null;
    var s = n.textContent;
    var isW = function(c){ return /[\p{L}'’]/u.test(c); };
    var a = off; while(a > 0 && isW(s[a-1])) a--;
    var b = off; while(b < s.length && isW(s[b])) b++;
    if(b - a < 2) return null;
    var r = document.createRange();
    r.setStart(n, a); r.setEnd(n, b);
    return {range: r, word: s.slice(a, b)};
  }
  window.__hylkiSpellMark = function(bad){
    if(!spellHl) return;
    spellHl.clear();
    if(bad && spellRange && spellRange.startContainer.isConnected){
      spellHl.add(spellRange);
    }
  };
  /* Which blocks the caret sits in, for the toolbar toggles: `q` quote,
     `u` bulleted list, `o` numbered list. Posted only when it changes. */
  var lastFmt = null;
  function caretBlock(){
    var sel = getSelection();
    var n = sel.rangeCount ? sel.anchorNode : null;
    return n && n.nodeType === 3 ? n.parentNode : n;
  }
  /* WebKit's Indent wraps the paragraph in a blockquote of its own, with
     no border: an indent, not a quote, for the Quote toggle and for Enter
     twice (#358). */
  function isIndent(b){
    return !!(b.style && b.style.borderStyle === 'none' && b.style.marginLeft);
  }
  function quoteOf(el){
    var b = el && el.closest ? el.closest('blockquote') : null;
    while(b && isIndent(b)){
      b = b.parentElement ? b.parentElement.closest('blockquote') : null;
    }
    return b;
  }
  /* The element the font and size menus describe: the caret's, or with a
     selection the first selected character's. The anchor would not do: a
     font applied to a selection leaves it at the end of the text before
     the new span, and the menu went on naming the old font (#358). */
  function styledEl(){
    var sel = getSelection();
    if(sel.rangeCount && !sel.isCollapsed){
      var r = sel.getRangeAt(0), root = r.commonAncestorContainer;
      if(root.nodeType === 3) return root.parentNode;
      var w = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
      for(var t = w.nextNode(); t; t = w.nextNode()){
        if(!r.intersectsNode(t) || !t.nodeValue.trim()) continue;
        if(t === r.startContainer && r.startOffset >= t.length) continue;
        return t.parentNode;
      }
    }
    var el = caretBlock();
    return el && el.nodeType === 1 ? el : document.body;
  }
  /* The caret's font (#358): its first family, or the generic family the
     editor's own falls under. A font chosen for the next character typed
     comes first, then WebKit's own value, which the selection's anchor
     could not stand in for. */
  function fontOf(el){
    var g = pending && pending.fontFamily || '';
    if(!g) try{ g = String(document.queryCommandValue('fontName') || ''); }catch(_){}
    if(!g){ try{ g = getComputedStyle(el).fontFamily; }catch(_){} }
    var first = g.split(',')[0].replace(/["']/g, '').trim();
    if(first && !/^(system-ui|sans-serif|serif|monospace|ui-[a-z-]+|-webkit-[a-z-]+)$/i.test(first)) return first;
    g = g.toLowerCase();
    if(/mono/.test(g)) return 'monospace';
    if(/(^|,)\s*"?serif/.test(g) || (/serif/.test(g) && !/sans/.test(g))) return 'serif';
    return 'sans-serif';
  }
  /* The caret's text size in points, the unit the size menu shows: a size
     chosen for the next character typed, or the text's own. */
  function sizeOf(el){
    var want = pending && pending.fontSize || '';
    var px = 0;
    if(/pt$/.test(want)) return String(parseFloat(want));
    if(want) px = parseFloat(want);
    else try{ px = parseFloat(getComputedStyle(el).fontSize); }catch(_){}
    // A view not laid out yet reports nonsense; the label keeps what it had.
    return px >= 4 ? String(Math.round(px * 0.75)) : '';
  }
  function fmtState(){
    var el = caretBlock();
    var q = !!quoteOf(el);
    var ul = false, ol = false, block = '';
    try{
      ul = document.queryCommandState('insertUnorderedList');
      ol = document.queryCommandState('insertOrderedList');
      block = String(document.queryCommandValue('formatBlock') || '').toLowerCase();
    }catch(_){}
    var at = styledEl();
    var s = (q ? 'q' : '') + (ul ? 'u' : '') + (ol ? 'o' : '')
      + '|' + block + '|' + fontOf(at) + '|' + sizeOf(at);
    if(s === lastFmt) return;
    lastFmt = s;
    try{ window.webkit.messageHandlers.hylkiFormat.postMessage(s); }catch(_){}
  }
  document.addEventListener('selectionchange', fmtState);
  document.addEventListener('input', fmtState);
  /* The Quote button is a toggle (#137): inside a quote it steps the
     caret's paragraph out one level, the same move as Enter twice; anywhere
     else it quotes the paragraph. The list buttons toggle on their own. */
  window.__hylkiQuote = function(){
    var el = caretBlock();
    if(quoteOf(el)){
      document.execCommand('outdent');
    }else{
      document.execCommand('formatBlock', false, 'blockquote');
    }
    fmtState();
  };
  /* The format bar's menus take the focus while they are open, and the
     caret with it: kept when one opens, put back before its command. */
  var saved = null;
  window.__hylkiSave = function(){
    var sel = getSelection();
    saved = sel.rangeCount && document.body.contains(sel.anchorNode) ? sel.getRangeAt(0).cloneRange() : null;
  };
  window.__hylkiRestore = function(){
    if(!saved) return;
    var sel = getSelection();
    sel.removeAllRanges();
    sel.addRange(saved);
    saved = null;
  };
  /* Paragraph styles (#358): a heading, preformatted text or a plain
     paragraph. */
  window.__hylkiBlock = function(tag){
    document.execCommand('formatBlock', false, tag);
    fmtState();
  };
  /* Font and colors (#358) as inline styles, which every mail client
     keeps; WebKit's own markup is a <font> tag otherwise. Only for these
     commands: Bold and the rest stay the plain tags Markdown maps. */
  window.__hylkiStyled = function(cmd, value){
    var sel = getSelection();
    if(cmd === 'fontName' && sel.rangeCount && sel.isCollapsed){
      pend('fontFamily', value);
      return;
    }
    document.execCommand('styleWithCSS', false, true);
    document.execCommand(cmd, false, value);
    document.execCommand('styleWithCSS', false, false);
    fmtState();
  };
  /* A font or size chosen with nothing selected (#358) is for the next
     character typed, which opens a span of it, the way the other styles
     carry on from the caret. Not WebKit's own typing style, which any
     change of selection drops: picking a font in the font dialog takes the
     focus away from the document and back. */
  var pending = null;
  function pend(prop, value){
    pending = pending || {};
    pending[prop] = value;
    fmtState();
  }
  /* Font size (#358) in points. WebKit's own command knows only the seven
     <font size> steps, so the text is marked with the largest, which no
     message written here uses, and the mark turns into the size asked for
     (a <font> other mail kept as it was is left alone). 0 is the editor's
     own size. */
  function sizeValue(pt){
    return pt ? pt + 'pt' : getComputedStyle(document.body).fontSize;
  }
  function clearSizes(root){
    root.querySelectorAll('[style]').forEach(function(d){
      d.style.fontSize = '';
      if(!d.getAttribute('style')) d.removeAttribute('style');
    });
    root.querySelectorAll('font[size]').forEach(function(d){ d.removeAttribute('size'); });
  }
  window.__hylkiSize = function(pt){
    var sel = getSelection();
    if(!sel.rangeCount) return;
    var sizeWant = sizeValue(pt);
    if(sel.isCollapsed){
      pend('fontSize', sizeWant);
      return;
    }
    var before = new Set(document.body.querySelectorAll('font[size="7"]'));
    document.execCommand('styleWithCSS', false, false);
    document.execCommand('fontSize', false, '7');
    document.body.querySelectorAll('font[size="7"]').forEach(function(f){
      if(before.has(f)) return;
      clearSizes(f);
      f.removeAttribute('size');
      f.style.fontSize = sizeWant;
    });
    fmtState();
  };
  document.addEventListener('beforeinput', function(e){
    if(!pending || e.inputType !== 'insertText' || !e.data) return;
    var want = pending;
    pending = null;
    var sel = getSelection();
    if(!sel.rangeCount || !sel.isCollapsed) return;
    e.preventDefault();
    var span = document.createElement('span');
    if(want.fontFamily) span.style.fontFamily = want.fontFamily;
    if(want.fontSize) span.style.fontSize = want.fontSize;
    // A lone space at the end of a line would collapse, and the caret
    // with it out of the span.
    var text = document.createTextNode(e.data === ' ' ? '\u00a0' : e.data);
    span.appendChild(text);
    var r = sel.getRangeAt(0);
    r.insertNode(span);
    r.setStart(text, text.length);
    r.collapse(true);
    sel.removeAllRanges();
    sel.addRange(r);
    fmtState();
  });
  /* A font or size chosen with nothing selected is for the caret where
     it was. */
  function unpend(){
    if(!pending) return;
    pending = null;
    fmtState();
  }
  document.addEventListener('mousedown', unpend);
  document.addEventListener('keydown', function(e){
    if(e.key && e.key.indexOf('Arrow') === 0 || e.key === 'Home' || e.key === 'End') unpend();
  });
  /* Indent and outdent (#358). In a list they nest the item under the one
     above, or bring it back out. */
  window.__hylkiIndent = function(more){
    document.execCommand(more ? 'indent' : 'outdent');
    fmtState();
  };
  /* Return breaks the line, or with `__hylkiReturnParagraph` set starts a
     new paragraph, a <p> the recipient's client spaces as one; Shift+Return
     does the other. WebKit's own pair is the reverse of the default, and
     its paragraph is a copy of the block the caret is in, usually a
     spaceless <div>. Inside a list Return keeps making the next item.

     Enter twice inside a quote leaves it (#137), the way a list ends: the
     first Enter opens an empty line in the quote, the second takes that
     line out below it (WebKit's outdent drops one quote level for the
     caret's paragraph). Any other key in between makes the next Enter an
     ordinary one again. */
  function inlineNode(n){
    return n.nodeType !== 1
      || (!n.hasAttribute('data-hylki-ui') && getComputedStyle(n).display.indexOf('inline') === 0);
  }
  /* Whether `n` is on a line that is not a paragraph and is the only line
     of its block: text straight in the body, or a <div> with nothing of its
     own, the blank line a new message starts on when Return breaks lines.
     The line has no <br> in it, but for the one WebKit keeps at its end. */
  function loneLine(n){
    while(n && n.parentNode !== document.body) n = n.parentNode;
    if(!n) return false;
    var run, m;
    if(inlineNode(n)){
      run = [n];
      for(m = n.previousSibling; m && inlineNode(m); m = m.previousSibling) run.unshift(m);
      for(m = n.nextSibling; m && inlineNode(m); m = m.nextSibling) run.push(m);
    }else if(n.tagName === 'DIV' && bare(n)){
      /* The lines under it are its own. */
      if(n.nextElementSibling && n.nextElementSibling.hasAttribute(LINE)) return false;
      run = Array.prototype.slice.call(n.childNodes);
      if(!run.every(inlineNode)) return false;
    }else{
      return false;
    }
    for(var i = 0; i < run.length - 1; i++){
      if(run[i].nodeName === 'BR') return false;
    }
    return true;
  }
  var enterInQuote = false;
  if(window.__hylkiReturnParagraph){
    document.execCommand('defaultParagraphSeparator', false, 'p');
  }
  document.addEventListener('keydown', function(e){
    if(e.key !== 'Enter' || e.ctrlKey || e.altKey || e.metaKey || composing){
      enterInQuote = false;
      return;
    }
    var sel = getSelection();
    var n = sel.rangeCount ? sel.anchorNode : null;
    var el = n && n.nodeType === 3 ? n.parentNode : n;
    if(!el || !el.closest || !document.body.isContentEditable || el.closest('li')){
      enterInQuote = false;
      return;
    }
    var inQuote = !!quoteOf(el);
    if(!e.shiftKey && inQuote && enterInQuote && sel.isCollapsed){
      e.preventDefault();
      document.execCommand('outdent');
      enterInQuote = false;
      return;
    }
    enterInQuote = inQuote && !e.shiftKey;
    e.preventDefault();
    if(!window.__hylkiReturnParagraph === !e.shiftKey){
      if(!lineBlock(blockOf(n))){
        document.execCommand('insertLineBreak');
        return;
      }
      document.execCommand('insertParagraph');
      var l = blockOf(getSelection().anchorNode);
      if(l && /^(DIV|P)$/.test(l.tagName)){
        l.setAttribute(LINE, '');
        l.setAttribute('dir', 'auto');
      }
      return;
    }
    /* WebKit makes only the new block a paragraph, and the first line of
       a message stays loose text or a bare <div>: it would go out as no
       paragraph at all (#524). A single line becomes a <p> first. */
    if(loneLine(n)) document.execCommand('formatBlock', false, 'p');
    document.execCommand('insertParagraph');
    /* A paragraph split from a <div> is another <div>; it becomes a <p>
       so it carries a paragraph's space. Only a bare div: one with a
       class (the signature) or a style is somebody's markup. */
    var b = blockOf(getSelection().anchorNode);
    /* Split from a line, it is a copy of one, and starts a paragraph
       instead. */
    if(b) b.removeAttribute(LINE);
    if(b && b.tagName === 'DIV' && bare(b)){
      document.execCommand('formatBlock', false, 'p');
    }
  }, true);
  document.addEventListener('compositionstart', function(){ composing = true; });
  document.addEventListener('compositionend', function(){ composing = false; });
  document.addEventListener('input', function(){
    if(!spellHl) return;
    spellHl.clear();
    if(composing) return;
    clearTimeout(spellT);
    spellT = setTimeout(function(){
      if(composing) return;
      var w = caretWord();
      if(!w) return;
      spellRange = w.range;
      try{ window.webkit.messageHandlers.hylkiSpell.postMessage(w.word); }catch(_){}
    }, 600);
  });
  document.addEventListener('selectionchange', function(){
    if(!spellHl || !spellRange) return;
    var sel = getSelection();
    if(!sel.rangeCount) return;
    var r = sel.getRangeAt(0);
    if(!sel.isCollapsed || r.startContainer !== spellRange.startContainer
       || r.startOffset < spellRange.startOffset
       || r.startOffset > spellRange.endOffset){
      spellHl.clear();
      spellRange = null;
    }
  });
  /* Resizing a picture. The frame and its corner handles are ordinary
     elements in this contenteditable document — there is nowhere else to
     put them — so they are marked `data-hylki-ui` and stripped from
     everything the document hands back (see `__hylkiBodyHtml`). They are
     also contenteditable=false, so the caret cannot wander into them. */
  var rsBox = null, rsImg = null;
  /* Blue is an ordinary selection; red says the picture will be recut when
     the message is sent, which is the one thing here that cannot be undone.
     The same red the spelling underline and the tray dot use. */
  var RS_BLUE = '#3584e4', RS_RED = '#e01b24';
  function rsRemove(){ if(rsBox){ rsBox.remove(); rsBox = null; rsImg = null; } }
  function rsTint(){
    if(!rsBox || !rsImg) return;
    var c = rsImg.hasAttribute('data-hylki-fit') ? RS_RED : RS_BLUE;
    rsBox.style.outlineColor = c;
    Array.prototype.forEach.call(rsBox.children, function(h){ h.style.background = c; });
  }
  function rsPlace(){
    if(!rsBox || !rsImg) return;
    if(!rsImg.isConnected){ rsRemove(); return; }
    var r = rsImg.getBoundingClientRect();
    rsBox.style.left = (r.left + scrollX) + 'px';
    rsBox.style.top = (r.top + scrollY) + 'px';
    rsBox.style.width = r.width + 'px';
    rsBox.style.height = r.height + 'px';
  }
  /* A width in CSS *and* in the attribute: mail clients that ignore styles
     still honour width, and the height is left to follow so the aspect can
     never be lost. */
  function rsApply(img, w){
    img.style.width = w + 'px';
    img.style.height = 'auto';
    img.setAttribute('width', String(w));
    img.removeAttribute('height');
  }
  function rsDown(e){
    if(!rsImg) return;
    e.preventDefault(); e.stopPropagation();
    var handle = e.currentTarget, img = rsImg;
    var west = handle.dataset.corner[1] === 'w';
    var startX = e.clientX, startW = img.getBoundingClientRect().width;
    var maxW = document.body.clientWidth;
    function move(ev){
      var dx = ev.clientX - startX;
      var w = Math.round(west ? startW - dx : startW + dx);
      rsApply(img, Math.max(24, Math.min(w, maxW)));
      rsPlace();
    }
    function up(ev){
      handle.removeEventListener('pointermove', move);
      handle.removeEventListener('pointerup', up);
      try{ handle.releasePointerCapture(ev.pointerId); }catch(_){}
    }
    try{ handle.setPointerCapture(e.pointerId); }catch(_){}
    handle.addEventListener('pointermove', move);
    handle.addEventListener('pointerup', up);
  }
  function rsShow(img){
    rsRemove();
    rsImg = img;
    rsBox = document.createElement('div');
    rsBox.setAttribute('data-hylki-ui', 'resize');
    rsBox.setAttribute('contenteditable', 'false');
    rsBox.style.cssText = 'position:absolute;pointer-events:none;z-index:9;'
      + 'outline:1px solid #3584e4;';
    ['nw','ne','sw','se'].forEach(function(c){
      var h = document.createElement('div');
      h.dataset.corner = c;
      h.style.cssText = 'position:absolute;width:11px;height:11px;'
        + 'background:#3584e4;border:2px solid Canvas;border-radius:50%;'
        + 'pointer-events:auto;cursor:'
        + ((c === 'nw' || c === 'se') ? 'nwse-resize' : 'nesw-resize') + ';';
      h.style[c[0] === 'n' ? 'top' : 'bottom'] = '-6px';
      h.style[c[1] === 'w' ? 'left' : 'right'] = '-6px';
      h.addEventListener('pointerdown', rsDown);
      rsBox.appendChild(h);
    });
    document.body.appendChild(rsBox);
    rsPlace();
    rsTint();
  }
  /* The context menu's size entries. `original` drops the explicit width so
     the picture falls back to its natural size, still held to the writing
     width by the stylesheet. */
  window.__hylkiSetImageSize = function(kind){
    var img = window.__hylkiCtxImg || rsImg;
    if(!img) return;
    if(kind === 'original'){
      img.style.width = ''; img.style.height = '';
      img.removeAttribute('width'); img.removeAttribute('height');
    } else {
      var full = document.body.clientWidth;
      var f = kind === 'small' ? 0.25 : (kind === 'medium' ? 0.5 : 1);
      rsApply(img, Math.max(24, Math.round(full * f)));
    }
    if(img === rsImg) rsPlace(); else rsShow(img);
  };
  /* Resizing a picture changes how big it is drawn, not how many bytes it
     is: the full-resolution data: URI still goes out, and the recipient can
     still save the original. Arming this asks for the bytes to be recut to
     the size shown, once, as the message is sent — irreversible, so it is
     off until asked for and marked on the picture while it is on. */
  window.__hylkiToggleFit = function(hint){
    var img = window.__hylkiCtxImg || rsImg;
    if(!img) return;
    if(img.hasAttribute('data-hylki-fit')){
      img.removeAttribute('data-hylki-fit');
      img.removeAttribute('title');
    } else {
      img.setAttribute('data-hylki-fit', '1');
      img.title = hint;
    }
    if(img === rsImg) rsTint(); else rsShow(img);
  };
  /* Recut every armed picture to the width it is drawn at. drawImage on an
     already-loaded image and toDataURL are both synchronous, so the send
     path can do this and read the body in one go. Enlarged pictures are left
     alone: there are no extra pixels to be had. */
  function rsFlatten(){
    Array.prototype.forEach.call(
      document.querySelectorAll('img[data-hylki-fit]'), function(im){
        var w = Math.round(im.getBoundingClientRect().width);
        if(!w || !im.naturalWidth || w >= im.naturalWidth) return;
        try{
          var c = document.createElement('canvas');
          c.width = w;
          c.height = Math.max(1, Math.round(w * im.naturalHeight / im.naturalWidth));
          c.getContext('2d').drawImage(im, 0, 0, c.width, c.height);
          var m = /^data:(image\/[a-z+]+)/.exec(im.src);
          var type = m ? m[1] : 'image/png';
          im.src = type === 'image/jpeg'
            ? c.toDataURL('image/jpeg', 0.85) : c.toDataURL('image/png');
        }catch(_){}
      });
  }
  /* What the host reads instead of body.innerHTML: the same content with
     this script's own furniture taken out. The arming mark stays, so a
     draft reopened later still shows it and still honours it when sent. */
  window.__hylkiBodyHtml = function(){
    var c = document.body.cloneNode(true);
    Array.prototype.forEach.call(c.querySelectorAll('[data-hylki-ui]'),
      function(n){ n.remove(); });
    joinLines(c);
    Array.prototype.forEach.call(
      c.querySelectorAll(':scope>div[dir="auto"],:scope>p[dir="auto"],li[dir="auto"]'),
      function(n){ if(!quoted(n)) n.removeAttribute('dir'); });
    return c.innerHTML;
  };
  /* The plain-text rendering of the body, for the text/plain alternative
     and for a message composed as plain text (#180): quoted blocks carry
     "> " on every line, lists their dashes, links their address, and
     block boundaries become line breaks. innerText knew none of that. */
  function hylkiTxt(node, pre){
    if(node.nodeType === 3){
      var v = node.nodeValue;
      return pre ? v : v.replace(/[ \t\r\n]+/g, ' ');
    }
    if(node.nodeType !== 1) return '';
    if(node.hasAttribute('data-hylki-ui')) return '';
    var tag = node.tagName.toLowerCase();
    if(tag === 'br') return '\n';
    if(tag === 'img') return node.getAttribute('alt') || '';
    if(tag === 'style' || tag === 'script') return '';
    var s = '';
    /* A quoted plain-text part keeps its spaces under an inline pre-wrap
       (#436), as <pre> does. */
    var inPre = pre || tag === 'pre' || /^(pre|break-spaces)/.test(node.style.whiteSpace || '');
    for(var i = 0; i < node.childNodes.length; i++) s += hylkiTxt(node.childNodes[i], inPre);
    if(tag === 'a'){
      var h = node.getAttribute('href') || '';
      if(h && h !== s.trim() && h.indexOf('mailto:') !== 0) s += ' <' + h + '>';
    }
    if(tag === 'li') return '- ' + s.replace(/^[ \n]+/, '') + '\n';
    if(tag === 'hr') return '\n----\n';
    if(tag === 'blockquote'){
      var lines = s.replace(/^\n+|\n+$/g, '').split('\n');
      return '\n' + lines.map(function(l){ return '> ' + l; }).join('\n') + '\n';
    }
    if(/^(p|div|h[1-6]|pre|tr|ul|ol|table|section|article|header|footer)$/.test(tag)) return '\n' + s + '\n';
    return s;
  }
  /* Trailing blanks go, except the one in a signature's "-- " line, which
     is how a mail client tells the signature from the message. */
  function tidyTxt(t){
    return t.replace(/[ \t]+\n/g, function(m, at, all){
        var start = all.lastIndexOf('\n', at - 1) + 1;
        return m === ' \n' && at - start === 2 && all.slice(start, at) === '--' ? m : '\n';
      })
      .replace(/\n{3,}/g, '\n\n')
      .replace(/^\n+|\n+$/g, '');
  }
  /* What is sent has spaces where the editor keeps no-break spaces to
     show them: a run of them typed, or the indents of a plain-text quote. */
  window.__hylkiBodyText = function(){
    var c = document.body.cloneNode(true);
    joinLines(c);
    return tidyTxt(hylkiTxt(c, false).replace(/\u00a0/g, ' '));
  };
  /* Plain text (#424) shows the message as it is sent: the document is
     flattened to the text above, the signature and the "wrote:" line
     keeping their blocks (and classes) so that changing the From, adding a
     signature or a cloud link finds them as in rich text. A quote becomes
     its "> " lines. */
  function plainTextOf(nodes){
    var d = document.createElement('div');
    nodes.forEach(function(n){ d.appendChild(n.cloneNode(true)); });
    return tidyTxt(hylkiTxt(d, false));
  }
  /* A line's indent, its runs of spaces and its tabs (to the next stop of
     eight) as no-break spaces: in a block that wraps they would collapse
     to one, on screen and in the text read back to send (#436). */
  function keepSpaces(line){
    var out = '', col = 0;
    for(var i = 0; i < line.length; i++){
      var c = line.charAt(i);
      if(c === '\t'){
        var n = 8 - col % 8;
        for(var k = 0; k < n; k++) out += '\u00a0';
        col += n;
        continue;
      }
      if(c === ' ' && (i === 0 || /[ \t\u00a0]/.test(line.charAt(i - 1)))) c = '\u00a0';
      out += c;
      col++;
    }
    return out;
  }
  function plainBlock(tag, cls, text){
    var e = document.createElement(tag);
    if(cls) e.className = cls;
    e.innerHTML = text ? text.split('\n').map(function(l){ return esc(keepSpaces(l)); }).join('<br>') : '<br>';
    return e;
  }
  function markOf(n){
    if(n.nodeType !== 1) return '';
    if(n.classList.contains('hylki-sig')) return 'hylki-sig';
    if(n.classList.contains('hylki-quote-attr')) return 'hylki-quote-attr';
    return '';
  }
  /* Whether `el`'s content is text and nothing else: line breaks, blocks
     with no styling of their own, and (with `quotes`) quotes of the same,
     which plain text writes as "> " lines. */
  function plainOk(el, quotes){
    var keep = quotes ? /^(DIV|P|BR|SPAN|BLOCKQUOTE)$/ : /^(DIV|P|BR|SPAN)$/;
    var all = [el].concat(Array.prototype.slice.call(el.querySelectorAll('*')));
    for(var i = 0; i < all.length; i++){
      var n = all[i];
      if(n.closest('[data-hylki-ui]')) continue;
      if(n === document.body) continue;
      if(!keep.test(n.tagName)) return false;
      for(var j = 0; j < n.attributes.length; j++){
        var a = n.attributes[j].name;
        if(a === 'style'){ if(n.getAttribute('style').trim()) return false; }
        else if(!/^(class|dir|id|data-.+)$/.test(a)) return false;
      }
    }
    return true;
  }
  /* Whether switching to plain text takes anything away: styling,
     pictures, links, lists. A quote is not lost, only written out. */
  window.__hylkiPlainLoses = function(){ return !plainOk(document.body, true); };
  function toPlain(){
    var b = document.body, segs = [], run = [];
    var flush = function(){ segs.push(['div', '', plainTextOf(run)]); run = []; };
    Array.prototype.slice.call(b.childNodes).forEach(function(n){
      if(n.nodeType === 1 && n.hasAttribute('data-hylki-ui')) return;
      var mark = markOf(n);
      if(!mark){ run.push(n); return; }
      flush();
      segs.push([n.tagName === 'P' ? 'p' : 'div', mark, plainTextOf([n])]);
    });
    flush();
    var out = document.createDocumentFragment();
    /* The first block stays even when empty: it is the line a new
       message or a reply is typed on. */
    segs.forEach(function(g, i){
      if(g[2] || i === 0) out.appendChild(plainBlock(g[0], g[1], g[2]));
    });
    b.textContent = '';
    b.appendChild(out);
  }
  /* What the composer runs after putting HTML of its own into a plain
     text body (a signature, a cloud link): any block that is not plain
     text yet is made so. */
  window.__hylkiPlainFix = function(){
    if(!window.__hylkiPlain) return;
    Array.prototype.slice.call(document.body.children).forEach(function(n){
      if(n.hasAttribute('data-hylki-ui') || plainOk(n, false)) return;
      n.replaceWith(plainBlock(n.tagName === 'P' ? 'p' : 'div', markOf(n), plainTextOf([n])));
    });
  };
  if(window.__hylkiPlain){
    document.addEventListener('DOMContentLoaded', toPlain);
    /* Bold, italic and underline are WebKit's own keys, not the
       toolbar's: in plain text they would show what is never sent. */
    document.addEventListener('keydown', function(e){
      if((e.ctrlKey || e.metaKey) && !e.altKey && /^[biu]$/i.test(e.key)) e.preventDefault();
    }, true);
    document.addEventListener('beforeinput', function(e){
      if(/^format/.test(e.inputType || '')) e.preventDefault();
    }, true);
  }
  /* Sending, and only sending: recut first, then read with the marks taken
     out. A draft is saved through the plain reader above, so quality is
     never lost to a save. */
  window.__hylkiBodyHtmlForSend = function(){
    rsFlatten();
    Array.prototype.forEach.call(
      document.querySelectorAll('img[data-hylki-fit]'), function(n){
        n.removeAttribute('data-hylki-fit'); n.removeAttribute('title');
      });
    rsTint();
    return fontRuns(window.__hylkiBodyHtml());
  };
  /* The font set for new messages, written around each stretch of what
     was typed, since the editor's style sheet does not go out with it. The
     signature and the quoted original (its attribution line and the quote
     straight after) are left out, as the editor shows them. */
  function fontRuns(html){
    var style = window.__hylkiComposeFont;
    if(!style) return html;
    var t = document.createElement('template');
    t.innerHTML = html;
    var kids = Array.prototype.slice.call(t.content.childNodes);
    var kept = kids.map(function(n){
      if(n.nodeType !== 1) return false;
      if(n.classList.contains('hylki-sig') || n.classList.contains('hylki-quote-attr')) return true;
      var prev = n.previousElementSibling;
      return n.tagName === 'BLOCKQUOTE' && !!prev && prev.classList.contains('hylki-quote-attr');
    });
    var run = [];
    function flush(){
      var typed = run.some(function(n){ return n.nodeType === 1 || /\S/.test(n.nodeValue || ''); });
      if(typed){
        var d = document.createElement('div');
        d.setAttribute('style', style);
        run[0].parentNode.insertBefore(d, run[0]);
        run.forEach(function(n){ d.appendChild(n); });
      }
      run = [];
    }
    kids.forEach(function(n, i){ if(kept[i]) flush(); else run.push(n); });
    flush();
    return t.innerHTML;
  }
  addEventListener('scroll', rsPlace, true);
  addEventListener('resize', rsPlace);
  document.addEventListener('input', rsPlace);
  /* Clicking an image selects it whole, so it can be deleted, cut, or
     copied like any other selection, and raises its resize frame. */
  document.addEventListener('click', function(e){
    var t = e.target;
    if(rsBox && rsBox.contains(t)) return;   // a handle, not the document
    if(t && t.tagName === 'IMG'){
      var r = document.createRange(); r.selectNode(t);
      var s = getSelection(); s.removeAllRanges(); s.addRange(r);
      rsShow(t);
    } else {
      rsRemove();
    }
  });
  /* A right-click on an image selects it (like a left click) and remembers
     the exact node: the menu's stock Cut/Copy then act on the image itself,
     and Send as Attachment Instead knows which one to lift. Captured on
     button-2 mousedown as well as contextmenu — whichever of the two WebKit
     actually delivers before it opens the menu. */
  function rememberImg(e){
    var t = e.target;
    if(t && t.tagName === 'IMG'){
      window.__hylkiCtxImg = t;
      var r = document.createRange(); r.selectNode(t);
      var s = getSelection(); s.removeAllRanges(); s.addRange(r);
      rsShow(t);
    } else if(e.type === 'contextmenu'){
      window.__hylkiCtxImg = null;
    }
  }
  document.addEventListener('mousedown', function(e){ if(e.button === 2) rememberImg(e); }, true);
  document.addEventListener('contextmenu', rememberImg, true);
  document.addEventListener('dragover', function(e){
    var it = (e.dataTransfer && e.dataTransfer.items) || [];
    for(var i = 0; i < it.length; i++){
      if(it[i].kind === 'file'){ e.preventDefault(); return; }
    }
  });
  /* Files from a file manager are taken by the widget's drop target before
     this runs — WebKitGTK leaves `dataTransfer.files` empty for them. What
     reaches here is a drag that really does carry file data, such as an
     image dragged out of another page. */
  document.addEventListener('drop', function(e){
    var fs = (e.dataTransfer && e.dataTransfer.files) || [];
    var imgs = [];
    for(var i = 0; i < fs.length; i++){ if(/^image\//.test(fs[i].type)) imgs.push(fs[i]); }
    if(!imgs.length) return;
    e.preventDefault();
    if(document.caretRangeFromPoint){
      var r = document.caretRangeFromPoint(e.clientX, e.clientY);
      if(r){ var sel = getSelection(); sel.removeAllRanges(); sel.addRange(r); }
    }
    imgs.forEach(insertImageFile);
  });
  function insertLinkified(text){
    urlRe.lastIndex = 0;
    var out='', last=0, m;
    while((m = urlRe.exec(text)) !== null){
      out += esc(text.slice(last, m.index));
      var url = m[0];
      var href = /^www\./i.test(url) ? 'http://'+url : url;
      out += '<a href="'+esc(href)+'">'+esc(url)+'</a>';
      last = m.index + url.length;
    }
    out += esc(text.slice(last));
    out = out.replace(/\r\n|\r|\n/g,'<br>');
    document.execCommand('insertHTML', false, out);
  }
  document.addEventListener('paste', function(e){
    var cd = e.clipboardData; if(!cd) return;
    /* Plain text takes the clipboard's text and nothing else: no
       formatting, no picture, no link made of an address. */
    if(window.__hylkiPlain){
      window.__hylkiPasteOnce = null;
      e.preventDefault();
      var t = cd.getData('text/plain');
      if(t) document.execCommand('insertHTML', false, esc(t).replace(/\r\n|\r|\n/g, '<br>'));
      return;
    }
    /* An image on the clipboard beats any text flavour riding along with it,
       and pastes inline whichever paste mode is set. */
    var its = cd.items || [];
    for(var i = 0; i < its.length; i++){
      if(its[i].kind === 'file' && /^image\//.test(its[i].type)){
        e.preventDefault();
        insertImageFile(its[i].getAsFile());
        return;
      }
    }
    var rich = window.__hylkiPasteRich === true;
    if(window.__hylkiPasteOnce !== null){ rich = window.__hylkiPasteOnce; window.__hylkiPasteOnce = null; }
    var html = cd.getData('text/html');
    var text = cd.getData('text/plain');
    if(rich){
      if(html) return;                       // native paste keeps the formatting
      if(!text) return;
      urlRe.lastIndex = 0;
      if(!urlRe.test(text)) return;          // plain text with nothing to linkify
    } else {
      if(!text) return;                      // no text flavour; let WebKit try
    }
    e.preventDefault();
    insertLinkified(text);
  });
})();
