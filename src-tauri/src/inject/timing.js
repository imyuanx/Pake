// Estuary: record first-paint milestones in secondary windows and report them to the shell log.
(function () {
  try {
    if (window.top !== window) return;
    var T = {};
    var mark = function (k) { if (!(k in T)) T[k] = Math.round(performance.now()); };
    var mo = new MutationObserver(function () {
      if (document.getElementById("boot")) mark("bootInDom");
      var r = document.getElementById("root");
      if (r && r.firstChild) mark("rootChild");
      if (r && (r.innerText || "").trim().length > 0) mark("rootText");
      if (T.bootInDom != null && !document.getElementById("boot")) mark("bootRemoved");
    });
    mo.observe(document, { childList: true, subtree: true, characterData: true });
    document.addEventListener("DOMContentLoaded", function () { mark("dcl"); });
    window.addEventListener("load", function () { mark("load"); });
    var raf = function () { mark("firstRAF"); };
    if (window.requestAnimationFrame) requestAnimationFrame(raf);
    setTimeout(function () {
      try {
        mo.disconnect();
        performance.getEntriesByType("paint").forEach(function (e) { T[e.name] = Math.round(e.startTime); });
        var n = performance.getEntriesByType("navigation")[0];
        if (n) { T.respEnd = Math.round(n.responseEnd); T.domInteractive = Math.round(n.domInteractive); }
        T.res = performance.getEntriesByType("resource").filter(function (e) { return /\/assets\//.test(e.name); }).map(function (e) { return e.name.split("/").pop() + "@" + Math.round(e.startTime) + "-" + Math.round(e.responseEnd); }).join(" ");
        T.path = location.pathname;
        var inv = window.__TAURI_INTERNALS__ && window.__TAURI_INTERNALS__.invoke;
        if (inv) inv("window_timing", { data: JSON.stringify(T) });
      } catch (e) {}
    }, 6000);
  } catch (e) {}
})();
