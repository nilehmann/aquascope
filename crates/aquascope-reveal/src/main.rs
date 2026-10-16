//! Builds a reveal.js deck from a markdown file, running Aquascope over every
//! ```aquascope fenced block at build time and baking the results into the
//! page, and rendering every ```origins block into an annotated `<pre>`. See
//! `crates/aquascope-reveal/README.md`.

use std::{
  collections::{hash_map::DefaultHasher, BTreeSet},
  fs,
  hash::{Hash, Hasher},
  path::{Path, PathBuf},
  thread,
  time::{Duration, SystemTime},
};

use anyhow::{Context, Result};
use clap::{ArgGroup, Parser, ValueEnum};
use mdbook_aquascope::AquascopePreprocessor;
use site::{Include, Page, Site};

mod attributes;
mod deck;
mod deps;
mod frontmatter;
mod lint;
mod origins;
mod overlay;
mod run;
mod serve;
mod site;
mod stylesheet;
mod types;

/// Assets written into the deck's `aquascope/`. The first two come from the
/// frontend build via build.rs; the last two are the reveal.js glue.
const EMBED_JS: &[u8] = include_bytes!("../js/embed.iife.js");
const EMBED_CSS: &[u8] = include_bytes!("../js/style.css");
const GLUE_JS: &[u8] = include_bytes!("../assets/aquascope-reveal.js");
const GLUE_CSS: &[u8] = include_bytes!("../assets/aquascope-reveal.css");
/// mdBook's highlight.js theme. CodeMirror emits `hljs-*` spans that Aquascope
/// does not colour itself, relying on the book to supply a theme.
const HIGHLIGHT_CSS: &[u8] = include_bytes!("../assets/highlight.css");
const HIGHLIGHT_JS: &[u8] = include_bytes!("../assets/reveal-highlight.js");

/// The crab a `shouldFail` block shows, from the Rust book
/// (`rust-lang/book`, `src/img/ferris/`, MIT/Apache-2.0), served where
/// Aquascope's markup looks for it.
const CRAB: &[u8] = include_bytes!("../assets/ferris/does_not_compile.svg");

// reveal.js, highlight.js and Font Awesome, compiled in by build.rs from the
// frontend's node_modules.
include!(concat!(env!("OUT_DIR"), "/vendor.rs"));

/// Where in [`VENDOR`] reveal.js is, and where in the deck it goes.
const REVEAL_SRC: &str = "reveal.js/";
const REVEAL_DST: &str = "reveal/";
/// The same for highlight.js.
const HLJS_SRC: &str = "highlight.js/";
const HLJS_DST: &str = "highlight/";
/// Font Awesome 4.7, for decks that draw its icons in their own stylesheets.
const FONT_AWESOME: &str = "font-awesome/";

#[derive(Parser)]
#[clap(author, about, version)]
#[clap(group(ArgGroup::new("mode").required(true).args(["serve", "dist"])))]
struct Args {
  /// Markdown file holding the deck.
  input: PathBuf,

  /// Build the deck to hand out: `dir` for a directory of files, `single` for
  /// one self-contained .html file. A failed build writes nothing.
  #[clap(long, value_enum, value_name = "MODE", conflicts_with_all = ["serve", "watch"])]
  dist: Option<DistMode>,

  /// Where `--dist` writes: a directory for `dir` (default `dist`), a file
  /// for `single` (default `<input stem>.html`).
  // `conflicts_with` rather than `requires = "dist"`, which this clap does
  // not enforce for a member of a required group. One of the two is always
  // given, so it says the same.
  #[clap(short, long, conflicts_with = "serve")]
  out: Option<PathBuf>,

  /// Page title. Overrides the front matter; defaults to the input file stem.
  #[clap(long)]
  title: Option<String>,

  /// Name of a reveal.js theme, e.g. white, black, league. Overrides the front
  /// matter; defaults to white.
  #[clap(long)]
  theme: Option<String>,

  /// Extra stylesheet to copy in and link after Aquascope's own.
  #[clap(long = "css")]
  extra_css: Vec<PathBuf>,

