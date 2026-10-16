use std::{
  env, fs,
  path::{Path, PathBuf},
};

use anyhow::{ensure, Context, Result};

/// Built by `depot build --release` in the frontend workspace.
const SRC_DIR: &str = "../../frontend/packages/aquascope-embed/dist/";
const DST_DIR: &str = "./js";

/// The subset of the embed bundle we actually link into a deck. The source map
/// is deliberately excluded -- it is large and only useful when debugging the
/// frontend itself.
const ASSETS: [&str; 2] = ["embed.iife.js", "style.css"];

/// Where the frontend workspace installs the third-party files a deck needs:
/// reveal.js, highlight.js and Font Awesome, devDependencies of
/// aquascope-embed so that `depot build` fetches them.
const NODE_MODULES: &str =
  "../../frontend/packages/aquascope-embed/node_modules";

#[allow(dead_code)]
#[path = "src/stylesheet.rs"]
mod stylesheet;

fn main() -> Result<()> {
  copy_embed()?;
  Vendor::default().write()
}

fn copy_embed() -> Result<()> {
  let src = Path::new(SRC_DIR);
  let dst = Path::new(DST_DIR);
  fs::create_dir_all(dst)?;

  for asset in ASSETS {
    let from = src.join(asset);
    ensure!(
      from.exists(),
      "missing {}. Run `depot build --release` in ./frontend first.",
      from.display()
    );
    fs::copy(&from, dst.join(asset))
      .with_context(|| format!("copying {}", from.display()))?;
    println!("cargo:rerun-if-changed={}", from.display());
  }
  check_release(&dst.join("embed.iife.js"))
}

/// A plain `depot build` leaves the bundle unminified and with React's
/// development checks, 2.5 MB against 0.9 MB, and every deck carries it --
/// a `--dist single` file inlines it whole. Fine while working on the
/// frontend, so only a release build of this crate, which is what
/// `cargo install` makes, refuses it.
fn check_release(bundle: &Path) -> Result<()> {
  let text = fs::read_to_string(bundle)?;
  if !text.contains("react-dom.development") {
    return Ok(());
  }
  let message = "the embed bundle is a development build. Run `depot build \
                 --release` in ./frontend.";
  ensure!(env::var("PROFILE").as_deref() != Ok("release"), "{message}");
  println!("cargo:warning={message}");
  Ok(())
}

/// The vendor table, `$OUT_DIR/vendor.rs`: every third-party file a deck may
/// need, as `(path, bytes)`, the path being the package's name and its own
/// layout, so a stylesheet's relative `url()`s still resolve.
///
/// Only what a deck loads is taken, and a few files are changed on the way:
///
/// - reveal.js 5.1.0 (MIT): `dist/reset.css`, `dist/reveal.css`,
///   `dist/reveal.js` without its `sourceMappingURL` comment, since the map
///   is not taken, the themes and their fonts, and the notes plugin. Not the
///   highlight plugin, which carries every highlight.js grammar.
/// - highlight.js 11.9.0 (BSD-3-Clause), from `@highlightjs/cdn-assets`: the
///   core and every grammar. A deck includes the grammars its fences name.
///   The grammars are classic scripts that register themselves on a global
///   `hljs`, but the core is published only as an ES module, so it is
///   wrapped as `var hljs=(()=>{...;return <default export>})();`.
/// - Font Awesome 4.7.0 (font SIL OFL 1.1, CSS MIT): its stylesheet and font.
///
/// Every font stylesheet is put through [`stylesheet::slim`], which cuts each
/// `@font-face` to the one source every browser a deck runs in reads, and
/// only the font files still referred to are taken: woff for the reveal
/// themes, woff2 for Font Awesome. The other formats are for Internet
/// Explorer and old Android.
#[derive(Default)]
struct Vendor {
  /// `include_bytes!` entries.
  table: String,
}

