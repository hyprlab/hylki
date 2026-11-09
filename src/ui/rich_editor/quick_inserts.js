(function(){
  var MIN_PREFIX = 3;
  var MAX_SUGGESTIONS = 3;
  var LOOKBEHIND = 256;
  var candidates = [];
  var lowered = [];
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

  function remember(words) {
    candidates = (Array.isArray(words) ? words : [])
      .map(function(value){ return String(value || '').trim(); })
      .filter(Boolean);
    lowered = candidates.map(function(value){ return value.toLocaleLowerCase(); });
  }

  function hide() {
    open = false;
    current = null;
    if (popup) popup.hidden = true;
    notify();
  }

  function wordBeforeCaret() {
    var text = '';
    var source = document.getElementById('src');
    if (source) {
      if (!document.hasFocus() || document.activeElement !== source) return null;
      if (source.selectionStart !== source.selectionEnd) return null;
      var source_end = source.selectionStart;
      text = source.value.slice(Math.max(0, source_end - LOOKBEHIND), source_end);
    } else {
      if (!document.hasFocus()) return null;
      var sel = window.getSelection();
      if (!sel || !sel.rangeCount || !sel.isCollapsed) return null;
      var node = sel.focusNode;
      if (!node || node.nodeType !== Node.TEXT_NODE) return null;
      text = (node.data || '').slice(
        Math.max(0, sel.focusOffset - LOOKBEHIND),
        sel.focusOffset
      );
    }
    var match = text.match(/(?:^|[\s\u00a0])([^\s.,!?;:()\[\]{}"«»„“\-—]{3,})$/u);
    if (!match || match[1].length < MIN_PREFIX) return null;
    var rect = null;
    if (source) {
      var box = source.getBoundingClientRect();
      rect = { left: box.left + 16, top: box.top + 18, bottom: box.top + 40 };
    } else {
      var range = sel.getRangeAt(0).cloneRange();
      range.collapse(true);
      rect = range.getBoundingClientRect();
      if (!rect || (!rect.width && !rect.height)) {
        rect = node && node.parentElement ? node.parentElement.getBoundingClientRect() : null;
      }
    }
    if (!rect) return null;
    return { prefix: match[1], rect: rect };
  }

  function matching(prefix) {
    var low = prefix.toLocaleLowerCase();
    return candidates.filter(function(value, i){
      return lowered[i].startsWith(low) && lowered[i] !== low;
    })
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
    if (!candidates.length) { hide(); return; }
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
    if (!open || !current) refresh();
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
    remember(words);
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

  remember(window.__hylkiQuickInserts);
  document.addEventListener('input', schedule, true);
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
