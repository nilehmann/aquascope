// Glue between reveal.js and the prerendered Aquascope embed bundle.
//
// Two things here are load-bearing and not obvious:
//
//  1. embed.iife.js registers its own `load` listener that hydrates
//     `document.body` wholesale. We do not want that: reveal keeps every
//     off-screen slide at `display: none`, where getBoundingClientRect()
//     returns zeros, and Aquascope's arrows are positioned from those rects --
//     so hydrating a hidden slide lays its arrows out at the wrong place.
//     Injecting the bundle *after* the load event has already fired means that
//     listener never runs, and we hydrate each slide as it becomes visible
//     instead. initAquascopeBlocks is idempotent (it strips the
//     `aquascope-embed` class as it goes), so re-visiting a slide is a no-op.
//
//  2. reveal is initialized with `disableLayout`, so it never applies a
//     `transform` to the slides. Aquascope draws its arrows in document
//     coordinates and does not compensate for a scaled ancestor, which is what
//     reveal's default fit-to-screen layout would give us.

(function aquascopeReveal() {
  var EMBED_SRC = "aquascope/embed.iife.js";

  function whenLoaded(fn) {
    if (document.readyState === "complete") {
      fn();
    } else {
      window.addEventListener("load", fn, false);
    }
  }

  var embedPromise = null;
  function loadEmbed() {
    if (!embedPromise) {
      embedPromise = new Promise(function (resolve, reject) {
        var script = document.createElement("script");
        script.src = EMBED_SRC;
        script.onload = resolve;
        script.onerror = function () {
          reject(new Error("Failed to load " + EMBED_SRC));
        };
        document.head.appendChild(script);
      });
    }
    return embedPromise;
  }

  function hydrate(slide) {
    if (!slide) {
      return;
    }
    loadEmbed().then(function () {
      window.initAquascopeBlocks(slide);
    }, console.error);
  }

  // A run's output, blown up over the slide.
  //
  // The editor owns the result block and rebuilds it wholesale on every run,
  // so the expand button is (re-)attached by observing the DOM rather than
  // rendered once. Everything below is additive: nothing here modifies the
  // output itself, and clearing the output with the editor's own close button
  // takes the expand button with it.
  var modal = null;
  var opener = null;

  function openModal(result) {
    closeModal();

    var root = document.createElement("div");
    root.className = "aq-output-modal";

    var panel = document.createElement("div");
    panel.className = "aq-output-modal-panel";
    // Focusable so the arrow keys scroll the panel rather than doing nothing.
    panel.tabIndex = -1;

    var pre = document.createElement("pre");
    var code = document.createElement("code");
    // A snapshot of the output as it stands, colours and all. Re-running while
    // the modal is open leaves the snapshot alone; the next open picks it up.
    //
    // Deliberately without the `result` class the inline block carries: a deck
    // styling its inline output -- `pre > .result { font-size: 0.8em }` is the
    // obvious thing to write -- would otherwise shrink the modal too, and win,
    // since the deck's stylesheet is linked after this one. The colours are
    // inline styles on the spans, so nothing here depends on that class.
    code.innerHTML = result.innerHTML;

    pre.appendChild(code);
    panel.appendChild(pre);
    root.appendChild(panel);

    // Only a press that starts on the backdrop dismisses, so selecting text in
    // the output and releasing outside the panel does not close it.
    root.addEventListener("mousedown", function (event) {
      if (event.target === root) {
        closeModal();
      }
    });

    opener = document.activeElement;
    document.body.appendChild(root);
    panel.focus();
    modal = root;
  }

  function closeModal() {
    if (!modal) {
      return;
    }
    modal.parentNode.removeChild(modal);
    modal = null;
    if (opener && opener.focus) {
      opener.focus();
    }
    opener = null;
  }

  // Capture phase, because reveal listens on the document and would otherwise
  // see these first: Escape opens the slide overview, and the arrow keys change
  // slide. While the modal is up, every key belongs to it -- but only Escape is
  // consumed, so the browser still scrolls the panel with the arrows and space.
  window.addEventListener(
    "keydown",
    function (event) {
      if (!modal) {
        return;
      }
      if (event.key === "Escape") {
        event.preventDefault();
        closeModal();
      }
      event.stopPropagation();
    },
    true
  );

  function decorate(container) {
    if (container.querySelector(".result-expand")) {
      return;
    }
    var result = container.querySelector(".result");
    if (!result) {
      return;
    }

    var button = document.createElement("button");
    button.type = "button";
    // `cm-button` is the editor's own button styling, shared with the ✕.
    button.className = "cm-button result-expand";
    button.title = "Show output full size";
    button.textContent = "⤢";
    button.addEventListener("click", function () {
      openModal(result);
    });

    // The editor pins its ✕ to the corner of the result box on its own. Put
    // both buttons in a `.top-right` row instead -- the same element the code
    // and interpreter controls sit in -- so the result box's controls are
    // spaced and revealed on hover exactly like every other block's. Moving
    // the ✕ keeps its click handler.
    var controls = document.createElement("div");
    controls.className = "top-right";
    controls.appendChild(button);

    var close = container.querySelector(".result-close");
    if (close) {
      controls.appendChild(close);
    }

    container.appendChild(controls);
  }

  // Appending the button is itself a mutation; `decorate` is idempotent, so the
  // second pass finds the button and stops.
  function watchForOutput() {
    new MutationObserver(function (mutations) {
      for (var i = 0; i < mutations.length; i++) {
        var target = mutations[i].target;
        if (target.nodeType !== 1 || !target.closest) {
          continue;
        }
        var container = target.closest(".result-container");
        if (container) {
          decorate(container);
        }
      }
    }).observe(document.body, { childList: true, subtree: true });
  }

  // The marker class sits on the <button> for the editor controls but on the
  // <i> for the interpreter controls -- and Font Awesome rewrites that <i> into
  // an <svg>, which has no click(). Always drive the enclosing button.
  function step(className) {
    var slide = Reveal.getCurrentSlide();
    if (!slide) {
      return;
    }
    var el = slide.getElementsByClassName(className)[0];
    if (!el) {
      return;
    }
    var button = el.closest("button") || el;
    if (button.click) {
      button.click();
    }
  }

  // Deck-level reveal options from the markdown front matter. Shallow-merged
  // over the defaults below, except `keyboard`, which is merged key by key so
  // that a deck adding a shortcut does not silently drop n/p.
  function withDeckOptions(options) {
    var deck = window.AQUASCOPE_REVEAL_OPTIONS || {};
    Object.keys(deck).forEach(function (key) {
      if (key === "keyboard") {
        Object.assign(options.keyboard, deck.keyboard);
      } else {
        options[key] = deck[key];
      }
    });
    return options;
  }

  whenLoaded(function () {
    watchForOutput();

    Reveal.initialize(withDeckOptions({
      // Needed by --watch: reloading restores position from the hash.
      hash: true,
      // No transform on the slides, so Aquascope's document-coordinate arrows
      // land where it expects. Layout comes from aquascope-reveal.css instead.
      disableLayout: true,
      // reveal binds N and P to next/prev slide by default. Rebind them to
      // step the diagram on the current slide.
      keyboard: {
        78: function () {
          step("step-next");
        },
        80: function () {
          step("step-back");
        }
      },
      plugins: [RevealHighlight, RevealNotes]
    })).then(function () {
      hydrate(Reveal.getCurrentSlide());
    });

    Reveal.on("slidechanged", function (event) {
      hydrate(event.currentSlide);
    });
  });
})();
