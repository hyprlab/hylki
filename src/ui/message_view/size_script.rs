//! The wrapper document's only script. It stays a Rust string: its `\`
//! line continuations are part of the bytes the page receives.

// The trailing window-blur listener is what makes a click *inside* a message
// body select its card: the sandboxed frames don't reliably dispatch
// parent-attached listeners, but a click into an iframe focuses it and blurs
// the wrapper window — caught there, the focused frame names the card, and
// focus is handed straight back so the next click fires again.
//
// `zf` is the message zoom on the frame's root. Under it WebKit reports the
// body's scroll sizes in the body's own unzoomed pixels, not the frame's, so
// at 90% the body always looked wider than its frame: each pass of the widen
// loop grew the frame by 1/zoom and centred mail drifted right (#473).
//
// A frame is shown once its document is parsed, not at its `load` event:
// that waits for every remote image and stylesheet in the mail, and a slow
// or silent image host left the card blank for up to a minute (#487).
// Listeners on a script-less sandboxed document never fire, so it is
// polled; `init` runs once and the later `load` only measures again.
/// Wrapper-document script: size each message iframe to its content height so the
/// whole conversation scrolls as one page (the iframes have no inner scrollbars).
/// Re-measures as images load and as content reflows.
pub(super) const SIZE_SCRIPT: &str = "\
function zf(d){try{return parseFloat(d.defaultView.getComputedStyle(d.documentElement).zoom)||1;}catch(_){return 1;}}\
function s(f){if(f._s)return;f._s=1;try{var d=f.contentDocument;if(!d||d.URL==='about:blank')return;\
f.classList.add('hylki-live');\
var b=d.body,e=d.documentElement;\
var sy=window.scrollY;var _r=f.getBoundingClientRect();\
var above=_r.bottom<=0;var old=_r.height||0;\
f.style.width='';void f.offsetWidth;\
var z=zf(d);\
for(var k=0;k<8;k++){var w=Math.max(b?Math.ceil(b.scrollWidth*z):0,e?e.scrollWidth:0);\
if(w<=f.clientWidth+1)break;f.style.width=w+'px';void f.offsetWidth;}\
var prev=f.style.height;f.style.height='0px';void f.offsetHeight;\
var h=Math.max(b?Math.ceil(b.scrollHeight*z):0,e?e.scrollHeight:0,b?Math.ceil(b.offsetHeight*z):0);\
if(h>0){f.style.height=h+'px';\
if(f.dataset.key&&f._h!==h){f._h=h;\
try{window.webkit.messageHandlers.hylki.postMessage('size:'+f.dataset.key+':'+h);}catch(_){}}}\
else{f.style.height=prev;h=old;}\
if(sy>0)window.scrollTo(0,above?sy+(h-old):sy);\
if(f._qp)f._qp();chase();pin();\
}catch(_){}finally{f._s=0;}}\
function pick(k,e){reportPos();\
var mo=e.shiftKey?'r':((e.ctrlKey||e.metaKey)?'t':'p');\
if(mo!=='p'){try{e.preventDefault();\
var g=(e.view&&e.view.getSelection)?e.view.getSelection():null;if(g)g.removeAllRanges();}catch(_){}}\
else{try{var t=(e.view&&e.view.getSelection)?e.view.getSelection():null;\
if(t&&String(t).length)return;}catch(_){}}\
try{window.webkit.messageHandlers.hylki.postMessage('sel:'+k+':'+mo);}catch(_){}}\
var QS='.hylki-quote-attr,.gmail_quote,blockquote:not([style*=\"border: none\"]),#divRplyFwdMsg,.yahoo_quoted';\
var SIG='.moz-signature,#Signature,.gmail_signature,[class*=\"signature\"]';\
var QT=window.vireoQT||['Show quoted text','Hide quoted text'];\
var WROTE=/(wrote|writes|schrieb|a écrit|escribió|escreveu|scrisse|schreef|skrev|kirjoitti|napisał|napsal|írta|написал|написала|έγραψε)\\s*:?$/i;\
var DIV=/^\\s*-{4,}[^-]{3,40}-{4,}\\s*$/;\
function isq(n){return n.nodeType===1&&(n.matches(QS)||!!n.querySelector(QS));}\
function blank(n){if(n.nodeType===1&&/^(STYLE|SCRIPT|META|LINK|TITLE)$/.test(n.tagName))return true;\
return n.nodeType===3?!n.textContent.trim():(n.nodeType!==1||!n.textContent.trim());}\
function kids(r){var a=[];for(var n=r.firstChild;n;n=n.nextSibling)if(!blank(n))a.push(n);return a;}\
function trailer(top,hd){\
if(hd)return true;\
var n=top.nextSibling;\
while(n&&(isq(n)||blank(n)))n=n.nextSibling;\
var t='';\
for(;n;n=n.nextSibling){\
if(n.nodeType===3)t+=n.textContent;\
else if(n.nodeType===1&&!n.matches(SIG)){\
var c=n.cloneNode(true);var sg=c.querySelectorAll(SIG);\
for(var i=0;i<sg.length;i++)sg[i].parentNode.removeChild(sg[i]);\
var br=c.querySelectorAll('br');\
for(var j=0;j<br.length;j++)br[j].parentNode.replaceChild(c.ownerDocument.createTextNode('\\n'),br[j]);\
t+='\\n'+c.textContent;}}\
t=t.replace(/\\u00a0/g,' ').trim();\
return !t||/^[-_]{2,}[ \\t]*(\\n|$)/.test(t);}\
function findq(d){\
var sel=['.hylki-quote-attr','.gmail_quote','blockquote[type=\"cite\"]','#divRplyFwdMsg','.yahoo_quoted'];\
for(var i=0;i<sel.length;i++){var q=d.querySelector(sel[i]);if(q)return [q,i===0||i===3];}\
var bs=d.querySelectorAll('div[style*=\"border-top\"]');\
for(var j=0;j<bs.length;j++){var lb=bs[j].querySelectorAll('b,strong'),c=0;\
for(var k=0;k<lb.length;k++)if(/:\\s*$/.test(lb[k].textContent))c++;\
if(c>=3)return [bs[j],true];}\
var w=d.createTreeWalker(d.body,4),n;\
while((n=w.nextNode())){if(DIV.test(n.textContent)){var pe=n.parentNode;\
return [pe!==d.body&&pe.textContent.trim()===n.textContent.trim()?pe:n,true];}}\
q=d.querySelector('blockquote:not([style*=\"border: none\"])');return q?[q,false]:null;}\
function plain(d){var ps=d.querySelectorAll('.hylki-plain');if(!ps.length)return null;\
var p=ps[ps.length-1],L=p.textContent.split('\\n'),i=L.length-1,qs=0,st=-1;\
for(var j=0;j<L.length;j++)if(DIV.test(L[j])){st=j;break;}\
if(st<0){while(i>=0&&(/^\\s*>/.test(L[i])||!L[i].trim())){if(L[i].trim())qs++;i--;}\
if(!qs||i<0)return null;st=i+1;\
if(WROTE.test(L[i].trim())){st=i;\
if(i>0&&/^(On|Am|Le|El|Em|Il|Op|Den|Dne|W dniu)\\s/.test(L[i-1].trim())&&!/[.!?:]\\s*$/.test(L[i-1]))st=i-1;}}\
var bf=false;for(var k=0;k<st;k++)if(L[k].trim()){bf=true;break;}\
if(!bf)return null;\
var e=st-1;while(e>=0&&!L[e].trim())e--;\
var off=-1;for(k=0;k<=e;k++)off+=L[k].length+1;\
var w=d.createTreeWalker(p,4),n,acc=0;\
while((n=w.nextNode())){var l=n.textContent.length;if(acc+l>=off)break;acc+=l;}\
if(!n)return null;\
var r=d.createRange();r.setStart(n,off-acc);r.setEnd(p,p.childNodes.length);\
var box=d.createElement('span');box.appendChild(r.extractContents());p.appendChild(box);\
return box;}\
function quote(f){try{var d=f.contentDocument;if(!d||!d.body||f._q)return;\
var h=findq(d);\
if(!h){var pb=plain(d);if(pb)fold(f,d,pb);return;}\
var q=h[0],hd=h[1];\
var root=d.body;\
for(;;){var ks=kids(root);if(ks.length===1&&ks[0].nodeType===1&&ks[0]!==q&&ks[0].contains(q))root=ks[0];else break;}\
var top=q;while(top.parentNode&&top.parentNode!==root)top=top.parentNode;\
if(!top.parentNode)return;\
var start=top,pv=top.previousSibling;\
while(pv&&(blank(pv)||pv.nodeName==='BR'))pv=pv.previousSibling;\
if(pv&&!hd&&pv.textContent.length<400&&WROTE.test(pv.textContent.trim()))start=pv;\
var before=false;\
for(var n=root.firstChild;n&&n!==start;n=n.nextSibling){\
if(!blank(n)||n.nodeName==='IMG'||(n.nodeType===1&&!!n.querySelector('img')))before=true;}\
if(!before)return;\
if(!trailer(top,hd))return;\
var box=d.createElement('div');root.insertBefore(box,start);\
while(box.nextSibling)box.appendChild(box.nextSibling);\
fold(f,d,box);}catch(_){}}\
function fold(f,d,box){try{f._q=1;\
var sp=d.createElement('div');sp.style.cssText='display:none;height:28px;';\
box.parentNode.insertBefore(sp,box);box.style.display='none';\
if(!f.parentNode)return;\
var b=document.createElement('button');b.className='hylki-quote';\
b.type='button';b.textContent='\u{2022}\u{2022}\u{2022}';\
b.setAttribute('title',QT[0]);\
f.parentNode.insertBefore(b,f.nextSibling);\
f._qp=function(){if(!b.classList.contains('open')){b.style.top='';return;}\
var r=sp.getBoundingClientRect();b.style.top=Math.round(f.offsetTop+r.top+(r.height-b.offsetHeight)/2)+'px';};\
b.addEventListener('click',function(e){e.stopPropagation();e.preventDefault();\
var on=box.style.display==='none',sy=window.scrollY;\
var from=f.getBoundingClientRect().height;\
box.style.display=on?'':'none';sp.style.display=on?'block':'none';\
b.classList.toggle('open',on);\
b.setAttribute('title',on?QT[1]:QT[0]);f._qp();\
var to=0;try{var dd=f.contentDocument,bb=dd.body,ee=dd.documentElement;\
var prev=f.style.height;f.style.height='0px';void f.offsetHeight;\
var z=zf(dd);to=Math.max(bb?Math.ceil(bb.scrollHeight*z):0,ee?ee.scrollHeight:0,bb?Math.ceil(bb.offsetHeight*z):0);\
f.style.height=prev;void f.offsetHeight;}catch(_){}\
if(!to){s(f);window.scrollTo(0,sy);return;}\
f.style.height=from+'px';f.classList.add('anim');\
void f.offsetHeight;\
f.style.height=to+'px';\
window.scrollTo(0,sy);\
setTimeout(function(){f.classList.remove('anim');s(f);\
if(on){var br=b.getBoundingClientRect();\
if(br.top>innerHeight*0.6)window.scrollBy({top:Math.min(br.top-innerHeight*0.3,to-from),behavior:'smooth'});}},280);});}catch(_){}}\
function foldCard(sec){if(!sec)return;var on=!sec.classList.contains('hylki-folded');\
sec.classList.toggle('hylki-folded',on);\
try{window.webkit.messageHandlers.hylki.postMessage('fold:'+sec.dataset.key+':'+(on?1:0));}catch(_){}\
if(!on){var f=sec.querySelector('iframe.hylki-frame');if(f){f._h=0;s(f);setTimeout(function(){s(f);},80);}}}\
function init(f){if(f._i){s(f);return;}f._i=1;quote(f);s(f);try{var d=f.contentDocument;if(d){if(window.ResizeObserver&&d.body){new ResizeObserver(function(){s(f);}).observe(d.body);}\
if(f.dataset.key&&!f._c){f._c=1;d.addEventListener('click',function(e){\
if(e.target&&e.target.closest&&e.target.closest('a'))return;pick(f.dataset.key,e);});}\
if(!f._u){f._u=1;['wheel','touchstart','mousedown','keydown'].forEach(function(ev){\
d.addEventListener(ev,function(){follow=null;hold=null;},{passive:true,capture:true});});\
d.addEventListener('keydown',selAll,true);d.addEventListener('keydown',copySel,true);}\
var im=d.images||[];for(var i=0;i<im.length;i++){if(!im[i].complete){im[i].addEventListener('load',function(){s(f);});im[i].addEventListener('error',function(){s(f);});}}}}catch(_){}\
setTimeout(function(){s(f);},250);setTimeout(function(){s(f);},1000);}\
function all(){return document.querySelectorAll('iframe.hylki-frame');}\
document.addEventListener('DOMContentLoaded',function(){\
var fs=all();var pend=fs.length,rdy=false;\
function ready(){if(rdy)return;rdy=true;\
try{window.webkit.messageHandlers.hylki.postMessage('ready:0:0');}catch(_){}\
var bd=document.body.dataset;\
if(bd.hylkiNoscroll){var a=(bd.hylkiAnchor||'').split(':');\
if(a.length>=3){var el=document.querySelector('.hylki-msg[data-key=\"'+a[0]+':'+a[1]+'\"]');\
if(el){hold={el:el,off:parseInt(a[2],10)||0,g:a[3]==='g'};\
if(el.querySelector('.hylki-dot'))follow=el;\
setTimeout(pin,0);}}return;}\
var ds=document.querySelectorAll('.hylki-msg .hylki-dot');\
var d=ds.length?ds[0]:null;\
if(d){var m=d.closest('.hylki-msg');\
if(m)setTimeout(function(){try{follow=m;reveal(m);}catch(_){}},0);}\
else if(bd.hylkiNewest){\
var nm=document.querySelector('.hylki-msg[data-key=\"'+bd.hylkiNewest+'\"]');\
if(nm)setTimeout(function(){try{follow=nm;reveal(nm);}catch(_){}},0);}}\
setTimeout(markClipped,0);setTimeout(markClipped,400);\
if(!pend)ready();\
for(var i=0;i<fs.length;i++){(function(f){var counted=false;\
function tick(){if(counted)return;counted=true;if(--pend<=0)ready();}\
if(f.contentDocument&&f.contentDocument.readyState==='complete'&&f.contentDocument.URL!=='about:blank'){init(f);tick();}\
f.addEventListener('load',function(){init(f);tick();});\
var polls=0,poll=setInterval(function(){if(f._i||++polls>1200){clearInterval(poll);return;}\
var fd=null;try{fd=f.contentDocument;}catch(_){}\
if(fd&&fd.URL!=='about:blank'&&fd.body&&fd.readyState!=='loading'){clearInterval(poll);init(f);tick();}},30);})(fs[i]);}\
setTimeout(ready,450);\
var hs=document.querySelectorAll('.hylki-msg-hdr');\
for(var j=0;j<hs.length;j++){hs[j].addEventListener('dblclick',function(){\
try{window.webkit.messageHandlers.hylki.postMessage('open:'+this.dataset.key);}catch(_){}});}\
if(document.querySelectorAll('.hylki-msg').length>1){document.body.classList.add('hylki-folds');\
for(var j=0;j<hs.length;j++){(function(h){var tm=null;\
h.addEventListener('click',function(e){\
if(e.ctrlKey||e.metaKey||e.shiftKey||e.altKey)return;\
if(e.target.closest&&e.target.closest('button,a,.hylki-addr,.hylki-acts,.hylki-tags,.hylki-verify,.hylki-rcpt'))return;\
if(tm){clearTimeout(tm);tm=null;return;}\
tm=setTimeout(function(){tm=null;foldCard(h.closest('.hylki-msg'));},260);});})(hs[j]);}\
var fb=document.querySelectorAll('.hylki-fbar');\
for(var j=0;j<fb.length;j++){fb[j].addEventListener('click',function(e){\
if(e.ctrlKey||e.metaKey||e.shiftKey||e.altKey)return;foldCard(this.closest('.hylki-msg'));});}}\
var rbd=document.body.dataset;\
if(rbd.hylkiReadmark&&document.body.classList.contains('hylki-conv')){\
var rdel=parseInt(rbd.hylkiReadmark,10)||250;var rt={};\
var rio=new IntersectionObserver(function(es){es.forEach(function(en){\
var el=en.target,k=el.dataset.key;\
var vis=en.intersectionRatio>=0.5||en.intersectionRect.height>window.innerHeight*0.6;\
if(vis&&!rt[k]){rt[k]=setTimeout(function(){\
try{window.webkit.messageHandlers.hylki.postMessage('seen:'+k);}catch(_){}\
rio.unobserve(el);},rdel);}\
else if(!vis&&rt[k]){clearTimeout(rt[k]);delete rt[k];}\
});},{threshold:[0,0.25,0.5,0.75,1]});\
var rsecs=document.querySelectorAll('.hylki-msg');\
for(var ri=0;ri<rsecs.length;ri++){if(rsecs[ri].querySelector('.hylki-dot'))rio.observe(rsecs[ri]);}}\
window.vf={marks:[],cur:-1};\
window.vfZoom=function(z){var fs=all();for(var i=0;i<fs.length;i++){try{var d=fs[i].contentDocument;\
if(d&&d.documentElement)d.documentElement.style.zoom=z;fs[i]._h=0;s(fs[i]);}catch(_){}}};\
window.vfDocs=function(){var ds=[document];var fs=document.querySelectorAll('iframe');\
for(var i=0;i<fs.length;i++){try{if(fs[i].contentDocument)ds.push(fs[i].contentDocument);}catch(_){}}return ds;};\
window.vfCss=function(d){if(d.getElementById('hylki-find-css'))return;\
var st=d.createElement('style');st.id='hylki-find-css';\
st.textContent='.hylki-find{background:rgba(255,198,0,0.5);color:#000;border-radius:5px;box-shadow:0 0 0 2px rgba(255,198,0,0.5);}.hylki-find.current{background:#ffc600;box-shadow:0 0 0 2px #ffc600;}';\
(d.head||d.documentElement).appendChild(st);};\
window.hylkiFindClear=function(){for(var i=0;i<vf.marks.length;i++){var m=vf.marks[i];\
var p=m.parentNode;if(!p)continue;while(m.firstChild)p.insertBefore(m.firstChild,m);p.removeChild(m);p.normalize();}\
vf.marks=[];vf.cur=-1;};\
window.vfPost=function(){try{window.webkit.messageHandlers.hylki.postMessage(\
'found:0:0:'+(vf.marks.length?vf.cur+1:0)+','+vf.marks.length);}catch(_){}};\
window.vfShow=function(){for(var i=0;i<vf.marks.length;i++)vf.marks[i].className='hylki-find'+(i===vf.cur?' current':'');\
vfPost();\
var m=vf.marks[vf.cur];if(!m)return;\
var d=m.ownerDocument;\
if(d===document){m.scrollIntoView({block:'center'});}\
else{var fs=document.querySelectorAll('iframe');\
for(var j=0;j<fs.length;j++){if(fs[j].contentDocument===d){\
var r=m.getBoundingClientRect(),fr=fs[j].getBoundingClientRect();\
window.scrollTo({top:window.scrollY+fr.top+r.top-window.innerHeight/2});break;}}}};\
window.vfY=function(m){var d=m.ownerDocument,r=m.getBoundingClientRect();\
if(d===document)return r.top+window.scrollY;\
var fs=document.querySelectorAll('iframe');\
for(var j=0;j<fs.length;j++){if(fs[j].contentDocument===d)\
return fs[j].getBoundingClientRect().top+window.scrollY+r.top;}\
return r.top;};\
window.hylkiFindStep=function(dir){if(!vf.marks.length)return;\
vf.cur=(vf.cur+dir+vf.marks.length)%vf.marks.length;vfShow();};\
window.hylkiFind=function(q){hylkiFindClear();q=(q||'').toLowerCase();\
if(q){var docs=vfDocs();\
for(var di=0;di<docs.length;di++){var d=docs[di];vfCss(d);\
var w=d.createTreeWalker(d.body||d.documentElement,NodeFilter.SHOW_TEXT,null);\
var nodes=[];var n;\
while((n=w.nextNode())){var pn=n.parentNode&&n.parentNode.nodeName;\
if(pn==='SCRIPT'||pn==='STYLE')continue;\
if(n.nodeValue&&n.nodeValue.toLowerCase().indexOf(q)>=0)nodes.push(n);}\
for(var ni=0;ni<nodes.length;ni++){var node=nodes[ni];\
var text=node.nodeValue,lower=text.toLowerCase(),pos=0,idx;\
var frag=d.createDocumentFragment(),had=false;\
while((idx=lower.indexOf(q,pos))>=0){had=true;\
frag.appendChild(d.createTextNode(text.slice(pos,idx)));\
var sp=d.createElement('span');sp.className='hylki-find';\
sp.textContent=text.slice(idx,idx+q.length);\
frag.appendChild(sp);vf.marks.push(sp);pos=idx+q.length;}\
frag.appendChild(d.createTextNode(text.slice(pos)));\
if(had)node.parentNode.replaceChild(frag,node);}}\
vf.marks=vf.marks.filter(function(m){return m.getClientRects().length>0;});\
if(vf.marks.length){vf.marks.sort(function(a,b){return vfY(a)-vfY(b);});\
vf.cur=0;vfShow();}else{vfPost();}}else{vfPost();}};\
var rs=document.querySelectorAll('.hylki-rcpt-toggle');\
for(var r=0;r<rs.length;r++){rs[r].addEventListener('click',function(e){\
e.stopPropagation();e.preventDefault();\
var h=this.closest('.hylki-msg-hdr');var b=h?h.querySelector('.hylki-rcpt'):null;if(!b)return;\
var on=b.hasAttribute('hidden');\
if(on)b.removeAttribute('hidden');else b.setAttribute('hidden','');\
this.classList.toggle('open',on);\
this.setAttribute('title',on?'Hide recipients':'Show recipients');});\
rs[r].addEventListener('dblclick',function(e){e.stopPropagation();});}\
document.addEventListener('click',function(e){\
if(e.target&&e.target.closest&&e.target.closest('.hylki-msg'))return;\
try{window.webkit.messageHandlers.hylki.postMessage('desel:0:0');}catch(_){}});\
document.addEventListener('click',function(e){\
var t=e.target&&e.target.closest?e.target.closest('.hylki-attc,.hylki-attsave'):null;if(!t)return;\
e.stopPropagation();e.preventDefault();\
var verb=t.classList.contains('hylki-attsave')?'attsave':'attopen';\
try{window.webkit.messageHandlers.hylki.postMessage(verb+':'+t.dataset.key+':'+t.dataset.idx);}catch(_){}},true);\
var ms=document.querySelectorAll('.hylki-msg');\
for(var q=0;q<ms.length;q++){ms[q].addEventListener('click',function(e){\
var k=this.dataset.key;if(k)pick(k,e);});}\
var actsDelay=parseInt(document.body.dataset.hylkiActsdelay||'0',10)||1200;\
document.documentElement.style.setProperty('--acts-delay',actsDelay+'ms');\
var actsMenu=document.body.dataset.hylkiActsmenu==='1';\
var ts=document.querySelectorAll('.hylki-acts-toggle');\
for(var t=0;t<ts.length;t++){ts[t].addEventListener('click',function(e){\
e.stopPropagation();e.preventDefault();\
if(actsMenu){var br=this.getBoundingClientRect();\
try{window.webkit.messageHandlers.hylki.postMessage('cardmenu:'+this.dataset.key+':'\
+br.left+','+br.bottom+','+window.innerWidth);}catch(_){}return;}\
var h=this.closest('.hylki-msg-hdr');var ac=h?h.querySelector('.hylki-acts'):null;if(!ac)return;\
var on=!ac.classList.contains('open');\
ac.classList.toggle('open',on);this.classList.toggle('open',on);});\
ts[t].addEventListener('dblclick',function(e){e.stopPropagation();});}\
for(var q2=0;q2<ms.length;q2++){(function(card){var timer=null;\
card.addEventListener('mouseleave',function(){\
var ac=card.querySelector('.hylki-acts.open');if(!ac)return;\
timer=setTimeout(function(){ac.classList.remove('open');\
var tg=card.querySelector('.hylki-acts-toggle');if(tg)tg.classList.remove('open');},actsDelay);});\
card.addEventListener('mouseenter',function(){if(timer){clearTimeout(timer);timer=null;}});})(ms[q2]);}\
var as=document.querySelectorAll('.hylki-act');\
for(var k=0;k<as.length;k++){as[k].addEventListener('click',function(e){\
e.stopPropagation();e.preventDefault();reportPos();\
if(this.dataset.act==='star')this.classList.toggle('on');\
var extra='';if(this.dataset.act==='moveto'){var br=this.getBoundingClientRect();\
extra=':'+(br.left+br.width/2)+','+br.bottom+','+window.innerWidth;}\
try{window.webkit.messageHandlers.hylki.postMessage(this.dataset.act+':'+this.dataset.key+extra);}catch(_){}});\
as[k].addEventListener('dblclick',function(e){e.stopPropagation();});}\
document.addEventListener('click',function(e){var b=e.target&&e.target.closest?e.target.closest('.hylki-unsub-btn'):null;\
if(!b)return;e.stopPropagation();e.preventDefault();\
var v=b.dataset.tpl?'tpl:'+b.dataset.key+':'+b.dataset.tpl:b.dataset.mdn?'mdn:'+b.dataset.key+':'+b.dataset.mdn:'unsub:'+b.dataset.key;\
try{window.webkit.messageHandlers.hylki.postMessage(v);}catch(_){}});\
document.addEventListener('click',function(e){var b=e.target&&e.target.closest?e.target.closest('.hylki-tr-btn'):null;\
if(!b)return;e.stopPropagation();e.preventDefault();\
try{window.webkit.messageHandlers.hylki.postMessage('tr:'+b.dataset.key);}catch(_){}});\
document.addEventListener('click',function(e){var b=e.target&&e.target.closest?e.target.closest('.hylki-inv-btn'):null;\
if(!b)return;e.stopPropagation();e.preventDefault();\
try{window.webkit.messageHandlers.hylki.postMessage('invite:'+b.dataset.key+':'+b.dataset.inv);}catch(_){}});\
});\
function markClipped(){var as=document.querySelectorAll('.hylki-addr');\
for(var i=0;i<as.length;i++){var a=as[i];\
a.classList.toggle('clipped',a.scrollWidth>a.clientWidth+1);}}\
function hideMailMenu(){var m=window.__hylkiMailMenu;\
if(m&&m.parentNode)m.parentNode.removeChild(m);window.__hylkiMailMenu=null;\
var sc=window.__hylkiMailScrim;\
if(sc&&sc.parentNode)sc.parentNode.removeChild(sc);window.__hylkiMailScrim=null;}\
function mailMsg(mail){try{window.webkit.messageHandlers.hylki.postMessage('composeto:0:0:'+mail);}catch(_){}}\
function showMailMenu(x,y,mail){hideMailMenu();\
var sc=document.createElement('div');sc.className='hylki-mailmenu-scrim';\
['mousedown','contextmenu'].forEach(function(ev){sc.addEventListener(ev,function(e){\
if(ev==='mousedown')e.preventDefault();e.stopPropagation();hideMailMenu();});});\
document.body.appendChild(sc);window.__hylkiMailScrim=sc;\
var mn=document.createElement('div');mn.className='hylki-mailmenu';\
function item(label,fn){var b=document.createElement('button');b.type='button';\
b.textContent=label;b.addEventListener('click',function(ev){\
ev.stopPropagation();ev.preventDefault();hideMailMenu();fn();});mn.appendChild(b);}\
item(hylkiAM[0],function(){mailMsg(mail);});\
item(hylkiAM[1],function(){try{window.webkit.messageHandlers.hylki.postMessage('copyaddr:0:0:'+mail);}catch(_){}});\
item(hylkiAM[2],function(){try{window.webkit.messageHandlers.hylki.postMessage('addcontact:0:0:'+mail);}catch(_){}});\
document.body.appendChild(mn);window.__hylkiMailMenu=mn;\
mn.style.left=Math.max(0,Math.min(x,window.innerWidth-mn.offsetWidth))+'px';\
mn.style.top=Math.max(0,Math.min(y,window.innerHeight-mn.offsetHeight))+'px';}\
/* The card a right-click at widget point (x, y) is on, in a view w\
   widget pixels wide, and the selected text, for Copy. A right-click\
   anywhere else dismisses the address menu, as a click does. Two answer\
   {skip:1}, for no message menu: one on the ground around the cards, which\
   is no message's (#529), and one on an address, which opens the address\
   menu here. WebKit sends the page's contextmenu event to the body frame a\
   previous right-click pressed in, whose release the app's menu took, so\
   the listener above can miss it. */\
window.__hylkiMenuAt=function(x,y,w){hideMailMenu();var r=window.innerWidth/w;x*=r;y*=r;\
var el=document.elementFromPoint(x,y);\
var ml=el&&el.closest?el.closest('.hylki-mail'):null;\
if(ml&&ml.dataset.mail){showMailMenu(x,y,ml.dataset.mail);return JSON.stringify({skip:1});}\
var m=el&&el.closest?el.closest('.hylki-msg'):null;\
if(!m)return JSON.stringify({skip:1});\
var sel='';try{sel=String(window.getSelection()||'');\
if(!sel){var fs=all();for(var j=0;j<fs.length&&!sel;j++){\
try{sel=String(fs[j].contentDocument.getSelection()||'');}catch(_){}}}}catch(_){}\
return JSON.stringify({key:(m.dataset&&m.dataset.key)||'',sel:sel});};\
/* A left click at widget point (x, y) that landed on a message's body\
   frame, which never reports its own clicks, picks that card as a click\
   on the rest of the card does. A link or a text selection in the body\
   keeps the click to itself. mo is p (plain), t (Ctrl) or r (Shift). */\
/* The card a press at widget point (x, y) can drag: one whose header, or\
   folded bar, is under it, away from the buttons, links and chips there.\
   Its key, or nothing. */\
window.__hylkiDragAt=function(x,y,w){var r=window.innerWidth/w;x*=r;y*=r;\
var el=document.elementFromPoint(x,y);if(!el||!el.closest)return '';\
if(el.closest('button,a,input,.hylki-addr,.hylki-acts,.hylki-tags,.hylki-verify,.hylki-rcpt'))return '';\
var h=el.closest('.hylki-msg-hdr,.hylki-fbar');if(!h)return '';\
var m=h.closest('.hylki-msg');return (m&&m.dataset.key)||h.dataset.key||'';};\
/* The reader drags a card by its header itself; WebKit's own drag of the\
   avatar picture there would start first and carry only the picture. */\
document.addEventListener('dragstart',function(e){var t=e.target;\
if(t&&t.closest&&t.closest('.hylki-msg-hdr,.hylki-fbar'))e.preventDefault();},true);\
window.__hylkiPickAt=function(x,y,w,mo){var r=window.innerWidth/w;x*=r;y*=r;\
var f=document.elementFromPoint(x,y);\
if(!f||f.tagName!=='IFRAME'||!f.classList.contains('hylki-frame'))return;\
var m=f.closest('.hylki-msg');var k=(m&&m.dataset.key)||f.dataset.key;if(!k)return;\
try{var d=f.contentDocument,b=f.getBoundingClientRect();\
var t=d.elementFromPoint(x-b.left,y-b.top);if(t&&t.closest&&t.closest('a'))return;\
var g=d.getSelection();if(g){if(mo!=='p')g.removeAllRanges();else if(String(g).length)return;}}catch(_){}\
reportPos();\
try{window.webkit.messageHandlers.hylki.postMessage('sel:'+k+':'+mo);}catch(_){}};\
window.addEventListener('keydown',function(e){if(e.key!=='Escape'||!window.__hylkiMailMenu)return;\
e.preventDefault();e.stopPropagation();hideMailMenu();},true);\
document.addEventListener('click',function(e){var m=window.__hylkiMailMenu;\
if(m&&e.target&&m.contains(e.target))return;hideMailMenu();},true);\
window.addEventListener('scroll',hideMailMenu,{passive:true});\
document.addEventListener('click',function(e){\
var t=e.target&&e.target.closest?e.target.closest('.hylki-mail'):null;\
if(!t||!t.dataset.mail)return;\
e.preventDefault();e.stopPropagation();mailMsg(t.dataset.mail);},true);\
document.addEventListener('click',function(e){\
var v=e.target&&e.target.closest?e.target.closest('.hylki-verify,.hylki-pgp'):null;\
if(!v||!v.dataset.key)return;e.preventDefault();e.stopPropagation();\
var r=v.getBoundingClientRect();\
try{window.webkit.messageHandlers.hylki.postMessage(\
'senderinfo:'+v.dataset.key+':'+r.left+','+r.top+','+r.width+','+r.height+','+window.innerWidth);}catch(_){}},true);\
document.addEventListener('contextmenu',function(e){\
var t=e.target&&e.target.closest?e.target.closest('.hylki-mail'):null;\
if(!t||!t.dataset.mail)return;\
e.preventDefault();e.stopPropagation();\
showMailMenu(e.clientX,e.clientY,t.dataset.mail);},true);\
window.addEventListener('resize',function(){var fs=all();for(var i=0;i<fs.length;i++)s(fs[i]);\
markClipped();});\
var _st;function reportPos(){var ms=document.querySelectorAll('.hylki-msg');\
var best=null,off=0;\
for(var i=0;i<ms.length;i++){var r=ms[i].getBoundingClientRect();\
if(r.top<=1){best=ms[i];off=Math.max(0,-r.top);}else break;}\
if(best&&best.dataset.key){try{window.webkit.messageHandlers.hylki.postMessage(\
'scrollat:'+best.dataset.key+':'+Math.round(off));}catch(_){}}}\
window.addEventListener('scroll',function(){clearTimeout(_st);\
_st=setTimeout(reportPos,120);},{passive:true});\
var follow=null,hold=null;\
function selAll(e){if(!(e.ctrlKey||e.metaKey)||e.altKey||(e.key!=='a'&&e.key!=='A'))return;\
if(!document.body||!document.body.classList.contains('hylki-conv'))return;\
e.preventDefault();e.stopPropagation();\
try{window.webkit.messageHandlers.hylki.postMessage('selall:0:0');}catch(_){}}\
window.addEventListener('keydown',selAll,true);\
/* Ctrl+C. Focus is kept in this document (see the blur handler below) so\
   the single-key shortcuts keep working, which leaves the selection in a\
   body frame and the native copy with nothing to copy. Copy from the\
   frame that holds the selection instead; if the engine refuses, hand the\
   text to the host, which sets the clipboard itself. */\
function copyDoc(d,w){var sel=d&&d.getSelection();if(!sel||sel.isCollapsed||!String(sel).length)return false;\
var ok=false;try{if(w!==window)w.focus();ok=d.execCommand('copy');}catch(_){}\
try{if(w!==window)window.focus();}catch(_){}\
if(!ok){try{window.webkit.messageHandlers.hylki.postMessage('copy:0:0:'+String(sel));}catch(_){}}\
showCopied();return true;}\
/* Copies whatever is selected, in this document or in a body frame. Called\
   from the keydown below when this view has the keyboard, and by the host\
   when it does not: the list takes GTK focus back after a click in the\
   reader, so Ctrl+C usually lands on the window, not here. */\
window.__hylkiCopySel=function(){try{if(copyDoc(document,window))return true;\
var fs=all();for(var i=0;i<fs.length;i++){try{if(copyDoc(fs[i].contentDocument,fs[i].contentWindow))return true;}catch(_){}}}catch(_){}\
return false;};\
function copySel(e){if(!(e.ctrlKey||e.metaKey)||e.altKey||e.shiftKey||(e.key!=='c'&&e.key!=='C'))return;\
var s=window.getSelection();if(s&&!s.isCollapsed&&String(s).length)return;\
if(window.__hylkiCopySel()){e.preventDefault();e.stopPropagation();}}\
function showCopied(){var b=document.body;if(!b)return;\
var p=document.getElementById('hylki-copied');\
if(!p){p=document.createElement('div');p.id='hylki-copied';p.className='hylki-copied';b.appendChild(p);}\
p.textContent=b.dataset.hylkiCopied||'Copied';\
void p.offsetHeight;p.classList.add('on');clearTimeout(p._t);\
p._t=setTimeout(function(){p.classList.remove('on');},1400);}\
window.addEventListener('keydown',copySel,true);\
function gutter(){try{return parseFloat(getComputedStyle(document.body).paddingTop)||0;}catch(_){return 0;}}\
function reveal(el){var r=el.getBoundingClientRect();var d=Math.round(r.top-gutter());if(d)window.scrollBy(0,d);}\
function chase(){if(!follow)return;try{reveal(follow);}catch(_){}}\
function pin(){if(follow||!hold)return;try{var r=hold.el.getBoundingClientRect();\
var d=Math.round(r.top+hold.off-(hold.g?gutter():0));if(d)window.scrollBy(0,d);}catch(_){}}\
['wheel','touchstart','mousedown','keydown'].forEach(function(ev){\
window.addEventListener(ev,function(){follow=null;hold=null;},{passive:true,capture:true});});\
window.addEventListener('blur',function(){setTimeout(function(){\
var a=document.activeElement;\
if(a&&a.tagName==='IFRAME'&&a.classList&&a.classList.contains('hylki-frame')&&a.dataset.key){\
try{a.blur();window.focus();}catch(_){}}},0);});";