  /// Extra script to copy in and load after the deck is initialized.
  #[clap(long = "js")]
  extra_js: Vec<PathBuf>,

  /// Directory included in the deck as `<name>/`, for images, fonts and
  /// anything else the slides refer to by path. Repeatable.
  #[clap(long = "static")]
  static_dirs: Vec<PathBuf>,

  /// Build the deck into a cache directory and serve it on localhost, with
  /// Run compiling against the deck's dependencies. Takes an optional port.
  #[clap(
    long,
    value_name = "PORT",
    num_args = 0 ..= 1,
    default_missing_value = "4321"
  )]
  serve: Option<u16>,

  /// With --serve, rebuild whenever the input, the extra assets or the
  /// static directories change, and reload the open page after each rebuild.
  #[clap(long, requires = "serve")]
  watch: bool,

  /// How often to poll for changes while watching.
  #[clap(long, default_value = "400", value_name = "MS")]
  poll_ms: u64,
}

#[derive(Clone, Copy, ValueEnum)]
enum DistMode {
  Dir,
  Single,
}

/// What a build is for, which decides where it goes and what the page
/// carries besides the slides.
enum Target {
  /// `--serve`: a directory in the cache, the page posting runs to the
  /// server and, with `--watch`, reloading itself.
  Dev {
    dir: PathBuf,
    port: u16,
    watch: bool,
  },
  DistDir(PathBuf),
  DistSingle(PathBuf),
}

impl Target {
  fn of(args: &Args) -> Result<Self> {
    let stem = || {
      args
        .input
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "slides".to_string())
    };
    Ok(match (args.dist, args.serve) {
      (Some(DistMode::Dir), _) => Target::DistDir(
        args.out.clone().unwrap_or_else(|| PathBuf::from("dist")),
      ),
      (Some(DistMode::Single), _) => Target::DistSingle(
        args
          .out
          .clone()
          .unwrap_or_else(|| PathBuf::from(format!("{}.html", stem()))),
      ),
      (None, Some(port)) => Target::Dev {
        dir: dev_dir(&args.input)?,
        port,
        watch: args.watch,
      },
      // clap requires one of the two.
      (None, None) => unreachable!(),
    })
  }

  fn is_dist(&self) -> bool {
    !matches!(self, Target::Dev { .. })
  }
}

/// Where `--serve` builds a deck: one directory per deck in the cache,
/// beside the external crates', named by a hash of the deck's path so two
/// decks served at once keep apart.
fn dev_dir(input: &Path) -> Result<PathBuf> {
  let path = fs::canonicalize(input)
    .with_context(|| format!("resolving {}", input.display()))?;
  let mut hasher = DefaultHasher::new();
  path.hash(&mut hasher);
  Ok(
    deps::cache_root()
      .join("aquascope-reveal")
      .join("serve")
      .join(format!("{:016x}", hasher.finish())),
  )
}

/// Applies the preprocessor's byte-range replacements back-to-front, so that
/// each splice leaves the ranges of the not-yet-applied ones valid.
fn apply(
  content: &str,
  mut edits: Vec<(std::ops::Range<usize>, String)>,
) -> String {
  edits.sort_by_key(|(range, _)| std::cmp::Reverse(range.start));
  let mut out = content.to_string();
  for (range, html) in edits {
    out.replace_range(range, &html);
  }
  out
}

fn escape(s: &str) -> String {
  s.replace('&', "&amp;")
    .replace('<', "&lt;")
    .replace('>', "&gt;")
}

/// The name a linked file goes by in the deck's `assets/`.
fn file_name(path: &Path) -> Result<String> {
  Ok(
    path
      .file_name()
      .with_context(|| format!("{} has no file name", path.display()))?
      .to_string_lossy()
      .into_owned(),
  )
}

