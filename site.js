// The only script of the page: a light / dark switch. It remembers the choice in this browser only.
// It makes no request and sends nothing anywhere.
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
