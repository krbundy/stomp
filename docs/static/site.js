// Search, theme toggle, mobile menu, copy buttons and the "on this page" highlight.
(function () {
  "use strict";

  // ------------------------------------------------------------ theme
  var root = document.documentElement;
  var toggle = document.querySelector(".theme-toggle");
  function currentTheme() {
    if (root.dataset.theme) return root.dataset.theme;
    return window.matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light";
  }
  if (toggle) {
    toggle.addEventListener("click", function () {
      var next = currentTheme() === "dark" ? "light" : "dark";
      root.dataset.theme = next;
      try { localStorage.setItem("stomp-theme", next); } catch (e) {}
    });
  }

  // ------------------------------------------------------------ mobile menu
  var menu = document.querySelector(".menu-button");
  var sidebar = document.getElementById("sidebar");
  if (menu && sidebar) {
    menu.addEventListener("click", function () {
      var open = sidebar.classList.toggle("open");
      menu.setAttribute("aria-expanded", open ? "true" : "false");
    });
  }

  // ------------------------------------------------------------ copy buttons
  document.querySelectorAll(".prose pre").forEach(function (pre) {
    var code = pre.querySelector("code");
    if (!code || (code.dataset.lang || "") === "plain") return; // output, not commands
    var button = document.createElement("button");
    button.type = "button";
    button.className = "copy";
    button.textContent = "Copy";
    button.addEventListener("click", function () {
      var text = code.innerText.replace(/\n$/, "");
      var done = function () {
        button.textContent = "Copied";
        setTimeout(function () { button.textContent = "Copy"; }, 1500);
      };
      if (navigator.clipboard) {
        navigator.clipboard.writeText(text).then(done, function () {});
      }
    });
    pre.appendChild(button);
  });

  // ------------------------------------------------------------ on this page
  var tocLinks = Array.prototype.slice.call(document.querySelectorAll(".toc a"));
  if (tocLinks.length && "IntersectionObserver" in window) {
    var byId = {};
    tocLinks.forEach(function (a) { byId[decodeURIComponent(a.hash.slice(1))] = a; });
    var visible = {};
    var observer = new IntersectionObserver(function (entries) {
      entries.forEach(function (e) { visible[e.target.id] = e.isIntersecting; });
      var first = Object.keys(byId).find(function (id) { return visible[id]; });
      if (first) {
        tocLinks.forEach(function (a) { a.classList.remove("active"); });
        byId[first].classList.add("active");
      }
    }, { rootMargin: "-60px 0px -70% 0px" });
    Object.keys(byId).forEach(function (id) {
      var h = document.getElementById(id);
      if (h) observer.observe(h);
    });
  }

  // ------------------------------------------------------------ search
  var input = document.getElementById("search");
  var results = document.getElementById("search-results");
  if (!input || !results) return;
  var index = null;
  var loading = false;
  var active = -1;

  function script(src, done) {
    var s = document.createElement("script");
    s.src = src;
    s.onload = done;
    document.head.appendChild(s);
  }

  // The index is large, so it's fetched the first time someone searches.
  function load() {
    if (index) return index;
    if (window.elasticlunr && window.searchIndex) {
      index = elasticlunr.Index.load(window.searchIndex);
      return index;
    }
    if (!loading) {
      loading = true;
      script(input.dataset.lunr, function () {
        script(input.dataset.index, function () {
          load();
          if (input.value) render();
        });
      });
    }
    return null;
  }

  function escape(s) {
    return s.replace(/[&<>"']/g, function (c) {
      return { "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c];
    });
  }

  // The index holds HTML-escaped text; turn it back into plain text.
  var decoder = document.createElement("textarea");
  function decode(s) {
    decoder.innerHTML = s;
    return decoder.value;
  }

  // A short excerpt around the first matching word.
  function excerpt(body, terms) {
    if (!body) return "";
    body = decode(body);
    var lower = body.toLowerCase();
    var at = -1;
    for (var i = 0; i < terms.length && at < 0; i++) at = lower.indexOf(terms[i]);
    var start = Math.max(0, at < 0 ? 0 : at - 60);
    var text = body.slice(start, start + 180).replace(/\s+/g, " ").trim();
    return (start > 0 ? "…" : "") + escape(text) + "…";
  }

  function render() {
    var q = input.value.trim();
    active = -1;
    if (!q) { results.hidden = true; results.innerHTML = ""; return; }
    var idx = load();
    if (!idx) { results.hidden = false; results.innerHTML = "<p>Search is loading…</p>"; return; }
    var hits = idx.search(q, {
      fields: { title: { boost: 3 }, description: { boost: 2 }, body: { boost: 1 } },
      bool: "AND",
      expand: true
    }).slice(0, 8);
    var terms = q.toLowerCase().split(/\s+/);
    if (!hits.length) {
      results.innerHTML = "<p>No pages match “" + escape(q) + "”.</p>";
    } else {
      results.innerHTML = hits.map(function (h) {
        var doc = idx.documentStore.getDoc(h.ref) || {};
        return '<a href="' + h.ref + '"><strong>' + escape(decode(doc.title || h.ref)) + "</strong><span>" +
          excerpt(doc.body || doc.description || "", terms) + "</span></a>";
      }).join("");
    }
    results.hidden = false;
  }

  function move(step) {
    var links = results.querySelectorAll("a");
    if (!links.length) return;
    active = (active + step + links.length) % links.length;
    links.forEach(function (a, i) { a.classList.toggle("active", i === active); });
    links[active].scrollIntoView({ block: "nearest" });
  }

  input.addEventListener("input", render);
  input.addEventListener("focus", function () { load(); if (input.value) render(); });
  input.addEventListener("keydown", function (e) {
    if (e.key === "ArrowDown") { e.preventDefault(); move(1); }
    else if (e.key === "ArrowUp") { e.preventDefault(); move(-1); }
    else if (e.key === "Enter") {
      var links = results.querySelectorAll("a");
      var target = links[active >= 0 ? active : 0];
      if (target) window.location.href = target.href;
    } else if (e.key === "Escape") { input.value = ""; render(); input.blur(); }
  });
  document.addEventListener("click", function (e) {
    if (!results.contains(e.target) && e.target !== input) results.hidden = true;
  });
  document.addEventListener("keydown", function (e) {
    var tag = (document.activeElement && document.activeElement.tagName) || "";
    if (e.key === "/" && tag !== "INPUT" && tag !== "TEXTAREA") { e.preventDefault(); input.focus(); }
  });
})();