/// Builds the deck, in memory. Nothing is written: that is the target's
/// business, and a build that fails has nothing to write.
fn build(
  args: &Args,
  target: &Target,
  preprocessor: &mut AquascopePreprocessor,
  shared_deps: &deps::Shared,
) -> Result<Site> {
  let source = fs::read_to_string(&args.input)
    .with_context(|| format!("reading {}", args.input.display()))?;

  // Checked against the file as written, so the line numbers are the ones the
  // author sees. A bad specifier is ignored rather than rejected by the
  // preprocessor, so warning is all we can do.
  for warning in lint::check(&source) {
    eprintln!("warning: {}:{warning}", args.input.display());
  }

  // Front matter is stripped first so the preprocessor's byte ranges are
  // computed against the same slice they are applied to.
  let (front, body) = frontmatter::split(&source)
    .with_context(|| format!("in {}", args.input.display()))?;

  // Before any block is compiled, since a block may use them. A relative
  // `path` dependency is relative to the deck, wherever the build runs from.
  let deck_dir = fs::canonicalize(&args.input)
    .with_context(|| format!("resolving {}", args.input.display()))?
    .parent()
    .map(Path::to_path_buf)
    .unwrap_or_default();
  let deps = deps::build(&front.dependencies, &deck_dir)
    .with_context(|| format!("in {}", args.input.display()))?;
  if !deps.is_empty() && target.is_dist() {
    eprintln!(
      "warning: {}: a --dist deck's Run posts to the Rust playground, which \
       has only its own selection of crates, not the deck's dependencies",
      args.input.display()
    );
  }
  *shared_deps.write().unwrap() = deps.clone();
  preprocessor.set_dependencies(deps.aquascope());

  // Both passes return byte ranges into `body` and never overlap: one
  // claims ```aquascope fences, the other ```origins fences.
  // `body` is a suffix of `source`, so the front matter's line count is what
  // an origins diagnostic has to add to name the line the author sees.
  let first_body_line =
    source[.. source.len() - body.len()].lines().count() + 1;

  // A broken block does not stop the build at once: every one is collected,
  // so the author sees all of them together, in the log and in the overlay.
  let file = args.input.display().to_string();
  let (mut edits, failures) = preprocessor.replacements_with_failures(body)?;
  let mut problems: Vec<_> = failures
    .into_iter()
    .map(|f| {
      let line = first_body_line + body[.. f.range.start].matches('\n').count();
      aquascope_problem(f, &format!("{file}:{line}"), &deps)
    })
    .collect();

  let notes = front.notes.unwrap_or_default();
  match origins::replacements(body, first_body_line, notes, &*preprocessor) {
    Ok((origin_edits, programs)) => {
      edits.extend(origin_edits);
      // Every ```origins block that claims to be code is compiled, so a typo
      // on a slide is found here rather than in the lecture. A block whose
      // failure is the point says `shouldFail`; one that is not a program
      // says `notation`.
      problems.extend(origins::check(&programs, &file, &deps));
    }
    Err(e) => {
      problems.push(overlay::Problem::general(&e.context(format!("in {file}"))))
    }
  }

  // The blocks that did render are worth keeping either way: the next build
  // then only waits on the ones being fixed.
  preprocessor.save_cache();

  if !problems.is_empty() {
    for problem in &problems {
      problem.print();
    }
    return Err(overlay::Failed(problems).into());
  }
  let content = apply(body, edits);

  let slides = deck::Deck::parse(&content).to_html();

  // Explicit flags win over the front matter, which wins over the defaults.
  let reveal_options = front.reveal_options_json();
  let title = args.title.clone().or(front.title).unwrap_or_else(|| {
    args
      .input
      .file_stem()
      .map(|s| s.to_string_lossy().into_owned())
      .unwrap_or_else(|| "Slides".to_string())
  });
  let theme = args
    .theme
    .clone()
    .or(front.theme)
    .unwrap_or_else(|| "white".to_string());

  let mut head = vec![
    css(&format!("{REVEAL_DST}dist/reset.css")),
    css(&format!("{REVEAL_DST}dist/reveal.css")),
    Include::Css {
      path: format!("{REVEAL_DST}dist/theme/{theme}.css"),
      id: Some("theme"),
    },
    css("aquascope/aquascope-reveal.css"),
    css("aquascope/highlight.css"),
    css("aquascope/style.css"),
    css(&format!("{FONT_AWESOME}css/font-awesome.min.css")),
  ];
  for path in &args.extra_css {
    head.push(css(&format!("assets/{}", file_name(path)?)));
  }

  // Only the grammars the deck's fences name; none, and no highlighting at
  // all, for a deck whose code is all ```aquascope and ```origins blocks.
  let grammars = grammars(&slides, &args.input);
  let mut tail = vec![
    Include::Js(format!("{REVEAL_DST}dist/reveal.js")),
    Include::Js(format!("{REVEAL_DST}plugin/notes/notes.js")),
    Include::InlineJs(format!(
      "window.AQUASCOPE_REVEAL_OPTIONS = {reveal_options};"
    )),
  ];
  if !grammars.is_empty() {
    tail.push(Include::Js(format!("{HLJS_DST}core.min.js")));
    for grammar in &grammars {
      tail.push(Include::Js(format!("{HLJS_DST}{grammar}")));
    }
    tail.push(Include::Js("aquascope/reveal-highlight.js".to_string()));
  }
  // The Run button posts to the Rust playground unless told otherwise. A
  // served deck answers it itself, from the same origin, against the deck's
  // dependencies and with no network.
  if let Target::Dev { .. } = target {
    tail.push(Include::InlineJs(format!(
      "window.AQUASCOPE_RUN_URL = \"{}\"; window.AQUASCOPE_RUN_STREAM_URL = \"{}\";",
      serve::RUN_ENDPOINT,
      serve::STREAM_ENDPOINT
    )));
  }
  tail.push(Include::Embed("aquascope/embed.iife.js".to_string()));
  tail.push(Include::Js("aquascope/aquascope-reveal.js".to_string()));
  for path in &args.extra_js {
    tail.push(Include::Js(format!("assets/{}", file_name(path)?)));
  }
  // livereload.js and the stamp it polls are written by `overlay::publish`,
  // after this build's outcome is known.
  if let Target::Dev { watch: true, .. } = target {
    tail.push(Include::Js("aquascope/livereload.js".to_string()));
  }

  let mut site = Site::new(Page {
    title: escape(&title),
    head,
    body: format!(
      "  <div class=\"reveal\">\n    <div class=\"slides\">\n{slides}    </div>\n  </div>\n"
    ),
    tail,
  });

  site.add("aquascope/embed.iife.js", EMBED_JS);
  site.add("aquascope/style.css", EMBED_CSS);
  site.add("aquascope/aquascope-reveal.js", GLUE_JS);
  site.add("aquascope/aquascope-reveal.css", GLUE_CSS);
  site.add("aquascope/highlight.css", HIGHLIGHT_CSS);
  if !grammars.is_empty() {
    site.add("aquascope/reveal-highlight.js", HIGHLIGHT_JS);
    site.add(
      format!("{HLJS_DST}core.min.js"),
      vendor(&format!("{HLJS_SRC}core.min.js")),
    );
    for grammar in &grammars {
      site.add(
        format!("{HLJS_DST}{grammar}"),
        vendor(&format!("{HLJS_SRC}{grammar}")),
      );
    }
  }

  // reveal.js with the one theme the deck uses, and every font a theme may
  // import; a single file inlines only those its theme does.
  let themes: Vec<&str> = VENDOR
    .iter()
    .filter_map(|(path, _)| {
      path
        .strip_prefix(REVEAL_SRC)?
        .strip_prefix("dist/theme/")?
        .strip_suffix(".css")
        .filter(|name| !name.contains('/'))
    })
    .collect();
  anyhow::ensure!(
    themes.contains(&theme.as_str()),
    "no reveal.js theme `{theme}`; the themes are {}",
    themes.join(", ")
  );
  for (path, bytes) in VENDOR {
    let Some(rel) = path.strip_prefix(REVEAL_SRC) else {
      continue;
    };
    let other_theme = rel.strip_prefix("dist/theme/").is_some_and(|t| {
      !t.contains('/') && t.ends_with(".css") && t != format!("{theme}.css")
    });
    if !other_theme {
      site.add(format!("{REVEAL_DST}{rel}"), *bytes);
    }
  }
  for (path, bytes) in VENDOR {
    if path.starts_with(FONT_AWESOME) {
      site.add(*path, *bytes);
    }
  }
  site.add("img/ferris/does_not_compile.svg", CRAB);

  for path in &args.extra_css {
    site.add(format!("assets/{}", file_name(path)?), read(path)?);
  }
  for path in &args.extra_js {
    site.add(format!("assets/{}", file_name(path)?), read(path)?);
  }
  // Last, so a deck's own file wins over one of ours at the same path -- its
  // own `img/ferris/` crab, say.
  for dir in &args.static_dirs {
    let name = file_name(dir)?;
    site.add_tree(&name, dir)?;
  }

  Ok(site)
}

