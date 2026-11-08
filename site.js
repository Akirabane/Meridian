// The only script of the page: a light / dark switch, and the tabs of the bar at the top.
// It makes no request and sends nothing anywhere. Without it, the page still works: every section shows, one under
// the other, and the bar's links jump to them.
(function () {
  var root = document.documentElement, KEY = 'meridian-theme';
  function stored() { try { return localStorage.getItem(KEY); } catch (err) { return null; } }
  function save(v) { try { localStorage.setItem(KEY, v); } catch (err) { /* the page works without it */ } }
  var saved = stored();
  if (saved === 'light' || saved === 'dark') root.setAttribute('data-theme', saved);
  var button = document.getElementById('theme');
  if (!button) return;
  button.addEventListener('click', function () {
    var dark = root.getAttribute('data-theme') === 'dark' ||
      (!root.getAttribute('data-theme') && window.matchMedia('(prefers-color-scheme: dark)').matches);
    var next = dark ? 'light' : 'dark';
    root.setAttribute('data-theme', next);
    save(next);
  });
})();

// Tabs: one section shown at a time. The address says which (#roadmap), so a link, a bookmark and the browser's
// Back button open the right tab; an address that points inside a section (#extension-orders) opens its tab.
(function () {
  var bar = document.getElementById('tabs');
  var list = bar && bar.querySelector('.tablist');
  if (!list || !window.history || !history.pushState) return;
  var tabs = Array.prototype.slice.call(list.querySelectorAll('a[href^="#"]'));
  var panels = tabs.map(function (a) { return document.getElementById(a.getAttribute('href').slice(1)); });
  if (!tabs.length || panels.some(function (p) { return !p; })) return;
  var header = document.querySelector('header.top');
  var langs = Array.prototype.slice.call(bar.querySelectorAll('.lang a'));
  langs.forEach(function (a) { a.setAttribute('data-base', a.getAttribute('href').split('#')[0]); });

  setUp();
  function setUp() {
    document.documentElement.classList.add('tabbed');
    list.setAttribute('role', 'tablist');
    tabs.forEach(function (a, i) {
      a.setAttribute('role', 'tab');
      a.id = 'tab-' + panels[i].id;
      a.setAttribute('aria-controls', panels[i].id);
      panels[i].setAttribute('role', 'tabpanel');
      panels[i].setAttribute('aria-labelledby', a.id);
    });
  }

  // the tab that is, or holds, the element of this id (-1: none)
  function tabOf(id) {
    var el = id ? document.getElementById(id) : null;
    for (var i = 0; el && i < panels.length; i++) {
      if (panels[i] === el || panels[i].contains(el)) return i;
    }
    return -1;
  }

  function show(i) {
    tabs.forEach(function (a, j) {
      var on = i === j;
      a.setAttribute('aria-selected', on ? 'true' : 'false');
      a.setAttribute('tabindex', on ? '0' : '-1');
      panels[j].hidden = !on;
    });
    // the active tab stays in sight in a bar that scrolls sideways (a narrow screen)
    var t = tabs[i];
    if (list.scrollWidth > list.clientWidth + 1) {
      list.scrollLeft = Math.max(0, t.offsetLeft - (list.clientWidth - t.offsetWidth) / 2);
    }
    // the other language opens on the same tab
    langs.forEach(function (a) { a.setAttribute('href', a.getAttribute('data-base') + '#' + panels[i].id); });
  }

  // the page's smooth scrolling is for the links inside a panel; the script's own moves are at once
  function jump(el) {
    try { el.scrollIntoView({ block: 'start', behavior: 'instant' }); } catch (err) { el.scrollIntoView(); }
  }

  // back to the top of the panel when the page was scrolled below the bar
  function toTop() {
    var y = header ? header.offsetTop + header.offsetHeight : 0;
    if (window.pageYOffset > y) {
      try { window.scrollTo({ top: y, behavior: 'instant' }); } catch (err) { window.scrollTo(0, y); }
    }
  }

  function route() {
    var id = '';
    try { id = decodeURIComponent(location.hash.slice(1)); } catch (err) { id = ''; }
    var i = tabOf(id);
    show(i < 0 ? 0 : i);
    if (i >= 0 && panels[i].id !== id) {
      // an element inside a panel: scroll to it now, and again once the page is laid out (on the first load the
      // browser may still be placing the page when this runs)
      var el = document.getElementById(id);
      jump(el);
      if (document.readyState !== 'complete') {
        window.addEventListener('load', function () { jump(el); }, { once: true });
      }
    } else {
      toTop();
    }
  }

  function activate(i, how) {
    var url = '#' + panels[i].id;
    if (location.hash !== url) history[how](null, '', url);
    show(i);
    toTop();
  }

  tabs.forEach(function (a, i) {
    a.addEventListener('click', function (ev) {
      // a click with a modifier (a new tab or window) is left to the browser
      if (ev.button !== 0 || ev.ctrlKey || ev.metaKey || ev.shiftKey || ev.altKey) return;
      ev.preventDefault();
      activate(i, 'pushState');
    });
  });

  list.addEventListener('keydown', function (ev) {
    var i = tabs.indexOf(document.activeElement);
    if (i < 0) return;
    var n = tabs.length, next = -1;
    if (ev.key === 'ArrowRight') next = (i + 1) % n;
    else if (ev.key === 'ArrowLeft') next = (i + n - 1) % n;
    else if (ev.key === 'Home') next = 0;
    else if (ev.key === 'End') next = n - 1;
    if (next < 0) return;
    ev.preventDefault();
    activate(next, 'replaceState');
    tabs[next].focus();
  });

  window.addEventListener('popstate', route);
  window.addEventListener('hashchange', route);
  route();
})();
