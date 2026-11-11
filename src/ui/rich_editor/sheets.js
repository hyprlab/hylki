(function(){
  /* Cells copied from a spreadsheet (#623) come as an HTML table whose look
     is split between attributes on the cells and a stylesheet in <head>:
     Calc names its font and size only there. WebKit's own paste keeps the
     table and drops the stylesheet, and the cells took the editor's font
     and line height. Here the stylesheet is written onto the elements it
     styles, as a mail client needs it anyway, and the table goes in looking
     as it does in any other client.

     `doc` is the clipboard's HTML parsed by DOMParser: an inert document,
     where nothing runs or loads. What comes back out is only the table's
     own markup, through the lists below. */
  var KEEP = /^(TABLE|CAPTION|COLGROUP|COL|THEAD|TBODY|TFOOT|TR|TD|TH|B|STRONG|I|EM|U|S|STRIKE|DEL|INS|SUB|SUP|FONT|SPAN|BR|P|DIV|A|SMALL|BIG|CENTER)$/;
  /* Gone with what is inside them. A cell's note is one: Calc hides it
     behind a red mark, and in a message there is nothing to open it. */
  var DROP = /^(SCRIPT|STYLE|TITLE|META|LINK|BASE|IMG|PICTURE|SVG|MATH|OBJECT|EMBED|IFRAME|FRAME|VIDEO|AUDIO|SOURCE|CANVAS|FORM|INPUT|BUTTON|SELECT|TEXTAREA|TEMPLATE|NOSCRIPT|COMMENT)$/;
  /* A link's address is checked on its own, in prune(). */
  var ATTRS = /^(style|align|valign|bgcolor|width|height|colspan|rowspan|span|border|cellspacing|cellpadding|color|face|size|dir|lang)$/;
  /* What a cell passes on to what is inside it, so the same font need not
     be written on the table, its rows and every cell. */
  var INHERITED = /^(font-|color$|line-height$|letter-spacing$|word-spacing$|text-align$|text-indent$|text-transform$|white-space|direction$)/;

  function topTables(root){
    return Array.prototype.filter.call(root.getElementsByTagName('table'), function(t){
      return !t.parentElement.closest('table');
    });
  }
  /* Rough specificity, which is all a spreadsheet's stylesheet needs:
     Excel's classes against its td rule. */
  function specificity(sel){
    var s = sel.replace(/\((?:[^()]|\([^()]*\))*\)/g, '');
    var ids = (s.match(/#[\w-]+/g) || []).length;
    var cls = (s.match(/\.[\w-]+|\[[^\]]*\]|:(?!:)[\w-]+/g) || []).length;
    var tags = (s.replace(/#[\w-]+|\.[\w-]+|\[[^\]]*\]|::?[\w-]+/g, ' ').match(/[a-zA-Z][\w-]*/g) || []).length;
    return ids * 10000 + cls * 100 + tags;
  }
  function splitSelectors(text){
    var out = [], depth = 0, cur = '';
    for(var i = 0; i < text.length; i++){
      var c = text.charAt(i);
      if(c === '(') depth++;
      else if(c === ')') depth--;
      if(c === ',' && !depth){ out.push(cur.trim()); cur = ''; continue; }
      cur += c;
    }
    if(cur.trim()) out.push(cur.trim());
    return out;
  }
  /* The style rules of the document's sheets, each selector of a list on
     its own. A constructed sheet ignores @import, so nothing is fetched. */
  function rulesOf(doc){
    var out = [];
    function collect(list){
      Array.prototype.forEach.call(list, function(r){
        if(r.type !== 1) return;
        splitSelectors(r.selectorText).forEach(function(sel){
          if(/::/.test(sel)) return;
          out.push({sel: sel, spec: specificity(sel), style: r.style});
        });
      });
    }
    Array.prototype.forEach.call(doc.querySelectorAll('style'), function(st){
      try{
        var sheet = new CSSStyleSheet();
        sheet.replaceSync(st.textContent);
        collect(sheet.cssRules);
      }catch(_){}
    });
    return out;
  }
  /* A property worth keeping: nothing that loads (a background picture
     would be a remote fetch from the composer) and nothing of Office's. */
  function keepProp(name, value){
    return !/^-|^mso-|^(caret-color|orphans|widows)$/.test(name)
      && !/url\(|expression\(/i.test(value);
  }
  /* WebKit hands a page's HTML over with the computed values of the
     document it was read in written onto the outermost element: the
     initial values of a page with no style, black text among them. They
     say nothing the recipient's own page does not, and black would stay
     black on a dark one. */
  var PLAIN = {
    'color': /^(rgb\(0, 0, 0\)|#000000|black)$/i,
    'font-style': /^normal$/, 'font-variant-caps': /^normal$/,
    'font-weight': /^(400|normal)$/, 'letter-spacing': /^normal$/,
    'word-spacing': /^0(px)?$/, 'text-align': /^start$/,
    'text-transform': /^none$/, 'text-indent': /^0(px)?$/,
    'white-space': /^normal$/, 'text-decoration': /^none$/
  };
  function plainValue(name, value){
    return !!PLAIN[name] && PLAIN[name].test(value);
  }
  /* The declarations that apply to `el`, from the sheet and then its own
     style attribute, as {name: [value, important]}. */
  function cascade(el, rules){
    var won = {}, rank = {};
    rules.forEach(function(r){
      var hit = false;
      try{ hit = el.matches(r.sel); }catch(_){}
      if(!hit) return;
      for(var i = 0; i < r.style.length; i++){
        var name = r.style[i], imp = r.style.getPropertyPriority(name) === 'important';
        var key = (imp ? 1e9 : 0) + r.spec;
        if(rank[name] !== undefined && rank[name] > key) continue;
        rank[name] = key;
        won[name] = [r.style.getPropertyValue(name), imp];
      }
    });
    var own = el.style;
    for(var i = 0; i < own.length; i++){
      var name = own[i], imp = own.getPropertyPriority(name) === 'important';
      if(won[name] && won[name][1] && !imp) continue;
      won[name] = [own.getPropertyValue(name), imp];
    }
    return won;
  }
  function inheritedOf(decls){
    var out = {};
    Object.keys(decls).forEach(function(n){ if(INHERITED.test(n)) out[n] = decls[n][0]; });
    return out;
  }
  /* A double line needs three pixels to be drawn as two; Calc writes the
     one it draws as two lines with a 2px width, which every engine paints
     as a single one. */
  function widenDouble(style){
    ['top', 'right', 'bottom', 'left'].forEach(function(side){
      if(style.getPropertyValue('border-' + side + '-style') !== 'double') return;
      if(parseFloat(style.getPropertyValue('border-' + side + '-width')) < 3){
        style.setProperty('border-' + side + '-width', '3px');
      }
    });
  }
  /* `base` is what the page around the table gave it, its font above
     all, under the table's own. */
  function clean(el, rules, parentInh, base){
    var decls = cascade(el, rules);
    Object.keys(base || {}).forEach(function(n){
      if(!decls[n]) decls[n] = [base[n], false];
    });
    var mine = inheritedOf(decls);
    /* What an ancestor already passes down is left to it, and so is a
       plain value where nothing above says otherwise. */
    Object.keys(decls).forEach(function(n){
      var v = decls[n][0];
      if(parentInh[n] === v
         || (plainValue(n, v) && (parentInh[n] === undefined || plainValue(n, parentInh[n])))){
        delete decls[n];
        delete mine[n];
      }
    });
    var inh = Object.assign({}, parentInh, mine);
    for(var i = el.attributes.length - 1; i >= 0; i--){
      var a = el.attributes[i].name.toLowerCase();
      if(!ATTRS.test(a)) el.removeAttribute(el.attributes[i].name);
    }
    el.removeAttribute('style');
    Object.keys(decls).forEach(function(n){
      if(keepProp(n, decls[n][0])) el.style.setProperty(n, decls[n][0], decls[n][1] ? 'important' : '');
    });
    widenDouble(el.style);
    if(!el.getAttribute('style')) el.removeAttribute('style');
    Array.prototype.slice.call(el.children).forEach(function(c){ clean(c, rules, inh); });
  }
  /* Tags kept as they are, tags dropped with their content, and anything
     else unwrapped: an office program's own elements, Google Sheets'
     wrapper. Whitespace between rows is the source's indentation. */
  function prune(root, links){
    Array.prototype.slice.call(root.childNodes).forEach(function(n){
      if(n.nodeType === 3){
        if(/^(TABLE|THEAD|TBODY|TFOOT|TR|COLGROUP)$/.test(root.tagName) && !n.nodeValue.trim()) n.remove();
        /* As the page drew it: the editor would keep a run of it, as
           no-break spaces. */
        else n.nodeValue = n.nodeValue.replace(/[ \t\n\r\f]+/g, ' ');
        return;
      }
      if(n.nodeType !== 1){ n.remove(); return; }
      var tag = n.tagName.toUpperCase();
      /* WebKit's mark for a space it had to keep: an ordinary one here. */
      if(tag === 'SPAN' && n.classList.contains('Apple-converted-space')){
        n.replaceWith(n.ownerDocument.createTextNode(n.textContent.replace(/\u00a0/g, ' ')));
        return;
      }
      if(DROP.test(tag) || (tag === 'A' && /\bcomment-indicator\b/.test(n.className))){
        n.remove();
        return;
      }
      prune(n, links);
      if(tag === 'A'){
        var href = (n.getAttribute('href') || '').trim();
        if(/^(https?:|mailto:)/i.test(href)) links.push([n, href]);
        else{ n.replaceWith.apply(n, Array.prototype.slice.call(n.childNodes)); return; }
      }
      if(!KEEP.test(tag)) n.replaceWith.apply(n, Array.prototype.slice.call(n.childNodes));
    });
  }
  /* A cell's text starts and ends where its words do. */
  function trimEnds(cell){
    var texts = [], w = cell.ownerDocument.createTreeWalker(cell, NodeFilter.SHOW_TEXT);
    for(var t = w.nextNode(); t; t = w.nextNode()) texts.push(t);
    function trim(list, re){
      for(var i = 0; i < list.length; i++){
        list[i].nodeValue = list[i].nodeValue.replace(re, '');
        if(list[i].nodeValue) return;
        list[i].remove();
      }
    }
    trim(texts, /^ +/);
    trim(texts.filter(function(x){ return x.isConnected; }).reverse(), / +$/);
  }
  /* Whether the table brings colors of its own: shown on the light page it
     was made on, in a dark composer (see the editor's stylesheet). */
  function colored(t){
    if(t.querySelector('[bgcolor],[color]') || t.hasAttribute('bgcolor')) return true;
    var all = [t].concat(Array.prototype.slice.call(t.querySelectorAll('[style]')));
    return all.some(function(e){
      var s = e.getAttribute('style') || '';
      return /color|background|border/i.test(s);
    });
  }
  /* The table in `doc` when the clipboard holds a spreadsheet's cells and
     nothing else, as markup ready to insert; null otherwise. `one` lets a
     single cell through, which a paste as text would rather have as its
     value. */
  window.__hylkiSheet = function(doc, one){
    var body = doc.body;
    if(!body) return null;
    var rules = rulesOf(doc);
    Array.prototype.forEach.call(body.querySelectorAll('style,script,title,meta,link'), function(n){ n.remove(); });
    var tables = topTables(body);
    if(tables.length !== 1) return null;
    var t = tables[0];
    if(body.textContent.trim() !== t.textContent.trim() || !t.textContent.trim()) return null;
    if(Array.prototype.some.call(body.querySelectorAll('img'), function(i){ return !t.contains(i); })) return null;
    if(!one && t.querySelectorAll('td,th').length < 2) return null;
    var page = {}, up = [];
    for(var p = t.parentElement; p; p = p.parentElement) up.unshift(p);
    up.forEach(function(e){ Object.assign(page, inheritedOf(cascade(e, rules))); });
    /* The values WebKit writes on the table are those of the page being
       pasted into: in a dark editor its text color is white, which the
       spreadsheet never asked for. */
    try{
      if(t.style.color === getComputedStyle(document.body).color) t.style.removeProperty('color');
    }catch(_){}
    var links = [];
    prune(t, links);
    Array.prototype.forEach.call(t.querySelectorAll('td,th'), trimEnds);
    clean(t, rules, {}, page);
    /* The line height of a page with no style of its own, which the rows'
       heights were measured against; the editor's is taller. */
    if(!t.style.lineHeight) t.style.lineHeight = 'normal';
    links.forEach(function(l){ l[0].setAttribute('href', l[1]); });
    if(colored(t)) t.setAttribute('class', 'hylki-sheet');
    return t.outerHTML;
  };
})();