/// The highlight.js grammars, as paths under [`HLJS_SRC`], for the languages
/// the fences in `slides` name, by name or alias. One with no grammar is
/// warned about and left plain.
fn grammars(slides: &str, input: &Path) -> Vec<String> {
  let named: BTreeSet<&str> = slides
    .split("<code class=\"language-")
    .skip(1)
    .filter_map(|rest| rest.split(['"', ' ']).next())
    .collect();
  // Every name a grammar answers to, with its path: its file's, and the
  // aliases its minified source lists as `aliases:["cc","c++",...]`.
  let mut languages: Vec<(&str, &str)> = Vec::new();
  for (path, bytes) in VENDOR {
    let Some(rel) = path.strip_prefix(HLJS_SRC) else {
      continue;
    };
    let Some(name) = rel
      .strip_prefix("languages/")
      .and_then(|name| name.strip_suffix(".min.js"))
    else {
      continue;
    };
    languages.push((name, rel));
    let text = std::str::from_utf8(bytes).unwrap_or_default();
    for list in text.split("aliases:[").skip(1) {
      let list = &list[.. list.find(']').unwrap_or(0)];
      for alias in list.split(',') {
        languages.push((alias.trim_matches('"'), rel));
      }
    }
  }
  let mut grammars = BTreeSet::new();
  for language in named {
    match languages
      .iter()
      .find(|(alias, _)| alias.eq_ignore_ascii_case(language))
    {
      Some((_, rel)) => {
        grammars.insert(rel.to_string());
      }
      None => eprintln!(
        "warning: {}: no highlight.js grammar for ```{language}, so its blocks \
         are left plain",
        input.display()
      ),
    }
  }
  grammars.into_iter().collect()
}

