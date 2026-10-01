// Injected only by `aquascope-reveal --watch`. Polls the stamp that each
// build writes and reacts when it changes, so a rebuild shows up without
// touching the browser.
//
// A build that succeeded removes build-error.html before bumping the stamp,
// and the page reloads. location.reload() preserves the hash, and reveal
// restores position from it -- you land back on the slide and fragment you
// were looking at.
//
// A build that failed writes build-error.html instead and leaves the deck as
// it was, so the page stays put and shows the error in a modal over it, the
// way a dev server's error overlay does. Esc or a click outside hides the
// modal until the next build; a fixed build reloads the page and it is gone.
(function livereload() {
  var STAMP = "build-stamp.txt";
  var ERROR = "build-error.html";
  var current = null;
  var host = null;

  function get(path) {
    return fetch(path, { cache: "no-store" }).then(function (response) {
      return response.ok ? response.text() : null;
    });
  }

  // The overlay lives in a shadow root so that neither the reveal theme's
  // rules for h1, pre, section and the rest nor the deck's own stylesheets
  // reach into it. Colours for rustc's output are given here too, for a dark
  // background: the fragment's spans read `var(--aq-ansi-*)`.
  var STYLE =
    ":host { all: initial; }" +
    ".backdrop { position: fixed; inset: 0; z-index: 2147483647;" +
    "  background: rgba(0, 0, 0, 0.66); display: flex;" +
    "  align-items: flex-start; justify-content: center; overflow-y: auto;" +
    "  padding: 6vh 16px; box-sizing: border-box;" +
    "  font: 14px/1.5 system-ui, -apple-system, sans-serif; }" +
    ".modal { --aq-ansi-bright-red: #ff6b6b; --aq-ansi-bright-green: #7ee787;" +
    "  --aq-ansi-bright-yellow: #e3b341; --aq-ansi-bright-blue: #79c0ff;" +
    "  --aq-ansi-bright-magenta: #d2a8ff; --aq-ansi-bright-cyan: #56d4dd;" +
    "  --aq-ansi-bright-black: #8b949e; --aq-ansi-bright-white: #f0f6fc;" +
    "  --aq-ansi-red: #ff6b6b; --aq-ansi-green: #7ee787;" +
    "  --aq-ansi-yellow: #e3b341; --aq-ansi-blue: #79c0ff;" +
    "  --aq-ansi-magenta: #d2a8ff; --aq-ansi-cyan: #56d4dd;" +
    "  --aq-ansi-black: #8b949e; --aq-ansi-white: #c9d1d9;" +
    "  position: relative; width: min(960px, 100%); background: #181a1f;" +
    "  color: #d4d4d4; border-top: 6px solid #ff5555; border-radius: 6px;" +
    "  box-shadow: 0 16px 48px rgba(0, 0, 0, 0.5); padding: 20px 28px 24px;" +
    "  box-sizing: border-box; }" +
    ".title { color: #ff5555; font-size: 18px; font-weight: 600;" +
    "  margin: 0 32px 4px 0; }" +
    ".hint { color: #8b949e; font-size: 12px; margin-bottom: 16px; }" +
    ".close { position: absolute; top: 12px; right: 14px; background: none;" +
    "  border: 0; color: #8b949e; font-size: 22px; line-height: 1;" +
    "  cursor: pointer; padding: 4px 8px; }" +
    ".close:hover { color: #f0f6fc; }" +
    ".problem { border-top: 1px solid #30343c; padding: 14px 0 4px; }" +
    "header { white-space: pre-wrap; margin-bottom: 8px; }" +
    ".location { color: #79c0ff; font-family: ui-monospace, SFMono-Regular," +
    "  Menlo, monospace; margin-right: 10px; }" +
    ".message { color: #f0f6fc; }" +
    "pre { margin: 0 0 8px; padding: 12px 14px; background: #0d0f12;" +
    "  border-radius: 4px; overflow-x: auto; white-space: pre;" +
    "  font: 13px/1.45 ui-monospace, SFMono-Regular, Menlo, monospace;" +
    "  color: #d4d4d4; }" +
    "details { margin-bottom: 8px; }" +
    "summary { color: #8b949e; cursor: pointer; font-size: 12px;" +
    "  margin-bottom: 6px; }";

  function hide() {
    if (host) {
      host.remove();
      host = null;
    }
  }

  function show(fragment) {
    hide();
    host = document.createElement("aquascope-build-error");
    var root = host.attachShadow({ mode: "open" });
    var count = (fragment.match(/<section class="problem">/g) || []).length;
    root.innerHTML =
      "<style>" + STYLE + "</style>" +
      '<div class="backdrop"><div class="modal" role="alertdialog"' +
      ' aria-modal="true" aria-labelledby="t">' +
      '<button class="close" title="Dismiss (Esc)">&times;</button>' +
      '<div class="title" id="t">Build failed' +
      (count > 1 ? ": " + count + " problems" : "") + "</div>" +
      '<div class="hint">Fix the source and this page reloads. ' +
      "Esc dismisses.</div>" +
      fragment +
      "</div></div>";
    var backdrop = root.querySelector(".backdrop");
    backdrop.addEventListener("click", function (event) {
      if (event.target === backdrop) {
        hide();
      }
    });
    root.querySelector(".close").addEventListener("click", hide);
    document.documentElement.appendChild(host);
  }

  // Captured on window, ahead of reveal's own listener on document, so that
  // keys pressed at the modal do not also move the deck behind it -- and Esc
  // closes the modal rather than opening reveal's overview.
  window.addEventListener(
    "keydown",
    function (event) {
      if (!host) {
        return;
      }
      event.stopImmediatePropagation();
      if (event.key === "Escape") {
        event.preventDefault();
        hide();
      }
    },
    true
  );

  // On load too: the page may be opened, or reloaded by hand, while the last
  // build is still broken.
  function checkError() {
    return get(ERROR).then(function (fragment) {
      if (fragment !== null) {
        show(fragment);
      }
      return fragment;
    });
  }
  checkError().catch(function () {});

  setInterval(function () {
    get(STAMP)
      .then(function (text) {
        if (text === null) {
          return;
        }
        if (current === null) {
          current = text;
        } else if (text !== current) {
          current = text;
          return checkError().then(function (fragment) {
            if (fragment === null) {
              location.reload();
            }
          });
        }
      })
      .catch(function () {
        // Server momentarily unavailable; try again on the next tick.
      });
  }, 1000);
})();
