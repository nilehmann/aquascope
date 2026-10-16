// Stands in for reveal.js's highlight plugin, which carries every
// highlight.js grammar -- 0.9 MB of them. aquascope-reveal includes the
// highlight.js core and only the grammars the deck's fences name, ahead of
// this. Each fenced block that names one of those is highlighted; a block
// that names none is left plain.
window.RevealHighlight = {
  id: "highlight",
  init: function () {
    document
      .querySelectorAll(".reveal pre code[class*='language-']")
      .forEach(function (el) {
        var match = el.className.match(/(?:^|\s)language-(\S+)/);
        if (match && hljs.getLanguage(match[1])) {
          hljs.highlightElement(el);
        }
      });
  }
};