fn css(path: &str) -> Include {
  Include::Css {
    path: path.to_string(),
    id: None,
  }
}

fn read(path: &Path) -> Result<Vec<u8>> {
  fs::read(path).with_context(|| format!("reading {}", path.display()))
}

fn vendor(path: &str) -> &'static [u8] {
  VENDOR
    .iter()
    .find(|(p, _)| *p == path)
    .map(|(_, bytes)| *bytes)
    .unwrap_or_else(|| panic!("{path} is not vendored"))
}

/// Explains an ```aquascope block Aquascope could not render.
///
/// Aquascope's own report says only that the build failed, with no location
/// and no compiler message, so the block is put through rustc to recover one.
/// Its raw output is kept as the details for when that finds nothing -- a
/// timeout, or a crash in Aquascope itself.
fn aquascope_problem(
  failure: mdbook_aquascope::BlockFailure,
  location: &str,
  deps: &deps::Deps,
) -> overlay::Problem {
  let (message, diagnostic) =
    match (run::check(&failure.code, deps), failure.should_fail) {
      (Err(e), false) => ("the ```aquascope block does not compile", Some(e)),
      (Ok(()), true) => (
        "Aquascope failed on this block, which is marked shouldFail but \
         compiles",
        None,
      ),
      _ => ("Aquascope failed on this block", None),
    };
  overlay::Problem {
    location: Some(location.to_string()),
    message: message.to_string(),
    diagnostic,
    details: Some(format!("{:#}", failure.error)),
  }
}

