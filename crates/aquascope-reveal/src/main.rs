//! Builds a reveal.js deck from a markdown file, running Aquascope over every
//! ```aquascope fenced block at build time and baking the results into the
//! page. See `crates/aquascope-reveal/README.md`.

use std::{
  fs,
  path::{Path, PathBuf},
  thread,
  time::{Duration, SystemTime},
};

use anyhow::{Context, Result};
use clap::Parser;
use mdbook_aquascope::AquascopePreprocessor;

mod attributes;
mod deck;
mod frontmatter;
mod lint;
mod serve;

/// Assets copied verbatim into `<out>/aquascope/`. The first two come from the
/// frontend build via build.rs; the last two are the reveal.js glue.
const EMBED_JS: &[u8] = include_bytes!("../js/embed.iife.js");
const EMBED_CSS: &[u8] = include_bytes!("../js/style.css");
const GLUE_JS: &[u8] = include_bytes!("../assets/aquascope-reveal.js");
const GLUE_CSS: &[u8] = include_bytes!("../assets/aquascope-reveal.css");
/// mdBook's highlight.js theme. CodeMirror emits `hljs-*` spans that Aquascope
/// does not colour itself, relying on the book to supply a theme.
const HIGHLIGHT_CSS: &[u8] = include_bytes!("../assets/highlight.css");
/// Injected only by `--watch`.
const LIVERELOAD_JS: &[u8] = include_bytes!("../assets/livereload.js");

const CDN: &str = "https://cdn.jsdelivr.net/npm/reveal.js@5.1.0";

#[derive(Parser)]
#[clap(author, about, version)]
struct Args {
  /// Markdown file holding the deck.
  input: PathBuf,

  /// Directory to write the built deck into.
  #[clap(short, long, default_value = "dist")]
  out_dir: PathBuf,

  /// Page title. Overrides the front matter; defaults to the input file stem.
  #[clap(long)]
  title: Option<String>,

  /// Name of a reveal.js theme, e.g. white, black, league. Overrides the front
  /// matter; defaults to white.
  #[clap(long)]
  theme: Option<String>,

  /// Path to a local reveal.js package (the directory containing `dist` and
  /// `plugin`, e.g. node_modules/reveal.js). Without this the deck loads
  /// reveal.js from a CDN, which means it will not work offline.
  #[clap(long)]
  reveal_dir: Option<PathBuf>,

  /// Extra stylesheet to copy in and link after Aquascope's own.
  #[clap(long = "css")]
  extra_css: Vec<PathBuf>,

  /// Extra script to copy in and load after the deck is initialized.
  #[clap(long = "js")]
  extra_js: Vec<PathBuf>,

  /// Directory copied verbatim into the output as `<out-dir>/<name>`, for
  /// images, fonts and anything else the slides reference by path. Repeatable.
  #[clap(long = "static")]
  static_dirs: Vec<PathBuf>,

  /// Serve the built deck on localhost. Takes an optional port.
  #[clap(
    long,
    value_name = "PORT",
    num_args = 0 ..= 1,
    default_missing_value = "4321"
  )]
  serve: Option<u16>,

  /// Rebuild whenever the input, the extra assets or the static directories
  /// change. Also injects a script that reloads the open page after each
  /// rebuild.
  #[clap(long)]
  watch: bool,

  /// How often to poll for changes while watching.
  #[clap(long, default_value = "400", value_name = "MS")]
  poll_ms: u64,
}

