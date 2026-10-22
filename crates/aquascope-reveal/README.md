# aquascope-reveal

Builds a [reveal.js](https://revealjs.com) deck from a markdown file, running
Aquascope over every ` ```aquascope ` block at build time and baking the results
into the page. The output is static: presenting it needs no Aquascope server.

Every run says what it is for. `--serve` is for writing and presenting the
deck; `--dist` builds it to hand out, as a directory or as one `.html` file:

```
aquascope-reveal lecture.md --static img --serve --watch
aquascope-reveal lecture.md --static img --dist dir      # dist/
aquascope-reveal lecture.md --static img --dist single   # lecture.html
```

A run with neither is an error, and the two cannot be combined. See
[Serving](#serving) and [Distributing](#distributing).

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
notes: float
revealOptions:
  navigationMode: default
  slideNumber: "c/t"
---

# First slide
```

The block is recognised only when the file *starts* with `---`; anywhere else
that line is a slide separator. `--title` and `--theme` override it.
`notes` is where ```` ```origins ```` blocks show their
[callouts](#callouts): `strip` (the default) or `float`. `dependencies` names
the [external crates](#external-crates) the blocks may use, and `modules`
holds [code every block can import](#deck-modules).

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
| `[[^:TEXT:]]` | no box: `TEXT` shows its type on hover (see [Types on hover](#types-on-hover)) |
| `[[+N:TEXT:]]` | no box: `TEXT` appears on click `N` (see [Stepping through code](#stepping-through-code)) |
| `[[=:TEXT:]]`, `[[=N:TEXT:]]` | `TEXT` in focus and the rest faded, from the start or on click `N` (see [Highlighting](#highlighting)) |

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

### Types on hover

`[[^:EXPR:]]` draws no box. It makes `EXPR` show its type while the pointer
is over it, for a slide that walks through what each part of an expression
is:

`````markdown
```origins
# use std::ops::Deref;
# struct MyBox<T>(T);
# impl<T> MyBox<T> { fn new(x: T) -> MyBox<T> { MyBox(x) } }
# impl<T> Deref for MyBox<T> { type Target = T; fn deref(&self) -> &T { &self.0 } }
fn main() {
    let [[^:b:]] = MyBox::new(5);
    let x = [[^:*([[^:[[^:b:]].deref():]]):]] + 1;
}
```
`````

The type is not written in the block. It is asked of the compiler at build
time, so it is the type rustc gave that expression and cannot drift from the
code. It is the type as written, before any `&` or `*` rustc inserts: `b` in
`b.deref()` is a `MyBox<i32>`, not the `&MyBox<i32>` it is borrowed as for
the call. Paths are trimmed to their last segment, as in a diagnostic.

Markers nest, and only the innermost one under the pointer answers, so
hovering `b` above names `b` rather than the call around it. They combine
with origin boxes, hidden lines and `run`.

A marker on a function or method *name* shows its signature instead, twice
when it is generic: as declared, bounds and all, and as instantiated at this
call.

```rust
let (tx, rx) = mpsc::[[^:channel:]]::<i32>();
tx.[[^:send:]](1).unwrap();
```

shows, on `channel` and on `send`,

```
fn channel<T>() -> (Sender<T>, Receiver<T>)
fn channel() -> (Sender<i32>, Receiver<i32>)

fn send(&self, t: T) -> Result<(), SendError<T>>
fn send(&self, t: i32) -> Result<(), SendError<i32>>
```

The declaration is the function's own source text, so a `std` function reads
as it does in the standard library -- `where` clause on a line of its own --
which takes the toolchain's `rust-src` component; without it the generic
signature is rebuilt from the types. The marker may cover the name alone or
the path to it, `[[^:mpsc::channel:]]`, and a function with nothing generic
about it shows one line. A tooltip that would run off the window is moved
back onto it, and one near the bottom opens above its expression.

A marker has to cover exactly one expression, the name a `let` or a
parameter binds -- `let mut [[^:b:]]` -- or a function or method name, and
one that does not fails the build: `[[^:b.deref:]]()` is none of them. An expression inside
parentheses can be marked with or without them. A marker in a `notation`
block is an error too, since nothing is compiled to ask, and so is one on a
hidden line, which would show nothing. In a `shouldFail` block the types are
there as long as the error comes after type checking -- a borrow-check error,
which is what such a block usually shows.

The types come from `aquascope-driver types`, run the same way as the
```` ```aquascope ```` blocks, so a block with a type marker needs the same
nightly toolchain and shares their cache (`.aquascope-cache`). A block without
one is still checked with plain `rustc`.

The rendered expression is a `<span class="ty" data-type="…">`. Its colours
are `--ty-hover`, `--ty-tip-bg` and `--ty-tip-fg`. The tooltip is a single
`div.ty-tip` fixed over the page rather than part of the code, so it shows
above everything -- highlights, floating notes, the run output -- and no
container clipping its contents can cut it off.

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
| `[[a:…:]]`, `[[?:…:]]`, `[[*:…:]]`, `[[^:…:]]`, `[[+N:…:]]`, `[[=…:]]` | erased |
| `'!a` | `'_` -- a concrete origin is the lifetime inference would pick, and is not nameable where the notation writes it |
| `'?a` | `'a` |
| `'a` | `'a` |

A block with a `main` is compiled as a program, one without as a set of items,
so a lone `struct` and `impl` check without complaint. `unused` warnings are
allowed: a slide shows what makes its point and nothing else.

`run` gives the block a button that compiles and runs it, the same way the
Aquascope editor's does -- posting to `aquascope-reveal --serve` itself when
the deck is being served, so a lecture needs no network, and to the Rust
playground from a `--dist` deck. The output box, its close and expand buttons and the
full-size modal are the editor's, so the two kinds of block behave alike.

When the deck is served, the output is also *streamed*: each line appears as
the program prints it, stdout and stderr interleaved as they were written, so
two threads taking turns are seen taking turns. Compiler diagnostics arrive
first, in one piece. Pressing Run again, or closing the output, stops the
program that is still running. A `--dist` deck posts to the
playground, which only answers once the program has finished, and the
editor's Run button in ```` ```aquascope ```` blocks does the same either
way.

### The editor

Every compiled block -- plain, `run` or `shouldFail`, anything but
`notation` -- has an ✎ button beside its Run button. It opens the block's
program in an editor over the slide, to be changed and run in front of the
room. The editor holds the program as it is compiled, not the text on the
slide: hidden lines are shown, markers are gone and `'!a` reads `'_`, so
what is in it is Rust that runs as it stands.

| control | does |
| --- | --- |
| **▶ Run** | compiles and runs the code in the editor, the way the block's Run button does, with the output under the code |
| **Reset** | puts back the program the block was built with |
| **vim** | toggles vim keybindings, remembered by the browser for the next deck |
| **✕**, a click outside, Escape | closes it; with vim on, Escape is vim's and `:q` closes |
| Ctrl/Cmd `+` `-` `0` | makes the code and output larger, smaller, and back, until the page reloads |

The slide never changes: its boxes and steps are what was written. The
edits are kept for the block until the page reloads, so closing the editor
by accident loses nothing, and the ✎ carries a dot while they differ from
the slide's program. `shouldFail` blocks run like any other, so the error can
be shown and then fixed live.

The program is on the block, in `data-program`. The editor is the embed
bundle's CodeMirror, so the first one opened loads the bundle if no
```` ```aquascope ```` block on an earlier slide has. `--aq-editor-font-size`
sets its size (default `32px`), and the dot is `--aq-editor-edited`.

### External crates

`dependencies` in the front matter lists crates the blocks may use, each
written as Cargo.toml would declare it, in YAML:

```markdown
---
dependencies:
  trpl: "0.2"
  tokio: { version: "1", features: [full] }
  my-helpers: { path: ../helpers }
---
```

They are built once, by `cargo build`, into a crate of their own under
`$XDG_CACHE_HOME/aquascope-reveal/deps/` (`~/.cache` when that is unset), and
both the build-time check and the Run button under `--serve` compile each
block with `--extern` for every dependency. A block then says `trpl::block_on`
or `use tokio::sync::mpsc;` as a program in a cargo project would, with no
`extern crate`. A relative `path` is relative to the deck.

cargo runs on every build, so it prints its progress the first time -- tokio
takes a while -- and is a no-op afterwards, needing no network once the
crates are downloaded. That crate's directory is named by a hash of the
dependencies and of `rustc -vV`, so changing either builds afresh rather than
loading rlibs the compiler cannot read; old directories are not cleaned up.

Aquascope sees them too: ```` ```aquascope ```` blocks and `[[^:…:]]` types
compile each block as a cargo project of its own on Aquascope's nightly, and
that project gets the same `[dependencies]` and Cargo.lock. The crates are
built for it a second time, with that nightly and against Miri's sysroot,
which is what the block is compiled against: an rlib built for any other
`std` does not load. They share one target directory, `target-aquascope/`
beside the other build, so that happens once and not once per block. Answers
are cached under the block and the dependencies, so a deck without any keeps
its cache as it was.

In a `--dist` deck, Run posts to the Rust playground, which has [its own
selection of crates][playground-crates] -- many of the popular ones, `trpl`
among them, but not necessarily a deck's. `--dist` warns when a deck has
dependencies.

[playground-crates]: https://github.com/rust-lang/rust-playground/blob/main/compiler/base/Cargo.toml

### Deck modules

`modules` in the front matter defines Rust modules once for the whole deck,
each a name and its code:

````markdown
---
modules:
  pin: |
    pub enum Example {
        Start,
        Waiting { v: Vec<i32>, r: *const Vec<i32> },
        Done,
    }
---
````

Every compiled block -- ```` ```aquascope ```` and ```` ```origins ````, but not
`notation` -- is compiled with each one after it, `mod pin { … }`, so a block
says `use pin::Example;` and shows nothing else of it. They go at the end so
the lines on the slide keep their numbers in a compile error. Items a block
imports must be `pub`, as in any module.

An ```` ```origins ```` block's [editor](#the-editor) leaves them out: it
shows the block's own program, and the page carries the modules once, in
`window.AQUASCOPE_MODULES`, for Run to append on the way to the compiler --
`--serve`'s or the playground's. An ```` ```aquascope ```` block gets them as
[hidden lines](#hidden-lines) instead, and its interpreter steps into the
module's functions like any others, so a block that calls one marks the
steps it means to show.


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

### Stepping through code

A block can reveal its code a click at a time. `#N` in front of a line shows
that line on click `N`:

`````markdown
```origins,run
fn main() {
    let v = vec![1, 2, 3];
    #1 let r = &v;
    #2 v.push(4);
    #3 println!("{r:?}");
}
```
`````

The marker goes after the indentation, and the one space after it is dropped,
which is the rule `# ` follows for a hidden line: the marker's characters never
count as indentation, so `#1 fn helper() {` is a top-level line. `#` followed
by a digit is never Rust at the start of a line (an attribute is `#[`), and
`##1` is the escaped `#1`, as `##` is everywhere else.

For several lines at once, or part of one, `[[+N:…:]]` is a frame like any
other and nests with them:

`````markdown
```origins
fn longest<[[+1:'?a:]]>(x: &[[+1:'?a :]]str, y: &[[+1:'?a :]]str) -> &[[+1:'?a :]]str {
    if x.len() > y.len() { x } else { y }
}

[[+2:
fn main() {
    #3 let s = longest("ab", "c");
}
:]]
```
`````

**A line holding nothing but opening markers, or nothing but `:]]`, is folded
into its neighbour** -- the opening ones onto the start of the next line's
code, the closing ones onto the end of the line before. That is what lets a
marker wrap whole lines without leaving a blank line where it sat, and it holds
for every frame, so `[[a:` and `:]]` on lines of their own frame a struct
with no blank line at either end of the box.

`N` is reveal's `data-fragment-index`, not a count within the block, so code
and prose line up: a `<div class="fragment" data-fragment-index="2">` beside
the block appears on the same click as its `#2` lines. Steps count from 1.

A step is a `<span class="fragment" data-fragment-index="N">` around the code,
which reveal hides with `visibility`, so a hidden step keeps its space and the
block's frame does not grow as the steps arrive. A step covering the same code
as a box is emitted outside it, so the box is hidden with its code.

Steps only change what is visible. The block is compiled and run as the whole
program, whichever steps are showing, and the intermediate states are not
checked -- when one has to be, write each as its own block and swap them with
fragments. A `shouldFail` block's crab is there from the start. Steps may be
used in a `notation` block.

The build fails on a step that could not show: `#0`, a step on a hidden line,
a step inside another with a lower number, and a `#N` line that opens a marker
it does not close.

### Highlighting

`[[=:…:]]` puts part of a block in focus: the rest of the block fades, and the
highlighted code keeps its full strength on a soft tint. A highlight over
several lines is one tinted shape behind all of them, drawn the way an
editor draws a selection: the lines share a left and a right edge, except
that a highlight starting partway through a line starts there and one ending
partway through a line ends there. Highlighting the closure in
`spawn(|| { … });` leaves `spawn(` and `);` outside it, and a highlight over
whole lines is a plain rectangle. Indentation never counts as code.
`[[=N:…:]]` does the same from click `N`, with the code on the slide from the
start -- the click lights it, it does not reveal it.

`````markdown
```origins,shouldFail
fn main() {
    let mut v = [[=1:vec![1, 2, 3]:]];
    let r = [[=2:&v:]];
    [[=3:
    v.push(4);
    println!("{r:?}");
    :]]
}
```
`````

Timed highlights are a walk-through: a `[[=N:…:]]` highlight is lit on step
`N` and on no other, all the ones sharing a step lit together. A click that
lights no highlight -- a `[[+M:…:]]` step revealing code, a fragment of prose
-- returns the block to full strength, and so does the click after the last
highlight, which the block adds itself if nothing else on the slide has it.
A `[[=:…:]]` highlight is lit whenever no timed one is -- before the
first, on a click that lights none, and after the last.

It is a frame like the others: it nests with them, a line holding only the
marker folds into its neighbour, and it is erased from the program that is
compiled. A box over exactly the same code is drawn inside it, so the box
is lifted out of the fade with its code. `N` is the same reveal step number
as `#N`, and a highlight inside a `[[+M:…:]]` step must be lit on step `M` or
later, since its code is not shown before then. `[[=0:` and a highlight on a
hidden line fail the build.

#### Callouts

A highlight can carry a sentence explaining it. The text is written after
the fence, not inside the marker, as one `[=N]: text` line per step:

`````markdown
```origins
fn main() {
    [[=2:thread::spawn:]]([[=3:|| {
        println!("hi from the spawned thread!");
    }:]]);
}
```
[=2]: `thread::spawn` starts a new OS thread and returns a `JoinHandle` right away.
[=3]: The closure is the thread's **body**. It runs concurrently with `main`.
`````

The note for step `N` is shown in a strip under the code while step `N`'s
highlights are lit, and `[=]: text` explains the `[[=:…:]]` ones lit from
the start. The text is inline markdown. The lines have the shape of
markdown's link reference definitions, so a plain markdown preview hides
them.

The notes start on the line after the closing fence, may be separated by
blank lines, and end at the first line that is neither. A note may be for a
step with no `[[=N:` highlight: it is then shown on that step on its own,
with nothing in the code singled out, and like the highlights it is gone
the click after the block's last step. `[=]` in a block with no `[[=:`
highlight, or a step defined twice, fails the build. A highlight without a
note is fine.

The strip never moves the slide. Every note is in it from the start,
stacked in one place, so it is as tall as its longest note before any of
them shows, and only which one is visible changes from step to step. Its
background, `--ohl-notes-bg`, and its rule fade in with the first note.

##### Floating callouts

The strip costs the block a row at its bottom. `notes: float` in the front
matter shows every block's notes as a card floating over the code instead,
and `notes=float` or `notes=strip` in a fence overrides the deck for that
block (```` ```origins,run,notes=float ````). The `[=N]:` lines are the same
either way.

A floating card takes no room and has no arrow: it sits beside what is lit,
its left edge in the highlight's colour. aquascope-reveal.js places it on
every step:

- right of the lit lines, past the end of the code on every line the card
  spans, and narrowed to fit there -- so it covers no code;
- if that leaves too little room for a readable card -- a zoomed-in browser,
  or a long line among those the card spans -- still right of the lit code,
  past only the lit code, covering the faded code to its right: a card that
  leaves its row reads as a note on the lines it lands on;
- if the lit code reaches too near the right edge for that, in the clear
  space nearest the lit lines, above them before below, as long as it is
  close enough to read as theirs;
- otherwise just over the lit lines, or just under them, covering faded
  ones. Only a block too short to hold the card has it hang under the
  block, outside the frame.

A note with nothing lit is placed the same way against the block's first
line, which puts it in the top-right corner. The card is re-placed when the
window resizes and when the block grows, as it does when a run's output
arrives. Its colours are `--ohl-notes-bg` for the background,
`--ohl-note-edge` for its left edge and `--ohl-note-border` for the rest.

Nothing moves as the focus does: the tint and the fade are both laid over
the code rather than being part of it. The tint is an `svg.ohl-box` that
aquascope-reveal.js shapes to the highlighted code each time it is lit and
when the window resizes. Their look is the deck's to change: `--ohl` is the
tint colour, `--ohl-fade` how strongly the rest fades (`1` hides it).

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
The crab itself, `img/ferris/does_not_compile.svg`, is built in; a
`--static img` directory holding its own copy replaces it.

Font Awesome 4.7 is built in too, and linked before a deck's own
stylesheets, so a deck can draw its icons -- `content: "\f071"; font-family:
FontAwesome` -- without shipping the font. Only for drawing in CSS: an `<i
class="fa fa-...">` in a slide is replaced by Aquascope's bundle, which
carries Font Awesome 6's JavaScript, with an icon from its own set.

A fenced block that is neither ```aquascope nor ```origins is highlighted by
highlight.js 11.9, if its fence names a language: ```cpp, ```c++ and every
other name or alias highlight.js knows. Only the grammars a deck's fences
name are included, and none at all, with no highlighter, when it names none.
A fence that names no language is left plain -- there is no guessing -- and
one naming a language highlight.js does not have is warned about and left
plain. This replaces reveal's highlight plugin, which carries all 190
grammars, 0.9 MB, and its line-number features (`data-line-numbers`) are not
supported.

A deck's own stylesheet is linked after this one, so it can override anything.
The colours are meant to be overridden: redefine `--o` and `--o-pale` on a
`.origin-<letter>` class.

A rendered block carries `oframe origin-<letter>`
for a box around code, `oname origin-<letter>` for a boxed lifetime, the same
two without the `origin-` class for the neutral forms, `oexact` for the
dashed box, and `ohl` for a highlight, with `on` while it is lit; everything else is highlight.css's own `hljs-*` classes, so a
rendered block is indistinguishable from the Aquascope-highlighted ones on
other slides. A boxed lifetime carries no `hljs-symbol` of its own, because
`.oname` owns its colour and weight.

Only `a`, `b` and `c` have colours here. `'!x` for any other letter draws
neutral -- indistinguishable from `'?x` while claiming something different --
until a stylesheet gives `.origin-x` an `--o` of its own.

Two things follow from how this is rendered, and are easy to trip over when
hand-writing the equivalent HTML instead:

- The output is a `pre` with **no `code` child**. The highlighter rewrites
  the innerHTML of every `pre code` naming a language, which would strip the
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
| `--serve [PORT]` | Build into a cache directory and serve the deck on `127.0.0.1:PORT`, default 4321. See [Serving](#serving). |
| `--watch` | With `--serve`: rebuild on change and reload the open page. See [Watching](#watching). |
| `--dist dir\|single` | Build the deck to hand out, as a directory or as one `.html` file. The mode is required. See [Distributing](#distributing). |
| `-o, --out` | With `--dist`: where to write. Defaults to `dist` for `dir`, `<input stem>.html` for `single`. |
| `--title` | Page title. Defaults to the input file stem. |
| `--theme` | reveal.js theme name. Defaults to `white`. The themes are reveal.js 5.1.0's, which is built in. |
| `--css` | Extra stylesheet, included as `assets/<name>` and linked after Aquascope's. Repeatable. |
| `--js` | Extra script, included as `assets/<name>` and loaded last. Repeatable. |
| `--static` | Directory included as `<name>/`, for images and fonts the slides refer to by path. Repeatable. |
| `--poll-ms` | Poll interval while watching. Defaults to 400. |

Exactly one of `--serve` and `--dist` is required. reveal.js itself, with its
themes, fonts and notes plugin, highlight.js and Font Awesome 4.7 are
compiled into the binary, so no deck needs a network to show.

## Serving

`--serve` builds the deck into a directory of its own in the cache --
`$XDG_CACHE_HOME/aquascope-reveal/serve/<hash of the deck's path>/`, beside
the [external crates](#external-crates) -- and runs a small built-in static
server on loopback over it, so presenting needs nothing else installed and
writes nothing into the project. The directory is emptied when the server
starts, so nothing from an earlier run is served.

A served deck's Run compiles on this machine, against the deck's
dependencies, with the output streamed. A slide referring to a file the deck
does not have is a warning here; `--dist` refuses to build it.

It is a preview server and nothing more: one thread per
connection, no keep-alive, no ranges, no compression, and every response is
`Cache-Control: no-store` so a rebuilt asset is never served from cache.
Requests that try to escape the output directory are rejected, including
percent-encoded forms of `..`.

## Distributing

`--dist` builds the deck to hand out. The mode is not optional:

- `--dist dir` writes a directory, `dist` unless `-o` says otherwise, to be
  hosted anywhere or opened as a file.
- `--dist single` writes one `.html` file, `<input stem>.html` unless `-o`
  says otherwise, with everything the deck needs inside it: reveal.js,
  Aquascope, the theme's fonts, the `--css` and `--js` files, and every
  file of a `--static` directory that the slides use. It opens from a
  double-click, an email attachment or a USB stick with no network.

Both behave like a compiler rather than a dev server:

- **A failed build writes nothing.** The problems go to stderr and the exit
  code is non-zero. The output is built beside its target and moved into
  place only once it is complete, so the previous one stays as it was.
- **Every relative reference has to resolve.** A slide's `src`, `href`,
  `poster`, `data-src`, `data-background-image`, `style` and `<style>`
  `url()`s, and the `url()`s and `@import`s of every stylesheet, must name a
  file the deck has -- in practice, one in a `--static` directory. A
  reference that does not fails the build, naming its slide, as does one
  starting with `/` or leaving the deck with `..`. Absolute URLs (`https:`,
  `data:`) are left alone.
- **`--dist dir` replaces its directory whole**, so nothing from an earlier
  build lingers. It writes a `.aquascope-reveal` marker into it and replaces
  only a directory holding one, or an empty one; any other fails the build,
  so a mistyped `-o` cannot delete anything.
- **Run posts to the Rust playground**, the deck having no server; see
  [External crates](#external-crates).

`--dist single` has two limits of its own. A link to a file (`<a
href="notes.pdf">`) fails the build, since browsers refuse to open a `data:`
URI from a link; use `--dist dir` for such a deck. And every inlined file
grows the one file by a third again, base64 being what it is, so the build
warns about each file over 5 MB and prints the size it wrote.

To keep it small, a single file carries only what a browser will load. Each
stylesheet is put through lightningcss: an `@font-face` listing several
formats keeps only the best every browser reads (woff2, then woff, then
ttf/otf), since the others would never be fetched -- for Font Awesome's own
CSS that is 77 KB of its 1.3 MB -- and an `@import` is replaced by the text
of the stylesheet it imports rather than inlined as a `data:` URI, which
would base64 its fonts twice. A typical lecture deck comes to about 2 MB,
most of it the Aquascope bundle.

## Watching

`--watch`, which only goes with `--serve`, rebuilds whenever the deck, any
`--css`/`--js` file, or anything inside a `--static` directory changes, and
injects a script that reloads the open page once the rebuild lands. The two
are the usual authoring loop:

```
aquascope-reveal lecture.md --static img --serve --watch
```

Reloading preserves the hash and reveal restores position from it, so you come
back to the slide *and* fragment you were on. Rebuilds are fast because the
Aquascope cache is keyed on each block's code and config -- only blocks you
actually edited are re-analyzed.

A failing build prints the error, leaves the watcher running, and shows the
error in a modal over the open page, the way a dev server's error overlay does.
The page underneath stays on the last successful build. Every broken
```` ```aquascope ```` and ```` ```origins ```` block is listed with its line
in the deck and rustc's diagnostic, not just the first one; for an
```` ```aquascope ```` block, Aquascope's raw output sits in a collapsed
section, for failures rustc does not explain. Esc or a click outside dismisses
the modal until the next build, and a successful build reloads the page as
usual. If the very first build fails, a placeholder page carries the modal
until there is a deck.

The reload script, `build-stamp.txt` and `build-error.html` are written only
under `--watch`, into the cache directory. `--serve` without `--watch` fails
on a broken first build, printing every problem; `--dist` always does.

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
  hydrate-everything listener never runs. A `--dist single` deck carries the
  bundle in an inert `<script type="text/x-aquascope-embed">` instead of a
  file, and the glue runs its text at that same point. `initAquascopeBlocks` is idempotent —
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
`frontend/packages/aquascope-embed/dist/`, so run `depot build --release` in
`frontend` before `cargo install --path crates/aquascope-reveal`, or `cargo
make install-reveal` for both. A plain `depot build` makes a development
bundle, unminified and with React's development checks, almost three times
the size; a release build of this crate refuses it, and a debug build warns.

`build.rs` also compiles in reveal.js 5.1.0, highlight.js 11.9.0 and Font
Awesome 4.7.0. None of them is checked in: they are devDependencies of
`aquascope-embed`, pinned exactly, so the same `depot build` installs them
into its `node_modules`, and `build.rs` takes what a deck loads from there,
checking each is the version it expects. It changes a few files on the way
-- font stylesheets cut to woff/woff2 sources, highlight.js's ES-module core
wrapped as a classic script -- as its `Vendor` doc comment describes. To
upgrade one, change its version in `aquascope-embed/package.json` and in
`build.rs`. The `shouldFail` crab, from the Rust book, is the one file
checked in, as `assets/ferris/does_not_compile.svg`.