/// Writes a development build, or prints a failed one and hands its
/// problems to the open page.
fn report(dir: &Path, result: Result<Site>) -> Result<()> {
  let general;
  let problems = match &result {
    Ok(site) => {
      for broken in site.broken_refs() {
        eprintln!("warning: {broken}");
      }
      site::write_dir(site, dir)?;
      Vec::new()
    }
    Err(e) => {
      eprintln!("Build failed: {e:#}");
      let problems = overlay::problems_of(e);
      if problems.is_empty() {
        general = overlay::Problem::general(e);
        vec![&general]
      } else {
        problems
      }
    }
  };
  overlay::publish(dir, &problems)
}

/// Every file a rebuild depends on: the deck itself, the linked assets, and
/// the contents of any `--static` directory.
fn watched(args: &Args) -> Vec<PathBuf> {
  fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
      return;
    };
    for entry in entries.flatten() {
      let path = entry.path();
      if path.is_dir() {
        walk(&path, out);
      } else {
        out.push(path);
      }
    }
  }

  let mut files = vec![args.input.clone()];
  files.extend(args.extra_css.iter().cloned());
  files.extend(args.extra_js.iter().cloned());
  for dir in &args.static_dirs {
    walk(dir, &mut files);
  }
  files.sort();
  files
}

/// Modification times of everything watched. Collecting the file list afresh
/// each time means added and removed files register as changes too.
fn fingerprint(args: &Args) -> Vec<(PathBuf, Option<SystemTime>)> {
  watched(args)
    .into_iter()
    .map(|path| {
      let mtime = fs::metadata(&path).and_then(|m| m.modified()).ok();
      (path, mtime)
    })
    .collect()
}

fn main() -> Result<()> {
  let args = Args::parse();
  let target = Target::of(&args)?;
  let mut preprocessor = AquascopePreprocessor::new()?;
  let deps = deps::Shared::default();

  let (dir, port, watch) = match &target {
    Target::DistDir(dir) => {
      let site = build(&args, &target, &mut preprocessor, &deps)?;
      site::dist_dir(&site, dir)?;
      println!("Wrote {}", dir.display());
      return Ok(());
    }
    Target::DistSingle(file) => {
      let site = build(&args, &target, &mut preprocessor, &deps)?;
      let size = site::dist_single(&site, file)?;
      println!(
        "Wrote {} ({:.1} MB)",
        file.display(),
        size as f64 / (1 << 20) as f64
      );
      return Ok(());
    }
    Target::Dev { dir, port, watch } => (dir.clone(), *port, *watch),
  };

  // Afresh each run, so nothing an earlier one wrote -- a dropped --static
  // directory, a stale error page -- is served.
  let _ = fs::remove_dir_all(&dir);
  fs::create_dir_all(&dir)
    .with_context(|| format!("creating {}", dir.display()))?;

  // Without --watch a failed build is fatal. With it, even the first failure
  // only prints and goes to the overlay, and the watcher keeps running.
  let result = build(&args, &target, &mut preprocessor, &deps);
  if watch {
    report(&dir, result)?;
  } else {
    let site = result?;
    for broken in site.broken_refs() {
      eprintln!("warning: {broken}");
    }
    site::write_dir(&site, &dir)?;
  }

  serve::spawn(dir.clone(), port, deps.clone())?;

  if !watch {
    thread::park();
    return Ok(());
  }

  println!("Watching {} files. Ctrl-C to stop.", watched(&args).len());
  let mut last = fingerprint(&args);
  loop {
    thread::sleep(Duration::from_millis(args.poll_ms));
    let current = fingerprint(&args);
    if current == last {
      continue;
    }
    last = current;

    let result = build(&args, &target, &mut preprocessor, &deps);
    if let Err(e) = report(&dir, result) {
      eprintln!("Could not report the build: {e:#}");
    }
  }
}