/// Applies the preprocessor's byte-range replacements back-to-front, so that
/// each splice leaves the ranges of the not-yet-applied ones valid.
fn apply(content: &str, mut edits: Vec<(std::ops::Range<usize>, String)>) -> String {
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

/// Copies `src` into `<dst_dir>/<file name>` and returns the name, for linking.
fn copy_into(src: &PathBuf, dst_dir: &PathBuf) -> Result<String> {
  let name = src
    .file_name()
    .with_context(|| format!("{} has no file name", src.display()))?
    .to_string_lossy()
    .into_owned();
  fs::copy(src, dst_dir.join(&name))
    .with_context(|| format!("copying {}", src.display()))?;
  Ok(name)
}

fn copy_tree(src: &PathBuf, dst: &PathBuf) -> Result<()> {
  fs::create_dir_all(dst)?;
  for entry in fs::read_dir(src)? {
    let entry = entry?;
    let to = dst.join(entry.file_name());
    if entry.file_type()?.is_dir() {
      copy_tree(&entry.path(), &to)?;
    } else {
      fs::copy(entry.path(), to)?;
    }
  }
  Ok(())
}

fn build(args: &Args, preprocessor: &mut AquascopePreprocessor) -> Result<()> {
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

  let content = apply(body, preprocessor.replacements(body)?);
  preprocessor.save_cache();

  let slides = deck::Deck::parse(&content).to_html();

  let out = &args.out_dir;
  let aquascope_dir = out.join("aquascope");
  let assets_dir = out.join("assets");
  fs::create_dir_all(&aquascope_dir)?;
  fs::create_dir_all(&assets_dir)?;

  fs::write(aquascope_dir.join("embed.iife.js"), EMBED_JS)?;
  fs::write(aquascope_dir.join("style.css"), EMBED_CSS)?;
  fs::write(aquascope_dir.join("aquascope-reveal.js"), GLUE_JS)?;
  fs::write(aquascope_dir.join("aquascope-reveal.css"), GLUE_CSS)?;
  fs::write(aquascope_dir.join("highlight.css"), HIGHLIGHT_CSS)?;

  // Either vendor reveal.js next to the deck or point at a CDN.
  let (reveal, plugin) = match &args.reveal_dir {
    Some(dir) => {
      for sub in ["dist", "plugin"] {
        let from = dir.join(sub);
        anyhow::ensure!(
          from.is_dir(),
          "{} is not a reveal.js package: no {sub}/ directory",
          dir.display()
        );
        copy_tree(&from, &out.join("reveal").join(sub))
          .with_context(|| format!("copying {}", from.display()))?;
      }
      ("reveal/dist".to_string(), "reveal/plugin".to_string())
    }
    None => (format!("{CDN}/dist"), format!("{CDN}/plugin")),
  };

  for dir in &args.static_dirs {
    let name = dir
      .file_name()
      .with_context(|| format!("{} has no directory name", dir.display()))?;
    copy_tree(dir, &out.join(name))
      .with_context(|| format!("copying {}", dir.display()))?;
  }

  // Explicit flags win over the front matter, which wins over the defaults.
  let reveal_options = front.reveal_options_json();
  let title = args
    .title
    .clone()
    .or(front.title)
    .unwrap_or_else(|| {
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

  let mut head = String::new();
  for css in &args.extra_css {
    let name = copy_into(css, &assets_dir)?;
    head.push_str(&format!(
      "  <link rel=\"stylesheet\" href=\"assets/{name}\" />\n"
    ));
  }

  let mut tail = String::new();
  for js in &args.extra_js {
    let name = copy_into(js, &assets_dir)?;
    tail.push_str(&format!("  <script src=\"assets/{name}\"></script>\n"));
  }

  // The stamp changes on every rebuild; livereload.js polls it and reloads.
  if args.watch {
    fs::write(aquascope_dir.join("livereload.js"), LIVERELOAD_JS)?;
    fs::write(out.join("build-stamp.txt"), stamp())?;
    tail.push_str("  <script src=\"aquascope/livereload.js\"></script>\n");
  }

  let html = format!(
    r#"<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="utf-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1.0" />
  <title>{title}</title>
  <link rel="stylesheet" href="{reveal}/reset.css" />
  <link rel="stylesheet" href="{reveal}/reveal.css" />
  <link rel="stylesheet" href="{reveal}/theme/{theme}.css" id="theme" />
  <link rel="stylesheet" href="aquascope/aquascope-reveal.css" />
  <link rel="stylesheet" href="aquascope/highlight.css" />
  <link rel="stylesheet" href="aquascope/style.css" />
{head}</head>
<body>
  <div class="reveal">
    <div class="slides">
{slides}    </div>
  </div>
  <script src="{reveal}/reveal.js"></script>
  <script src="{plugin}/highlight/highlight.js"></script>
  <script src="{plugin}/notes/notes.js"></script>
  <script>window.AQUASCOPE_REVEAL_OPTIONS = {reveal_options};</script>
  <script src="aquascope/aquascope-reveal.js"></script>
{tail}</body>
</html>
"#,
    title = escape(&title),
  );

  let index = out.join("index.html");
  fs::write(&index, html)?;
  println!("Wrote {}", index.display());

  Ok(())
}

/// A value that changes on every rebuild. Nanosecond resolution is well past
/// what the poll interval can distinguish, so successive builds never collide.
fn stamp() -> String {
  SystemTime::now()
    .duration_since(SystemTime::UNIX_EPOCH)
    .map(|d| d.as_nanos().to_string())
    .unwrap_or_default()
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
  let mut preprocessor = AquascopePreprocessor::new()?;

  // Fail loudly on the first build; once watching, a bad edit should only
  // print and leave the watcher running.
  build(&args, &mut preprocessor)?;

  if let Some(port) = args.serve {
    serve::spawn(args.out_dir.clone(), port)?;
  }

  if !args.watch {
    // Serving without watching still has to stay alive.
    if args.serve.is_some() {
      thread::park();
    }
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

    if let Err(e) = build(&args, &mut preprocessor) {
      eprintln!("Build failed: {e:#}");
    }
  }
}
