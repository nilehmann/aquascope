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


  // The Run button on an ```origins block.
  //
  // Those blocks are baked HTML rather than an editor, so nothing renders a
  // Run button for them and there is no CodeMirror document to read. The
  // program is on the element instead, in `data-run-code`: it is not what the
  // slide shows, since hidden lines are missing from the slide and the origin
  // markers are not Rust.
  //
  // Everything below builds the DOM the editor builds -- a `.result-container`
  // holding `pre > code.result` -- so the expand-to-modal button above and
  // every style for the result box apply here without knowing the difference.
  var DEFAULT_RUN_URL = "https://play.rust-lang.org/evaluate.json";

  function runOrigins(block, result) {
    result.innerHTML =
      '<button type="button" class="cm-button result-close" title="Hide output">✕</button>' +
      '<pre><code class="result hljs language-bash">Running...</code></pre>';
    result.querySelector(".result-close").addEventListener("click", function () {
      result.innerHTML = "";
    });

    var code = result.querySelector(".result");
    fetch(window.AQUASCOPE_RUN_URL || DEFAULT_RUN_URL, {
      headers: { "Content-Type": "application/json" },
      method: "POST",
      mode: "cors",
      body: JSON.stringify({
        version: "stable",
        optimize: "0",
        code: block.getAttribute("data-run-code"),
        edition: "2021"
      })
    })
      .then(function (response) {
        return response.json();
      })
      .then(function (response) {
        if (response.result.trim() === "") {
          code.innerText = "No output";
          code.classList.add("result-no-output");
        } else {
          // The endpoint's contract: `result` is HTML, which is what carries
          // rustc's colours. Same trust as the editor's own run.
          code.innerHTML = response.result;
          code.classList.remove("result-no-output");
        }
      })
      .catch(function (error) {
        code.innerText = "Playground Communication: " + error.message;
      });
  }

  // Both the controls and the output go inside `.origins-block`, which is what
  // `.aquascope` is to an editor: the positioned element carrying the frame.
  // Not inside the `pre` -- it carries `.hljs`, whose `overflow-x: auto` makes
  // it a scroll box that clips absolutely-positioned children and eats the
  // part of a button that hangs past its edge -- and not as a sibling of the
  // block, which would put the output outside the border.
  function addRunButtons(slide) {
    var blocks = slide.querySelectorAll(".origins-block[data-run-code]");
    for (var i = 0; i < blocks.length; i++) {
      var block = blocks[i];
      if (block.dataset.runReady) {
        continue;
      }
      block.dataset.runReady = "1";

      var result = document.createElement("div");
      result.className = "result-container";
      block.appendChild(result);

      var button = document.createElement("button");
      button.type = "button";
      button.className = "cm-button origins-run";
      button.title = "Compile and run";
      button.textContent = "▶";
      button.addEventListener(
        "click",
        (function (block, result) {
          return function () {
            runOrigins(block, result);
          };
        })(block, result)
      );

      var controls = document.createElement("div");
      controls.className = "top-right";
      controls.appendChild(button);
      block.appendChild(controls);
    }
  }

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

  // Moves the highlight in each ```origins block on `slide` to the step the
  // slide is on. A `[[=N:` highlight is a reveal fragment of its own, so reveal
  // already marks it `.visible` from step N on; the one lit is the latest of
  // those, together with any others on the same step. The block's `.ohl-end`
  // fragment is the step after the last one, from which nothing timed is lit
  // and the `[[=:` highlights, lit from the start, are lit again.
  //
  // The indices are read after reveal has renumbered them, which it does to
  // every slide's fragments, so they compare in the same order as written.
  function focusHighlights(slide) {
    if (!slide) {
      return;
    }
    var blocks = slide.querySelectorAll(".origins-block");
    Array.prototype.forEach.call(blocks, function (block) {
      var all = block.querySelectorAll(".ohl");
      if (all.length === 0) {
        return;
      }
      var index = function (el) {
        return Number(el.getAttribute("data-fragment-index"));
      };
      var latest = null;
      var timed = block.querySelectorAll(".ohl.fragment, .ohl-end");
      Array.prototype.forEach.call(timed, function (el) {
        if (
          el.classList.contains("visible") &&
          (latest === null || index(el) > index(latest))
        ) {
          latest = el;
        }
      });
      var stepping = latest !== null && !latest.classList.contains("ohl-end");
      Array.prototype.forEach.call(all, function (el) {
        var lit = el.classList.contains("fragment")
          ? stepping && index(el) === index(latest)
          : !stepping;
        el.classList.toggle("on", lit);
        frame(el, lit);
      });
      // The callouts follow the same rule: a step's note while its
      // highlights are lit, the `[=]` note while the `[[=:` ones are.
      var notes = block.querySelectorAll(".ohl-note");
      Array.prototype.forEach.call(notes, function (note) {
        var lit = note.classList.contains("fragment")
          ? stepping && index(note) === index(latest)
          : !stepping;
        note.classList.toggle("on", lit);
      });
    });
  }

  var SVG = "http://www.w3.org/2000/svg";

  // Draws, moves or hides the one tinted shape behind highlight `el`.
  //
  // The shape follows the code the way an editor draws a selection over
  // several lines: the lines share one left edge and one right edge, except
  // that a highlight starting partway through a line starts there, and one
  // ending partway through a line ends there -- so `spawn(|| {` … `});` with
  // the closure lit leaves `spawn(` and `);` outside. A highlight over whole
  // lines is a plain rectangle. Indentation and the newlines between lines are
  // not code and never widen it.
  //
  // Measured each time it is lit, since only a slide on screen has a layout,
  // and again on resize, when the code reflows.
  function frame(el, lit) {
    var box = el.ohlBox;
    if (!box) {
      box = el.ohlBox = document.createElementNS(SVG, "svg");
      box.setAttribute("class", "ohl-box");
      box.appendChild(document.createElementNS(SVG, "path"));
      el.closest("pre").appendChild(box);
    }
    box.classList.toggle("on", lit);
    if (!lit) {
      return;
    }

    var rows = lines(el);
    if (rows.length === 0) {
      box.classList.remove("on");
      return;
    }

    var pre = box.parentNode;
    var em = parseFloat(getComputedStyle(pre).fontSize);
    // How far the tint reaches past the code.
    var pad = 0.2 * em;
    // A notch is drawn between the highlight and code that touches it -- the
    // `(` before `|| {` -- so its edge keeps to the gap between the two glyphs
    // rather than reaching the full padding, which would tint one of them.
    var notch = 0.06 * em;

    var n = rows.length;
    var first = rows[0];
    var last = rows[n - 1];
    var mid = startsMidLine(el, pre);
    var ends = endsMidLine(el, pre);

    // The shared edges are those of the lines the notches leave whole.
    var left = Math.min.apply(
      null,
      (mid && n > 1 ? rows.slice(1) : rows).map(function (r) {
        return r.left;
      })
    );
    var right = Math.max.apply(
      null,
      (ends && n > 1 ? rows.slice(0, -1) : rows).map(function (r) {
        return r.right;
      })
    );
    var topLeft = mid ? first.left : left;
    var bottomRight = ends ? last.right : right;
    // Two lines that do not overlap -- the end of one and the start of the
    // next -- would make two shapes. Square off the bottom to keep one.
    if (n === 2 && bottomRight <= topLeft) {
      bottomRight = right;
    }

    // Where one line ends and the next begins: halfway through the leading.
    var below = function (i) {
      return (rows[i].bottom + rows[i + 1].top) / 2;
    };
    var top = first.top - pad;
    var bottom = last.bottom + pad;
    var points;
    var tl = mid ? topLeft - notch : topLeft - pad;
    var br = ends ? bottomRight + notch : bottomRight + pad;
    if (n === 1) {
      points = [[tl, top], [br, top], [br, bottom], [tl, bottom]];
    } else {
      var yt = below(0);
      var yb = below(n - 2);
      points = [
        [tl, top], [right + pad, top],
        [right + pad, yb], [br, yb],
        [br, bottom], [left - pad, bottom],
        [left - pad, yt], [tl, yt]
      ];
    }
    points = corners(points);

    // The svg covers the shape exactly. Its position is in the coordinates
    // of the `pre`, which it is positioned against: from the padding edge,
    // and moving with the content when the `pre` scrolls.
    var xs = points.map(function (p) { return p[0]; });
    var ys = points.map(function (p) { return p[1]; });
    var x0 = Math.min.apply(null, xs);
    var y0 = Math.min.apply(null, ys);
    var width = Math.max.apply(null, xs) - x0;
    var height = Math.max.apply(null, ys) - y0;
    var origin = pre.getBoundingClientRect();
    box.style.left = x0 - origin.left - pre.clientLeft + pre.scrollLeft + "px";
    box.style.top = y0 - origin.top - pre.clientTop + pre.scrollTop + "px";
    box.setAttribute("width", width);
    box.setAttribute("height", height);

    box.firstChild.setAttribute(
      "d",
      rounded(
        points.map(function (p) { return [p[0] - x0, p[1] - y0]; }),
        0.25 * em
      )
    );
  }

  // The highlighted code in `el`, one rectangle per line: every non-blank
  // run of text, and every origin box on a single line, which reaches past
  // its text by its padding and border. A box over several lines is left to
  // its text, since its later lines start at the margin.
  function lines(el) {
    var rects = [];
    var walker = document.createTreeWalker(el, NodeFilter.SHOW_TEXT);
    var range = document.createRange();
    for (var node = walker.nextNode(); node; node = walker.nextNode()) {
      var re = /\S+(?:[^\S\n]+\S+)*/g;
      var match;
      while ((match = re.exec(node.data))) {
        range.setStart(node, match.index);
        range.setEnd(node, match.index + match[0].length);
        Array.prototype.push.apply(rects, range.getClientRects());
      }
    }
    Array.prototype.forEach.call(
      el.querySelectorAll(".oframe, .oname, .oexact"),
      function (inner) {
        var r = inner.getClientRects();
        if (r.length === 1) {
          rects.push(r[0]);
        }
      }
    );

    rects.sort(function (a, b) {
      return a.top - b.top;
    });
    var rows = [];
    rects.forEach(function (r) {
      var row = rows[rows.length - 1];
      var centre = (r.top + r.bottom) / 2;
      if (row && centre >= row.top && centre <= row.bottom) {
        row.left = Math.min(row.left, r.left);
        row.right = Math.max(row.right, r.right);
        row.top = Math.min(row.top, r.top);
        row.bottom = Math.max(row.bottom, r.bottom);
      } else {
        rows.push({ left: r.left, right: r.right, top: r.top, bottom: r.bottom });
      }
    });
    return rows;
  }

  // Whether code precedes `el` on its first line, or follows it on its last.
  function startsMidLine(el, pre) {
    var range = document.createRange();
    range.setStart(pre, 0);
    range.setEndBefore(el);
    var text = range.toString();
    return /\S/.test(text.slice(text.lastIndexOf("\n") + 1));
  }

  function endsMidLine(el, pre) {
    var range = document.createRange();
    range.setStartAfter(el);
    range.setEnd(pre, pre.childNodes.length);
    return /\S/.test(range.toString().split("\n")[0]);
  }

  // Drops the corners a notch that is not there leaves behind: a point equal
  // to the one before it, or in line with its neighbours.
  function corners(points) {
    var out = points.filter(function (p, i) {
      var q = points[(i + points.length - 1) % points.length];
      return Math.abs(p[0] - q[0]) > 0.5 || Math.abs(p[1] - q[1]) > 0.5;
    });
    return out.filter(function (p, i) {
      var a = out[(i + out.length - 1) % out.length];
      var b = out[(i + 1) % out.length];
      var flatX = Math.abs(a[0] - p[0]) < 0.5 && Math.abs(b[0] - p[0]) < 0.5;
      var flatY = Math.abs(a[1] - p[1]) < 0.5 && Math.abs(b[1] - p[1]) < 0.5;
      return !flatX && !flatY;
    });
  }

  // A closed path through `points` with each corner rounded to `radius`, or
  // to half the shorter side next to it where that is less.
  function rounded(points, radius) {
    var n = points.length;
    var d = "";
    for (var i = 0; i < n; i++) {
      var p = points[i];
      var a = points[(i + n - 1) % n];
      var b = points[(i + 1) % n];
      var la = Math.hypot(p[0] - a[0], p[1] - a[1]);
      var lb = Math.hypot(b[0] - p[0], b[1] - p[1]);
      var r = Math.min(radius, la / 2, lb / 2);
      var start = [p[0] + ((a[0] - p[0]) / la) * r, p[1] + ((a[1] - p[1]) / la) * r];
      var end = [p[0] + ((b[0] - p[0]) / lb) * r, p[1] + ((b[1] - p[1]) / lb) * r];
      d += (i === 0 ? "M" : "L") + start[0] + " " + start[1];
      d += "Q" + p[0] + " " + p[1] + " " + end[0] + " " + end[1];
    }
    return d + "Z";
  }

  // Keeps a type tooltip on screen. It is centred under its expression,
  // which for a long signature -- `thread::spawn`'s, bounds and all -- runs
  // past the edge of the window, and near the bottom of the window it would
  // hang below it. The tooltip is a pseudo-element and cannot be measured,
  // so an invisible copy with the same styles is: the expression is given
  // the shift that brings the tooltip back inside, and `ty-above` when there
  // is room for it above but not below.
  var TY_MARGIN = 8;
  document.addEventListener("mouseover", function (event) {
    var ty = event.target.closest && event.target.closest(".ty");
    if (!ty) {
      return;
    }
    var probe = document.createElement("span");
    probe.className = "ty-measure";
    probe.textContent = ty.getAttribute("data-type");
    ty.appendChild(probe);
    var size = probe.getBoundingClientRect();
    var width = size.width;
    probe.remove();

    var box = ty.getBoundingClientRect();
    var left = box.left + box.width / 2 - width / 2;
    var max = window.innerWidth - TY_MARGIN - width;
    var shift = Math.max(TY_MARGIN, Math.min(left, max)) - left;
    // Set on the expression itself, never inherited: an expression inside
    // another would otherwise take its parent's shift.
    ty.style.setProperty("--ty-shift", shift + "px");
    // Opened above instead when there is no room for it below.
    var below = box.bottom + size.height + TY_MARGIN <= window.innerHeight;
    ty.classList.toggle("ty-above", !below && box.top - size.height > TY_MARGIN);
  });

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
      addRunButtons(Reveal.getCurrentSlide());
      focusHighlights(Reveal.getCurrentSlide());
    });

    Reveal.on("slidechanged", function (event) {
      hydrate(event.currentSlide);
      addRunButtons(event.currentSlide);
      focusHighlights(event.currentSlide);
    });

    ["fragmentshown", "fragmenthidden"].forEach(function (name) {
      Reveal.on(name, function () {
        focusHighlights(Reveal.getCurrentSlide());
      });
    });

    // reveal's own `resize` event is not sent under `disableLayout`.
    window.addEventListener("resize", function () {
      focusHighlights(Reveal.getCurrentSlide());
    });

    // The code's web font changes every measurement when it arrives.
    if (document.fonts) {
      document.fonts.ready.then(function () {
        focusHighlights(Reveal.getCurrentSlide());
      });
    }
  });
})();
