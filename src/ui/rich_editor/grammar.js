(function(){
  /* Grammar and style marks (#452). After a pause in typing, the text the
     user wrote (not the quote, not the signature) goes to the app as one
     string, paragraphs apart by a blank line, and the problems the server
     finds come back as offsets into it. They are drawn through the CSS
     Custom Highlight API, so nothing in the document, its undo history or
     the sent message changes. The app turns this on only in the composer:
     `__hylkiGrammarOn`. */
  if(!(window.Highlight && window.CSS && CSS.highlights)) return;
  var on = false, timer = null, seq = 0, asked = null, shown = null,
    composing = false, ids = 0, issues = [];
  var mistakes = new Highlight(), style = new Highlight();
  CSS.highlights.set('hylki-grammar', mistakes);
  CSS.highlights.set('hylki-style', style);
  var BLOCK = /^(P|DIV|LI|UL|OL|H[1-6]|BLOCKQUOTE|PRE|TABLE|TR|TD|TH|HR|DL|DT|DD)$/;
  /* WebKit's Indent is a blockquote with no border: the user's own text. */
  function isIndent(b){
    return !!(b.style && b.style.borderStyle === 'none' && b.style.marginLeft);
  }
  function skipped(el){
    if(el.tagName === 'BLOCKQUOTE') return !isIndent(el);
    return /^(IMG|SCRIPT|STYLE)$/.test(el.tagName)
      || el.matches('.hylki-quote-attr,.hylki-sig,[data-hylki-ui]');
  }
  /* The text and, for each text node in it, where it starts. Line breaks
     inside a text node read as spaces, as they show: the same length, so
     offsets still match the node's own. */
  function collect(){
    var text = '', segs = [];
    function brk(n){
      if(!text) return;
      var have = 0;
      for(var k = text.length - 1; k >= 0 && text[k] === '\n' && have < n; k--) have++;
      while(have++ < n) text += '\n';
    }
    function walk(node){
      for(var c = node.firstChild; c; c = c.nextSibling){
        if(c.nodeType === 3){
          if(c.nodeValue.length){
            segs.push([c, text.length]);
            text += c.nodeValue.replace(/[\n\r\t]/g, ' ');
          }
        } else if(c.nodeType === 1){
          var block = BLOCK.test(c.tagName);
          /* A line of a paragraph (#524) is one line of it. */
          var gap = c.hasAttribute('data-hylki-line') ? 1 : 2;
          if(skipped(c)){ if(block) brk(2); continue; }
          if(c.tagName === 'BR'){ text += '\n'; continue; }
          if(block) brk(gap);
          walk(c);
          if(block) brk(2);
        }
      }
    }
    if(document.body) walk(document.body);
    return {text: text, segs: segs};
  }
  /* The node and offset at a place in the text: the start of what follows
     it, or for an end, the end of what comes before. */
  function at(segs, off, end){
    for(var k = 0; k < segs.length; k++){
      var n = segs[k][0], s = segs[k][1], len = n.nodeValue.length;
      if(end ? off <= s + len : off < s + len){
        return [n, Math.max(0, Math.min(len, off - s))];
      }
    }
    return null;
  }
  function drop(test){
    issues = issues.filter(function(e){
      if(!test(e)) return true;
      mistakes.delete(e.r); style.delete(e.r);
      return false;
    });
  }
  function clear(){ mistakes.clear(); style.clear(); issues = []; }
  function send(){
    timer = null;
    if(!on) return;
    if(composing){ schedule(800); return; }
    var now = collect().text;
    if(now === shown || now === asked) return;
    seq++;
    if(!now.trim()){ clear(); shown = now; asked = null; return; }
    asked = now;
    try{
      window.webkit.messageHandlers.hylkiGrammar.postMessage(
        JSON.stringify({seq: seq, text: now}));
    }catch(_){}
  }
  function schedule(ms){
    clearTimeout(timer);
    timer = setTimeout(send, ms);
  }
  window.__hylkiGrammarOn = function(){
    on = true;
    schedule(400);
  };
  /* The marks taken away, and any answer on its way dropped: the next
     check starts afresh. */
  window.__hylkiGrammarClear = function(){
    clear();
    shown = asked = null;
  };
  /* The problems found in request `n`, if its text is still the text.
     Otherwise a newer check is on its way. */
  window.__hylkiGrammarShow = function(n, list){
    if(n !== seq) return;
    var now = collect();
    if(now.text !== asked) return;
    asked = null;
    clear();
    shown = now.text;
    list.forEach(function(it){
      var a = at(now.segs, it.offset, false),
        z = at(now.segs, it.offset + it.length, true);
      if(!a || !z) return;
      var r = document.createRange();
      try{ r.setStart(a[0], a[1]); r.setEnd(z[0], z[1]); }catch(_){ return; }
      if(r.collapsed) return;
      issues.push({id: ++ids, r: r, it: it, text: r.toString()});
      (it.style ? style : mistakes).add(r);
    });
  };
  /* An edit takes away the marks it touches, and the text is checked
     again once typing stops. */
  document.addEventListener('compositionstart', function(){ composing = true; });
  document.addEventListener('compositionend', function(){ composing = false; });
  document.addEventListener('input', function(ev){
    /* An input method can start a composition and never say it ended; the
       next keystroke that is not part of one says it for it. */
    composing = !!ev.isComposing;
    if(!on) return;
    var sel = getSelection();
    var c = sel.rangeCount ? sel.getRangeAt(0) : null;
    drop(function(e){
      return !e.r.startContainer.isConnected || e.r.collapsed
        || e.r.toString() !== e.text
        || (c && c.startContainer.isConnected
            && e.r.comparePoint(c.startContainer, c.startOffset) === 0);
    });
    shown = null;
    schedule(1200);
  });
  /* A right-click on a mark tells the app which problem it is on, for the
     menu it is about to build; anywhere else, that it is on none. */
  document.addEventListener('contextmenu', function(ev){
    if(!on) return;
    var hit = null;
    for(var k = 0; k < issues.length && !hit; k++){
      var e = issues[k];
      if(!e.r.startContainer.isConnected) continue;
      var rects = e.r.getClientRects();
      for(var j = 0; j < rects.length; j++){
        var b = rects[j];
        if(ev.clientX >= b.left - 1 && ev.clientX <= b.right + 1
           && ev.clientY >= b.top - 1 && ev.clientY <= b.bottom + 1){
          hit = {id: e.id, message: e.it.message, replacements: e.it.replacements,
            rule: e.it.rule, text: e.text};
          break;
        }
      }
    }
    try{
      window.webkit.messageHandlers.hylkiGrammarHit.postMessage(JSON.stringify(hit));
    }catch(_){}
  }, true);
  function find(id){
    for(var k = 0; k < issues.length; k++) if(issues[k].id === id) return issues[k];
    return null;
  }
  /* Put a suggestion in place of the problem, as typing it would: one
     step of the undo history. */
  window.__hylkiGrammarFix = function(id, replacement){
    var e = find(id);
    if(!e || !e.r.startContainer.isConnected || e.r.toString() !== e.text) return;
    var s = getSelection();
    s.removeAllRanges();
    s.addRange(e.r.cloneRange());
    if(replacement === '') document.execCommand('delete');
    else document.execCommand('insertText', false, replacement);
  };
  /* The problem, and every other like it, left alone. */
  window.__hylkiGrammarIgnore = function(rule, text){
    drop(function(e){ return e.it.rule === rule && e.text === text; });
  };
})();
