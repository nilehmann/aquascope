// Injected only by `aquascope-reveal --watch`. Polls the stamp that each
// rebuild writes and reloads when it changes, so a rebuild shows up without
// touching the browser. location.reload() preserves the hash, and reveal
// restores position from it -- you land back on the slide and fragment you
// were looking at.
(function livereload() {
  var STAMP = "build-stamp.txt";
  var current = null;

  setInterval(function () {
    fetch(STAMP, { cache: "no-store" })
      .then(function (response) {
        return response.ok ? response.text() : null;
      })
      .then(function (text) {
        if (text === null) {
          return;
        }
        if (current === null) {
          current = text;
        } else if (text !== current) {
          location.reload();
        }
      })
      .catch(function () {
        // Server momentarily unavailable; try again on the next tick.
      });
  }, 1000);
})();
