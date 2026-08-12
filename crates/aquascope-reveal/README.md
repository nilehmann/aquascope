# aquascope-reveal

Builds a [reveal.js](https://revealjs.com) deck from a markdown file, running
Aquascope over every ` ```aquascope ` block at build time and baking the results
into the page. The output is static: presenting it needs no Aquascope server.

```
aquascope-reveal lecture.md -o dist --serve
```

The output is a plain directory of static files, so it can also be served by
anything else, or committed and hosted.

## Front matter

Deck-level settings live in a YAML block at the top of the markdown. The shape
follows [reveal-md][reveal-md], the usual markdown-first wrapper around
reveal.js, so its documentation carries over: tool-level keys at the top,
reveal's own options nested under `revealOptions`, which is merged into the
object passed to `Reveal.initialize`.

```markdown
---
title: Ownership and Borrowing
theme: white
revealOptions:
  navigationMode: default
  slideNumber: "c/t"
---

# First slide
```

The block is recognised only when the file *starts* with `---`; anywhere else
that line is a slide separator. `--title` and `--theme` override it.

Unknown keys are an error rather than being ignored, as are options the rest of
the pipeline depends on: `disableLayout` (Aquascope's arrows are drawn in
document coordinates and would be misplaced by reveal's scaling) and `plugins`
(live JavaScript objects, not expressible in YAML). `keyboard` is merged key by
key rather than replaced, so adding a shortcut does not drop `n`/`p`.

[reveal-md]: https://github.com/webpro/reveal-md

## Authoring

The markdown is the same dialect `mdbook-aquascope` accepts, so decks written
for mdBook port over unchanged. Slides are separated by lines containing only
`---`, and vertical slides (a stack you descend into with Down) by `--`. Both
must sit between blank lines, which keeps them from being read as setext
headings; separators inside fenced code blocks are ignored.

```markdown
## Boxes are automatically deallocated

```aquascope,interpreter,horizontal
fn main() {
    let a = Box::new([0; 1_000_000]); `[]`
    let b = a; `[]`
}
```

<div class="fragment">Appears on the first Right press.</div>

---

## Next slide
```

Incremental reveal is reveal.js's own: `class="fragment"` shows an element on
the next Right press, `data-fragment-index` overrides the order. This crate adds
one variant, `class="fragment collapse"`, which takes up no space until it is
shown, for content that would otherwise leave a gap in the layout.

Aquascope owns the `step`, `step-marker`, `step-header`, `step-button` and
`step-table-*` class names. Do not reuse that prefix for slide machinery.

## Keys

Slide navigation is reveal's own: Left/Right between slides, Up/Down within a
vertical stack. For one key to walk the whole deck, descending into stacks as it
goes, set `navigationMode: linear` in the front matter.

`n` and `p` step the Aquascope diagram on the current slide. These are rebound
from reveal's defaults, where they are next/previous slide -- the one navigation
behaviour this crate does change, since stepping a diagram has no other key.

## Options

| Flag | Effect |
| --- | --- |
| `-o, --out-dir` | Where to write the deck. Defaults to `dist`. |
| `--title` | Page title. Defaults to the input file stem. |
| `--theme` | reveal.js theme name. Defaults to `white`. |
| `--reveal-dir` | Path to a local reveal.js package (the directory holding `dist` and `plugin`, e.g. `node_modules/reveal.js`). Without it the deck loads reveal.js from a CDN and will not work offline. |
| `--css` | Extra stylesheet, copied in and linked after Aquascope's. Repeatable. |
| `--js` | Extra script, copied in and loaded last. Repeatable. |
| `--static` | Directory copied verbatim to `<out-dir>/<name>`, for images and fonts the slides reference by path. Repeatable. |
| `--serve [PORT]` | Serve the deck on `127.0.0.1:PORT`, default 4321. |
| `--watch` | Rebuild on change and reload the open page. See below. |
| `--poll-ms` | Poll interval while watching. Defaults to 400. |

## Serving

`--serve` runs a small built-in static server on loopback, so previewing needs
nothing else installed. It is a preview server and nothing more: one thread per
connection, no keep-alive, no ranges, no compression, and every response is
`Cache-Control: no-store` so a rebuilt asset is never served from cache.
Requests that try to escape the output directory are rejected, including
percent-encoded forms of `..`.

## Watching

`--watch` rebuilds whenever the deck, any `--css`/`--js` file, or anything
inside a `--static` directory changes, and injects a script that reloads the
open page once the rebuild lands. Combine the two for the usual authoring loop:

```
aquascope-reveal lecture.md --static img --serve --watch
```

Reloading preserves the hash and reveal restores position from it, so you come
back to the slide *and* fragment you were on. Rebuilds are fast because the
Aquascope cache is keyed on each block's code and config -- only blocks you
actually edited are re-analyzed.

A failing build prints the error and leaves the watcher running; only the very
first build exits non-zero. The reload script and `build-stamp.txt` are written
only under `--watch`, so a plain build stays clean.

Changes are detected by polling modification times rather than with inotify, to
keep the crate free of a filesystem-notification dependency. The file list is
re-collected every tick, so added and deleted files count as changes too.

## How the embedding works

`embed.iife.js` exposes one global, `window.initAquascopeBlocks(root)`, which
scans a DOM subtree for `div.aquascope-embed` and hydrates each one from its
`data-code`, `data-annotations`, `data-operations`, `data-responses`,
`data-config` and `data-no-interact` attributes. This crate produces those divs
with `mdbook-aquascope`'s own preprocessor, so the analysis, the fence config
and the `` `[]` ``/`` `(` `)` ``/`` `{` `}` `` annotation markers behave
identically to mdBook.

Two constraints shape the glue in `assets/aquascope-reveal.js`:

- **Slides are hydrated as they become visible, not on load.** reveal keeps
  off-screen slides at `display: none`, where `getBoundingClientRect()` returns
  zeros, and Aquascope positions its pointer arrows from those rects. The embed
  bundle is injected *after* the `load` event has fired so that its own
  hydrate-everything listener never runs. `initAquascopeBlocks` is idempotent —
  it strips the `aquascope-embed` class as it goes — so revisiting a slide is
  free.

- **reveal's layout is disabled** (`disableLayout: true`), and slides are sized
  from CSS instead. reveal normally fits a fixed pixel canvas to the window with
  a `transform: scale()`; Aquascope draws arrows in document coordinates without
  compensating for a scaled ancestor, so they would land in the wrong place.

Note that Aquascope's arrows are drawn by leader-line, which
[does not render on Firefox](https://github.com/anseki/leader-line/issues/180).
Present in a Chromium-based browser.

## Building

`build.rs` copies the embed bundle out of
`frontend/packages/aquascope-embed/dist/`, so run `depot build` in `frontend`
before `cargo install --path crates/aquascope-reveal`.
