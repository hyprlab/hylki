(function(){
  /* Faint guide lines on the cells of the table being written in, gone again
     once the caret leaves it or the editor loses the focus. A table pasted
     from a spreadsheet often has no borders of its own (#623), so without
     them its cells cannot be told apart. The lines come from a stylesheet in
     <head> that names the table by its place in the body: nothing is written
     into the message, the undo history or the draft. */
  if(window.__hylkiPlain) return;
  var sheet = document.createElement('style');
  document.head.appendChild(sheet);
  var C = 'rgba(128,128,128,0.45)';
  var current = '';
  function path(el){
    var parts = [];
    while(el && el !== document.body){
      var p = el.parentElement; if(!p) return '';
      parts.unshift(':nth-child(' + (Array.prototype.indexOf.call(p.children, el) + 1) + ')');
      el = p;
    }
    return el ? 'body>' + parts.join('>') : '';
  }
  function tableAt(n){
    var e = n && (n.nodeType === 1 ? n : n.parentElement);
    var t = e && e.closest ? e.closest('table') : null;
    return t && document.body.contains(t) && t !== document.body ? t : null;
  }
  /* Each cell draws its right and bottom line, the first row its top and the
     first column its left, so neighbouring cells never double a line. Inset
     shadows take no room, so the table does not move when they appear. */
  /* A cell with lines of its own (a spreadsheet's borders) is drawn as
     it will be sent: a guide beside a black line would double it. */
  var BARE = ':not([style*=border])';
  function rules(t){
    /* The first row group, not the first child: a table from Calc opens
       with a <colgroup>. */
    var g = t + '>:nth-child(1 of thead,tbody,tfoot)';
    var cell = [t + '>tr>:is(td,th)' + BARE, t + '>*>tr>:is(td,th)' + BARE];
    var top = [t + '>tr:first-of-type>*' + BARE, g + '>tr:first-child>*' + BARE];
    var left = [t + '>tr>:first-child' + BARE, t + '>*>tr>:first-child' + BARE];
    var corner = [t + '>tr:first-of-type>:first-child' + BARE, g + '>tr:first-child>:first-child' + BARE];
    var r = 'inset -1px -1px 0 ' + C, tp = 'inset 0 1px 0 ' + C, lf = 'inset 1px 0 0 ' + C;
    return cell.join(',') + '{box-shadow:' + r + '}' +
      top.join(',') + '{box-shadow:' + r + ',' + tp + '}' +
      left.join(',') + '{box-shadow:' + r + ',' + lf + '}' +
      corner.join(',') + '{box-shadow:' + r + ',' + tp + ',' + lf + '}';
  }
  function update(){
    var sel = document.getSelection();
    var t = document.hasFocus() && sel && sel.rangeCount
      ? (tableAt(sel.anchorNode) || tableAt(sel.focusNode)) : null;
    var p = t ? path(t) : '';
    if(p === current) return;
    current = p;
    sheet.textContent = p ? rules(p) : '';
  }
  document.addEventListener('selectionchange', update);
  /* Typing can move the table (a line added above it) with the caret still
     inside, which leaves the selection where it was. */
  document.addEventListener('input', update);
  addEventListener('focus', update);
  addEventListener('blur', update);
})();
