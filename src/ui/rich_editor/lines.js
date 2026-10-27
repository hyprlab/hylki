
(function(){
  /* Each paragraph is laid out in the direction of its own text (#366),
     but WebKit draws the caret by the direction of the block it is in:
     at the end of an English paragraph under a Hebrew one, the caret
     stayed at the left edge (#524). So the blocks the style sheet lays out
     that way say dir="auto" while the message is written. What the
     document hands back has it taken out again: the sent message gets a
     direction written for it (crate::mail::direction). */
  var AUTO_DIR = 'body>div,body>p,li';
  /* Not in the message a reply or a forward quotes: it is somebody
     else's, and goes out as it came. */
  function quoted(n){ return n.matches('.hylki-quote-attr+blockquote *'); }
  function autoDir(){
    if(!document.body) return;
    Array.prototype.forEach.call(document.querySelectorAll(AUTO_DIR), function(n){
      if(!n.hasAttribute('dir') && !n.hasAttribute('data-hylki-ui') && !quoted(n)) n.setAttribute('dir', 'auto');
    });
  }
  new MutationObserver(autoDir)
    .observe(document.documentElement, {childList: true, subtree: true});
  /* A block with nothing of its own but that. */
  function bare(el){
    return !el.attributes.length
      || (el.attributes.length === 1 && el.getAttribute('dir') === 'auto');
  }
  /* A line started in a plain <div> or <p> of the body is a block of its
     own, a copy of that one, while the message is written: it then has a
     direction of its own, and the caret is drawn by it, where a <br> line
     takes its block's (#524). The style sheet takes the paragraph space
     out from between the lines of a <p>. What the document hands back
     joins each line to the block above with a <br>, the line it has always
     been, so drafts, the text part and the other formats see no
     difference. */
  var LINE = 'data-hylki-line';
  function lineBlock(el){
    if(!el || !/^(DIV|P)$/.test(el.tagName) || el.parentNode !== document.body) return false;
    for(var i = 0; i < el.attributes.length; i++){
      var a = el.attributes[i];
      if(a.name !== LINE && !(a.name === 'dir' && a.value === 'auto')) return false;
    }
    return true;
  }
  function blockOf(n){
    var el = n && (n.nodeType === 3 ? n.parentNode : n);
    return el && el.closest ? el.closest('p,div,li,blockquote,h1,h2,h3,h4,h5,h6,pre') : null;
  }
  /* Last first, so a run of lines folds up into the block it began in. A
     line with no block of its kind above it any more stays one of its own. */
  function joinLines(root){
    var lines = root.querySelectorAll('[' + LINE + ']');
    for(var i = lines.length - 1; i >= 0; i--){
      var l = lines[i], prev = l.previousSibling;
      while(prev && prev.nodeType === 3 && !prev.nodeValue.trim()) prev = prev.previousSibling;
      l.removeAttribute(LINE);
      if(!prev || prev.nodeType !== 1 || prev.tagName !== l.tagName) continue;
      /* A block's last <br> holds an empty line open, or ends the line. */
      if(!prev.lastChild || prev.lastChild.nodeName !== 'BR'){
        prev.appendChild(document.createElement('br'));
      }
      while(l.firstChild) prev.appendChild(l.firstChild);
      l.remove();
    }
  }
  /* A body loaded from HTML (a draft, a template, another format) has its
     lines as <br>s, the way they were saved. Each becomes a line block,
     as if it had been typed, so the caret on it is drawn by its own
     direction too. Only in a block holding nothing but inline content:
     a block inside is somebody's layout. A last <br> only ends the line
     before it. Not in plain text, where every block is a paragraph. */
  var BLOCKISH = /^(ADDRESS|ARTICLE|ASIDE|BLOCKQUOTE|DD|DETAILS|DIV|DL|DT|FIELDSET|FIGURE|FOOTER|FORM|H[1-6]|HEADER|HR|LI|MAIN|NAV|OL|P|PRE|SECTION|TABLE|UL)$/;
  function filled(n){
    return n.nodeType === 1 || (n.nodeType === 3 && /[^ \t\n\r\f]/.test(n.nodeValue));
  }
  function splitLines(){
    Array.prototype.slice.call(document.body.children).forEach(function(b){
      if(!/^(DIV|P)$/.test(b.tagName) || !bare(b)) return;
      var kids = Array.prototype.slice.call(b.childNodes), lines = [[]];
      if(kids.some(function(n){ return n.nodeType === 1 && BLOCKISH.test(n.tagName); })) return;
      kids.forEach(function(n){
        if(n.nodeName === 'BR') lines.push([]);
        else lines[lines.length - 1].push(n);
      });
      if(!lines[lines.length - 1].some(filled)) lines.pop();
      if(lines.length < 2) return;
      var at = b.nextSibling;
      b.textContent = '';
      lines.forEach(function(nodes, i){
        var el = b;
        if(i){
          el = document.createElement(b.tagName);
          el.setAttribute(LINE, '');
          el.setAttribute('dir', 'auto');
          b.parentNode.insertBefore(el, at);
        }
        if(nodes.some(filled)) nodes.forEach(function(n){ el.appendChild(n); });
        else el.appendChild(document.createElement('br'));
      });
    });
  }
  if(!window.__hylkiPlain) document.addEventListener('DOMContentLoaded', splitLines);
  window.__hylkiLines = {LINE: LINE, bare: bare, quoted: quoted, lineBlock: lineBlock,
    blockOf: blockOf, joinLines: joinLines};
})();
