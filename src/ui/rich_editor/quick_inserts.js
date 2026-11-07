(function(){
  var MIN_PREFIX = 3;
  var MAX_SUGGESTIONS = 3;
  var candidates = Array.isArray(window.__hylkiQuickInserts) ? window.__hylkiQuickInserts.slice() : [];
  var selected = 0;
  var current = null;
  var popup = null;
  var open = false;
  var notified = false;

  function notify() {
    if (notified === open) return;
    notified = open;
    try {
      window.webkit.messageHandlers.hylkiQuick.postMessage(open ? '1' : '0');
    } catch (_) {}
  }

  function editor() {
    return document.getElementById('src') || document.querySelector('body[contenteditable="true"]');
  }

  function hide() {
    open = false;
    current = null;
    if (popup) popup.hidden = true;
    notify();
  }

  function wordBeforeCaret() {
    var text = '';
    var rect = null;
    var source = document.getElementById('src');
    if (source) {
      if (!document.hasFocus() || document.activeElement !== source) return null;
      if (source.selectionStart !== source.selectionEnd) return null;
      text = source.value.slice(0, source.selectionStart);
      var box = source.getBoundingClientRect();
      rect = { left: box.left + 16, top: box.top + 18, bottom: box.top + 40 };
    } else {
      if (!document.hasFocus()) return null;
      var sel = window.getSelection();
      if (!sel || !sel.rangeCount || !sel.isCollapsed) return null;
      var node = sel.focusNode;
      if (!node || node.nodeType !== Node.TEXT_NODE) return null;
      text = (node.data || '').slice(0, sel.focusOffset);
      var range = sel.getRangeAt(0).cloneRange();
      range.collapse(true);
      var rects = range.getClientRects();
      rect = rects.length ? rects[rects.length - 1] : range.getBoundingClientRect();
      if (!rect || (!rect.width && !rect.height)) {
        rect = node.parentElement ? node.parentElement.getBoundingClientRect() : null;
      }
    }
    if (!rect) return null;
    var match = text.match(/(?:^|[\s\u00a0])([^\s.,!?;:()\[\]{}"«»„“\-—]{3,})$/u);
    if (!match || match[1].length < MIN_PREFIX) return null;
    return { prefix: match[1], rect: rect };
  }

  function matching(prefix) {
    var low = prefix.toLocaleLowerCase();
    return candidates.map(function(value){ return String(value || '').trim(); })
      .filter(function(value){ return value && value.toLocaleLowerCase().startsWith(low); })
      .filter(function(value){ return value.toLocaleLowerCase() !== low; })
      .slice(0, MAX_SUGGESTIONS);
  }

  function ensurePopup() {
    if (popup) return popup;
    popup = document.createElement('div');
    popup.id = 'hylki-quick-inserts';
    popup.contentEditable = 'false';
    popup.setAttribute('role', 'listbox');
    popup.addEventListener('mousedown', function(e){
      e.preventDefault();
      e.stopPropagation();
    });
    document.documentElement.appendChild(popup);
    return popup;
  }

  function place(rect) {
    var box = ensurePopup();
    box.hidden = false;
    box.style.left = '0px';
    box.style.top = '0px';
    var width = box.offsetWidth || 240;
    var height = box.offsetHeight || 60;
    var left = Math.max(8, Math.min(rect.left, window.innerWidth - width - 8));
    var top = rect.bottom + 5;
    if (top + height > window.innerHeight - 8) top = Math.max(8, rect.top - height - 5);
    box.style.left = Math.round(left) + 'px';
    box.style.top = Math.round(top) + 'px';
  }

  function render(words) {
    var box = ensurePopup();
    box.replaceChildren();
    words.forEach(function(word, i){
      var row = document.createElement('div');
      row.className = 'hylki-quick-row' + (i === selected ? ' active' : '');
      row.setAttribute('role', 'option');
      row.setAttribute('aria-selected', i === selected ? 'true' : 'false');
      var label = document.createElement('span');
      label.className = 'hylki-quick-label';
      label.textContent = word;
      row.appendChild(label);
      if (i === 0) {
        var key = document.createElement('span');
        key.className = 'hylki-quick-key';
        key.textContent = 'tab';
        row.appendChild(key);
      }
      row.addEventListener('mouseenter', function(){
        selected = i;
        Array.prototype.forEach.call(box.children, function(child, n){
          child.classList.toggle('active', n === i);
          child.setAttribute('aria-selected', n === i ? 'true' : 'false');
        });
      });
      row.addEventListener('click', function(e){
        e.preventDefault();
        selected = i;
        accept();
      });
      box.appendChild(row);
    });
  }

  function refresh() {
    var found = wordBeforeCaret();
    if (!found) { hide(); return; }
    var words = matching(found.prefix);
    if (!words.length) { hide(); return; }
    selected = Math.min(selected, words.length - 1);
    current = { prefix: found.prefix, words: words };
    open = true;
    render(words);
    place(found.rect);
    notify();
  }

  var scheduled = false;
  function schedule() {
    if (scheduled) return;
    scheduled = true;
    requestAnimationFrame(function(){ scheduled = false; refresh(); });
  }

  function accept() {
    if (!open || !current || !current.words.length) return false;
    var phrase = current.words[selected];
    var prefix = current.prefix;
    var source = document.getElementById('src');
    if (source) {
      var end = source.selectionStart;
      if (source.selectionEnd !== end || end < prefix.length) return false;
      source.setRangeText(phrase, end - prefix.length, end, 'end');
      window.__hylkiDirty = true;
      source.dispatchEvent(new InputEvent('input', { bubbles: true, inputType: 'insertReplacementText', data: phrase }));
    } else {
      var sel = window.getSelection();
      if (!sel || !sel.rangeCount || !sel.isCollapsed) return false;
      var node = sel.focusNode;
      var offset = sel.focusOffset;
      if (!node || node.nodeType !== Node.TEXT_NODE || offset < prefix.length) return false;
      var range = document.createRange();
      range.setStart(node, offset - prefix.length);
      range.setEnd(node, offset);
      sel.removeAllRanges();
      sel.addRange(range);
      document.execCommand('insertText', false, phrase);
    }
    hide();
    return true;
  }

  window.__hylkiQuickSet = function(words){
    candidates = Array.isArray(words) ? words.slice() : [];
    selected = 0;
    schedule();
  };
  window.__hylkiQuickAccept = function(){ return accept(); };
  window.__hylkiQuickMove = function(delta){
    if (!open || !current) return false;
    selected = (selected + delta + current.words.length) % current.words.length;
    render(current.words);
    return true;
  };
  window.__hylkiQuickDismiss = function(){ hide(); };

  document.addEventListener('input', refresh, true);
  document.addEventListener('compositionend', schedule, true);
  document.addEventListener('click', schedule, true);
  document.addEventListener('keyup', function(e){
    if (e.key === 'ArrowLeft' || e.key === 'ArrowRight' || e.key === 'Home' || e.key === 'End') schedule();
  }, true);
  document.addEventListener('selectionchange', schedule, true);
  window.addEventListener('scroll', schedule, true);
  window.addEventListener('resize', schedule);
  window.addEventListener('blur', hide);
  schedule();
})();
