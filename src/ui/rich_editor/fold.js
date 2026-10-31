(function(){
  /* The quoted original of a reply or forward, folded (#549): the reading
     pane beside the composer already shows it, and it is sent whether it
     is shown or not. The button is furniture, like the resize frame:
     `data-hylki-ui`, so nothing the document hands back carries it, and
     contenteditable=false, so the caret passes over it. Folded, it stands
     where the quote was, right after it, and the stylesheet hides the two
     blocks before it (the "wrote:" line and the quote) while <html> has
     `hylki-folded`, a class outside the body the message is read from.
     Open, it moves above the "wrote:" line, where the reader puts its own
     button, so a long quote folds again from its top. Runs after the plain
     view has flattened the body, where the quote is the block after its
     "wrote:" line. */
  if(!window.__hylkiFoldQuote) return;
  document.addEventListener('DOMContentLoaded', function(){
    var b = document.body, attr = null;
    for(var n = b.firstElementChild; n; n = n.nextElementSibling){
      if(n.classList.contains('hylki-quote-attr')){ attr = n; break; }
    }
    var quote = attr && attr.nextElementSibling;
    if(!quote || quote.hasAttribute('data-hylki-ui')) return;
    var root = document.documentElement;
    var tips = window.__hylkiFoldTips || ['', ''];
    var btn = document.createElement('div');
    btn.setAttribute('data-hylki-ui', 'fold');
    btn.setAttribute('contenteditable', 'false');
    function folded(){ return root.classList.contains('hylki-folded'); }
    /* `move` puts the button where it belongs for `on`. An edit that
       unfolds leaves it be: moving it inside the selection being edited
       shifts the selection. */
    function fold(on, move){
      root.classList.toggle('hylki-folded', on);
      btn.title = tips[on ? 0 : 1];
      if(move){ if(on) quote.after(btn); else attr.before(btn); }
    }
    /* On the press, not the click: the press is also what would move the
       caret, and the caret stays where the writing is. */
    btn.addEventListener('mousedown', function(e){
      e.preventDefault();
      if(e.button === 0) fold(!folded(), true);
    });
    fold(true, true);
    /* An edit that would reach into the hidden quote (Select All and
       Delete, Delete at the end of the line above it) unfolds it instead,
       so nothing is taken away unseen. The next one goes through. */
    function reaches(r){
      var a = document.createRange();
      try{ a.setStart(r.startContainer, r.startOffset); a.setEnd(r.endContainer, r.endOffset); }
      catch(_){ return false; }
      return a.intersectsNode(attr) || a.intersectsNode(quote);
    }
    document.addEventListener('beforeinput', function(e){
      if(!folded() || !/^(insert|delete)/.test(e.inputType || '')) return;
      var rs = e.getTargetRanges ? e.getTargetRanges() : [];
      if(!rs.length){
        var s = getSelection();
        rs = s.rangeCount ? [s.getRangeAt(0)] : [];
      }
      if(!rs.some(reaches)) return;
      e.preventDefault();
      fold(false);
    }, true);
    /* With the quote or the button edited away, or the quote no longer
       right under its "wrote:" line, there is nothing left to fold. */
    document.addEventListener('input', function(){
      if(btn.isConnected && attr.isConnected && attr.nextElementSibling === quote) return;
      root.classList.remove('hylki-folded');
      btn.remove();
    });
  });
})();
