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

## Origin boxes

A deck teaching lifetimes wants to draw a coloured box around the code a
reference borrows from, and to draw the lifetime in a type as that same box. An
```` ```origins ```` fence holds Rust with markers saying where those boxes go,
and is rendered at build time into a syntax-highlighted `<pre>`:

`````markdown
```origins
fn longest<'?a>(v1: &'?a Vec<i32>, v2: &'?a Vec<i32>) -> &'?a Vec<i32> {
    if v1.len() > v2.len() { v1 } else { v2 }
}

let v1: Vec<i32> = [[a:vec![1, 2, 3]:]];
let v2: Vec<i32> = [[a:vec![4, 5, 6]:]];
let r: &'!a Vec<i32> = longest(&v1, &v2);
```
`````

**No box appears without a marker.** A bare `'a` is ordinary Rust syntax, so
every box on the slide is one the source asked for -- which is what lets a
single block hold a generic definition beside the concrete origin a caller
instantiates it with, as above: the definition's `'a` is a variable, the
caller's is the origin the two `vec!`s live in.

| marker | draws |
| --- | --- |
| `'a` | a lifetime as ordinary syntax: no box |
| `'?a` | a **neutral box**: a variable ranging over origins, which is what a generic lifetime parameter on a definition is |
| `'!a` | a box in **origin `a`'s colour**: the concrete origin `a` |
| `[[a:TEXT:]]` | `TEXT` framed in origin `a`'s box; nests |
| `[[?:TEXT:]]` | `TEXT` framed in a box with no origin colour |
| `[[*:TEXT:]]` | a tight dashed box: what is *actually* borrowed, where the origin around it over-approximates |

The sigils are sugar: `'!a` is `[[a:'a:]]` and `'?a` is `[[?:'a:]]`. They exist
because a deck writes far more lifetimes than frames, and a bracket around each
one would bury the code. The general form still says something the sigils
cannot -- `[[b:'a:]]` boxes a lifetime *named* `a` in origin *b*'s colour, so a
lifetime's spelling need not be its origin's letter.

`'?` and `'!` are markers only when an identifier character follows, so
`let c = '!';` is still a char literal.

The frame close is `:]]` rather than `]]` because framed text usually ends in a
bracket, as in `vec![1, 2, 3]`. Everything between the colons is kept verbatim,
spaces included: it lands inside the box, so padding there would shift the code
away from the lines around it.

### Compiling, and the Run button

Every block is compiled at build time, because a block that is really Rust
should be able to prove it: a typo on a slide is otherwise found in the
lecture. Three specifiers say what a block is:

| fence | at build time | on the slide |
| --- | --- | --- |
| ```` ```origins ```` | compiled; a failure fails the build | — |
| ```` ```origins,run ```` | compiled | a Run button |
| ```` ```origins,shouldFail ```` | compiled; a *success* fails the build | the `does_not_compile` crab |
| ```` ```origins,notation ```` | not compiled | — |

`shouldFail` is for a block whose compile error is the point of the slide.
`notation` is for one that is not a program at all -- a bare signature, or a
body elided to `{ ... }` -- and cannot be combined with the other two.

The program a block is compiled as is not the text on the slide. The markers
are translated back into the Rust they annotate, which is possible precisely
because the sigils say which lifetimes are notation:

| in the block | in the program |
| --- | --- |
| `[[a:…:]]`, `[[?:…:]]`, `[[*:…:]]` | erased |
| `'!a` | `'_` -- a concrete origin is the lifetime inference would pick, and is not nameable where the notation writes it |
| `'?a` | `'a` |
| `'a` | `'a` |

A block with a `main` is compiled as a program, one without as a set of items,
so a lone `struct` and `impl` check without complaint. `unused` warnings are
allowed: a slide shows what makes its point and nothing else.

`run` gives the block a button that compiles and runs it, the same way the
Aquascope editor's does -- posting to the Rust playground, or to
`aquascope-reveal --serve` itself when the deck is being served, so a lecture
needs no network. The output box, its close and expand buttons and the
full-size modal are the editor's, so the two kinds of block behave alike.

### Hidden lines

A line whose first non-blank characters are `# ` is compiled but not shown.
This is mdBook's convention, and it is what carries the context a snippet
needs without putting it on the page: a `use`, the `fn main` around a
fragment, a helper the slide is not about. `##` at the start of a line is an
escaped `#`, for a block that means to show one.

`````markdown
```origins,run
# fn main() {
let mut v: Vec<i32> = [[a:vec![1, 2, 3]:]];
let r: &'!a Vec<i32> = &v;
# }
```
`````

Hidden lines are what make `run` useful at all: a Run button on a fragment
could only ever print a compile error.

### Styling

A block is rendered as

```html
<div class="origins-block"><pre class="code hljs">…</pre></div>
```

and the wrapper is deliberate: it is what `.aquascope` is to an editor. It
carries the dashed frame, it is the positioned element the crab and the Run
button hang off, and the run output is appended to it -- so the output lands
inside the border, as the editor's does. None of that can live on the `pre`,
because `pre.code` carries `.hljs`, whose `overflow-x: auto` makes it a scroll
box: it clips absolutely-positioned children, swallows clicks on the part of a
button that hangs past its edge, and gives the crab the code's font size
instead of the slide's, so `4.5em` would measure smaller than in an editor.

The stylesheet this crate ships renders all of that on its own -- the box
notation, the frame, the button and its hover states, and the run output --
once, for both kinds of block, so the two read as one thing on a slide. A
`shouldFail` block emits the same `.ferris-container` markup `aquascope-embed`
does. mdBook's `ferris.js` is not involved and must not be: it inserts its own
container as a *sibling* of the block, which is what puts the crab outside it.

A deck's own stylesheet is linked after this one, so it can override anything.
The colours are meant to be overridden: redefine `--o` and `--o-pale` on a
`.origin-<letter>` class.

A rendered block carries `oframe origin-<letter>`
for a box around code, `oname origin-<letter>` for a boxed lifetime, the same
two without the `origin-` class for the neutral forms, and `oexact` for the
dashed box; everything else is highlight.css's own `hljs-*` classes, so a
rendered block is indistinguishable from the Aquascope-highlighted ones on
other slides. A boxed lifetime carries no `hljs-symbol` of its own, because
`.oname` owns its colour and weight.

Only `a`, `b` and `c` have colours here. `'!x` for any other letter draws
neutral -- indistinguishable from `'?x` while claiming something different --
until a stylesheet gives `.origin-x` an `--o` of its own.

Two things follow from how this is rendered, and are easy to trip over when
hand-writing the equivalent HTML instead:

- The output is a `pre` with **no `code` child**. reveal's highlight plugin
  rewrites the innerHTML of every `pre code` it finds, which would strip the
  boxes; a `pre` on its own is never selected.
- A blank line in the block is emitted as a line holding one space, because a
  truly blank line would end the raw-HTML block the markdown renderer sees and
  wrap the rest of the `pre` in a `<p>`.

Rust is lexed with `ra-ap-rustc_lexer`, rustc's own lexer, so comments,
strings, char literals, raw strings and lifetimes are delimited exactly as
rustc delimits them -- `'x'` is a char literal and not a lifetime named `x`.
Which identifiers are keywords, types or call sites stays a heuristic, the same
one highlight.js applies.

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
