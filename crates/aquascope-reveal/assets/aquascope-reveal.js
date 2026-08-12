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