impl Vendor {
  fn write(mut self) -> Result<()> {
    let modules = Path::new(NODE_MODULES);
    ensure!(
      modules.join("reveal.js").exists(),
      "missing {}/reveal.js. Run `depot build --release` in ./frontend first.",
      modules.display()
    );

    let reveal = &package("reveal.js", "5.1.0")?;
    self.copy(reveal, "reveal.js", "dist/reset.css")?;
    self.copy(reveal, "reveal.js", "dist/reveal.css")?;
    let js = read_text(&reveal.join("dist/reveal.js"))?;
    let js: Vec<&str> = js
      .lines()
      .filter(|line| !line.starts_with("//# sourceMappingURL="))
      .collect();
    self.add("reveal.js/dist/reveal.js", js.join("\n").into_bytes())?;
    self.copy(reveal, "reveal.js", "plugin/notes/notes.js")?;
    for theme in files(&reveal.join("dist/theme"), "css")? {
      self.copy(reveal, "reveal.js", &format!("dist/theme/{theme}"))?;
    }
    for font in files(&reveal.join("dist/theme/fonts"), "")? {
      for css in files(&reveal.join("dist/theme/fonts").join(&font), "css")? {
        self.font_css(
          reveal,
          "reveal.js",
          &format!("dist/theme/fonts/{font}/{css}"),
        )?;
      }
    }

    let hljs = &package("@highlightjs/cdn-assets", "11.9.0")?;
    let core = read_text(&hljs.join("es/core.min.js"))?;
    let (body, export) = core
      .trim_end()
      .rsplit_once("export{")
      .context("highlight.js's core has no `export{`")?;
    let name = export
      .strip_suffix(" as default};")
      .context("highlight.js's core does not end in a default export")?;
    self.add(
      "highlight.js/core.min.js",
      format!("var hljs=(()=>{{{body};return {name}}})();\n").into_bytes(),
    )?;
    for grammar in files(&hljs.join("languages"), "js")? {
      if grammar.ends_with(".min.js") {
        self.copy(hljs, "highlight.js", &format!("languages/{grammar}"))?;
      }
    }

    let fa = &package("font-awesome", "4.7.0")?;
    self.font_css(fa, "font-awesome", "css/font-awesome.min.css")?;

    let mut out = String::from("pub const VENDOR: &[(&str, &[u8])] = &[\n");
    out.push_str(&self.table);
    out.push_str("];\n");
    fs::write(out_dir()?.join("vendor.rs"), out)?;
    Ok(())
  }

  /// `path` in the package at `dir`, as `{name}/{path}`, unchanged.
  fn copy(&mut self, dir: &Path, name: &str, path: &str) -> Result<()> {
    let file = fs::canonicalize(dir.join(path))
      .with_context(|| format!("{name} has no {path}"))?;
    self.entry(&format!("{name}/{path}"), &file);
    Ok(())
  }

  /// `bytes`, written out to be included as `path`.
  fn add(&mut self, path: &str, bytes: Vec<u8>) -> Result<()> {
    let file = out_dir()?.join("vendor").join(path);
    fs::create_dir_all(file.parent().unwrap())?;
    fs::write(&file, bytes)?;
    self.entry(path, &file);
    Ok(())
  }

  fn entry(&mut self, path: &str, file: &Path) {
    self.table.push_str(&format!(
      "  ({path:?}, include_bytes!({:?})),\n",
      file.display().to_string()
    ));
  }

  /// A font stylesheet, slimmed, and the fonts it still refers to.
  fn font_css(&mut self, dir: &Path, name: &str, path: &str) -> Result<()> {
    let css = stylesheet::slim(&read_text(&dir.join(path))?).css;
    let base = Path::new(path).parent().unwrap();
    for url in urls(&css) {
      let url = &url[.. url.find(['?', '#']).unwrap_or(url.len())];
      let font = normalize(&base.join(url));
      self.copy(dir, name, &font)?;
    }
    self.add(&format!("{name}/{path}"), css.into_bytes())
  }
}

/// The installed package `name`, checked to be `version`: the docs and the
/// vendored file layout above are for that version.
fn package(name: &str, version: &str) -> Result<PathBuf> {
  let dir = Path::new(NODE_MODULES).join(name);
  let manifest = read_text(&dir.join("package.json"))?;
  ensure!(
    manifest.contains(&format!("\"version\": \"{version}\"")),
    "{} is not {name} {version}. Run `depot build --release` in ./frontend.",
    dir.display()
  );
  println!("cargo:rerun-if-changed={}", dir.display());
  Ok(dir)
}

/// The names in `dir` ending in `.{ext}`, or every directory for `""`, sorted.
fn files(dir: &Path, ext: &str) -> Result<Vec<String>> {
  let mut names = Vec::new();
  for entry in
    fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))?
  {
    let entry = entry?;
    let name = entry.file_name().to_string_lossy().into_owned();
    let wanted = if ext.is_empty() {
      entry.file_type()?.is_dir()
    } else {
      name.ends_with(&format!(".{ext}"))
    };
    if wanted {
      names.push(name);
    }
  }
  names.sort();
  Ok(names)
}

/// The `url()`s in a minified stylesheet.
fn urls(css: &str) -> Vec<String> {
  css
    .split("url(")
    .skip(1)
    .filter_map(|rest| {
      let url = &rest[.. rest.find(')')?];
      Some(url.trim_matches(['"', '\'']).to_string())
    })
    .collect()
}

/// `path` with its `.` and `..` resolved, `/`-separated.
fn normalize(path: &Path) -> String {
  let mut parts: Vec<String> = Vec::new();
  for part in path.components() {
    match part.as_os_str().to_string_lossy().as_ref() {
      "." => {}
      ".." => {
        parts.pop();
      }
      part => parts.push(part.to_string()),
    }
  }
  parts.join("/")
}

fn read_text(path: &Path) -> Result<String> {
  fs::read_to_string(path)
    .with_context(|| format!("reading {}", path.display()))
}

fn out_dir() -> Result<PathBuf> {
  Ok(PathBuf::from(env::var("OUT_DIR")?))
}
