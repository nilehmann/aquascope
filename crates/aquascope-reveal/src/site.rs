//! The built deck, held in memory, and the ways of writing it out.
//!
//! A build produces a [`Site`]: the page, as the stylesheets and scripts it
//! includes around the slides, and every other file the deck needs, keyed by
//! the path the page refers to it by. Nothing is on disk yet, which is what
//! lets the same build be written three ways:
//!
//! - [`write_dir`], file by file, for `--serve`;
//! - [`dist_dir`], the same into a fresh directory that replaces the target
//!   only once it is complete;
//! - [`dist_single`], one `.html` with every file the page refers to
//!   inlined into it.
//!
//! Both `--dist` modes first [`Site::check_refs`]: every relative reference
//! in the slides and the stylesheets has to name a file in the site, since a
//! deck handed out with a broken image is not found out until it is shown.

use std::{
  collections::{BTreeMap, BTreeSet},
  fs,
  ops::Range,
  path::{Path, PathBuf},
};

use anyhow::{bail, Context, Result};

use crate::stylesheet;

/// Written into a `--dist dir` output. A directory holding it is one this
/// tool built, and may be replaced wholesale by the next build.
pub const MARKER: &str = ".aquascope-reveal";

/// The page's own path, which a reference to the deck's directory (`./`)
/// names.
const PAGE: &str = "index.html";

/// Assets the embed bundle refers to from markup it creates at runtime, which
/// no scan of the page can see. A single file maps them to inlined copies in
/// `window.AQUASCOPE_ASSETS`, where the bundle looks before using the path.
const RUNTIME_ASSETS: &[&str] = &["img/ferris/does_not_compile.svg"];

/// An inlined file larger than this is worth a warning: it is carried by the
/// one file, base64 and all, by everyone the deck is sent to.
const LARGE_FILE: usize = 5 << 20;

/// Something the page includes, in order.
pub enum Include {
  Css {
    path: String,
    id: Option<&'static str>,
  },
  Js(String),
  /// Code written into the page itself, in both kinds of output.
  InlineJs(String),
  /// The embed bundle, which aquascope-reveal.js loads itself once the page
  /// has loaded. Nothing in the page refers to it in a directory, and it is
  /// an inert script in a single file. See `loadEmbed` there.
  Embed(String),
}

pub struct Page {
  pub title: String,
  pub head: Vec<Include>,
  /// The `<div class="reveal">` and everything in it.
  pub body: String,
  pub tail: Vec<Include>,
}

pub struct Site {
  pub page: Page,
  files: BTreeMap<String, Vec<u8>>,
}

impl Site {
  pub fn new(page: Page) -> Self {
    Site {
      page,
      files: BTreeMap::new(),
    }
  }

  /// Adds a file at `path`, replacing any already there.
  pub fn add(&mut self, path: impl Into<String>, bytes: impl Into<Vec<u8>>) {
    self.files.insert(path.into(), bytes.into());
  }

  /// Adds every file under `dir` below `prefix`.
  pub fn add_tree(&mut self, prefix: &str, dir: &Path) -> Result<()> {
    for entry in
      fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))?
    {
      let entry = entry?;
      let name = entry.file_name().to_string_lossy().into_owned();
      let path = format!("{prefix}/{name}");
      if entry.file_type()?.is_dir() {
        self.add_tree(&path, &entry.path())?;
      } else {
        let bytes = fs::read(entry.path())
          .with_context(|| format!("reading {}", entry.path().display()))?;
        self.add(path, bytes);
      }
    }
    Ok(())
  }

  fn file(&self, path: &str) -> Option<&[u8]> {
    self.files.get(path).map(Vec::as_slice)
  }

  /// Every relative reference the page and its stylesheets make that names
  /// no file in the site, as `slide N: ...` or `<stylesheet>: ...`.
  pub fn broken_refs(&self) -> Vec<String> {
    let mut broken = Vec::new();
    for r in html_refs(&self.page.body) {
      let slide = slide_of(&self.page.body, r.at);
      match resolve("", &r.url) {
        Err(why) => broken.push(format!("slide {slide}: {why}")),
        Ok(target) if target != PAGE && self.file(&target).is_none() => broken
          .push(format!(
            "slide {slide}: `{}` is not in any --static directory",
            r.url
          )),
        Ok(_) => {}
      }
    }
    for include in self.page.head.iter() {
      if let Include::Css { path, .. } = include {
        self.broken_css_refs(path, &mut broken, &mut BTreeSet::new());
      }
    }
    broken
  }

  fn broken_css_refs(
    &self,
    path: &str,
    broken: &mut Vec<String>,
    seen: &mut BTreeSet<String>,
  ) {
    if !seen.insert(path.to_string()) {
      return;
    }
    let Some(bytes) = self.file(path) else {
      broken.push(format!("`{path}` is linked but was not built"));
      return;
    };
    let css = String::from_utf8_lossy(bytes);
    for r in css_refs(&css) {
      match resolve(dir_of(path), &r.url) {
        Err(why) => broken.push(format!("{path}: {why}")),
        Ok(target) => match self.file(&target) {
          None => broken.push(format!("{path}: `{}` does not exist", r.url)),
          Some(_) if r.import => self.broken_css_refs(&target, broken, seen),
          Some(_) => {}
        },
      }
    }
  }

  /// Fails, listing every broken reference, if there are any.
  pub fn check_refs(&self) -> Result<()> {
    let broken = self.broken_refs();
    if broken.is_empty() {
      return Ok(());
    }
    bail!(
      "the deck refers to files it does not have:\n  {}\nPut them in a \
       directory passed with --static, and refer to them by its name, e.g. \
       `img/diagram.png` for --static img.",
      broken.join("\n  ")
    )
  }

  /// The page, referring to the site's files by path.
  pub fn render_dir(&self) -> String {
    let linked = |includes: &[Include]| -> String {
      includes
        .iter()
        .map(|include| match include {
          Include::Css { path, id } => format!(
            "  <link rel=\"stylesheet\" href=\"{path}\"{} />\n",
            id_attr(*id)
          ),
          Include::Js(path) => format!("  <script src=\"{path}\"></script>\n"),
          Include::InlineJs(code) => format!("  <script>{code}</script>\n"),
          Include::Embed(_) => String::new(),
        })
        .collect()
    };
    document(
      &self.page.title,
      &linked(&self.page.head),
      &self.page.body,
      &linked(&self.page.tail),
    )
  }

  /// The page with everything it refers to inside it. References that are
  /// not relative -- `https:` fonts, `data:` URIs, `#fragments` -- are left
  /// as they are.
  pub fn render_single(&self) -> Result<String> {
    self.check_refs()?;
    let mut inliner = Inliner {
      site: self,
      warned: BTreeSet::new(),
      importing: BTreeSet::new(),
    };

    let mut head = String::new();
    for include in &self.page.head {
      head.push_str(&inliner.include(include)?);
    }

    // Before the embed bundle, which reads it.
    let mut assets = BTreeMap::new();
    for path in RUNTIME_ASSETS {
      if self.file(path).is_some() {
        assets.insert(*path, inliner.data_uri(path)?);
      }
    }
    let mut tail = format!(
      "  <script>window.AQUASCOPE_ASSETS = {};</script>\n",
      escape_script(&serde_json::to_string(&assets)?)
    );
    for include in &self.page.tail {
      tail.push_str(&inliner.include(include)?);
    }

    let body = inliner.html(&self.page.body)?;
    Ok(document(&self.page.title, &head, &body, &tail))
  }
}

fn id_attr(id: Option<&str>) -> String {
  id.map(|id| format!(" id=\"{id}\"")).unwrap_or_default()
}

fn document(title: &str, head: &str, body: &str, tail: &str) -> String {
  format!(
    r#"<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="utf-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1.0" />
  <title>{title}</title>
{head}</head>
<body>
{body}{tail}</body>
</html>
"#
  )
}

/// Turns references into `data:` URIs, warning once about each large file.
struct Inliner<'a> {
  site: &'a Site,
  warned: BTreeSet<String>,
  /// The stylesheets being inlined, to catch an `@import` cycle.
  importing: BTreeSet<String>,
}

impl Inliner<'_> {
  fn bytes(&mut self, path: &str) -> Result<&[u8]> {
    let bytes = self
      .site
      .file(path)
      .with_context(|| format!("`{path}` is not in the deck"))?;
    if bytes.len() > LARGE_FILE && self.warned.insert(path.to_string()) {
      eprintln!(
        "warning: `{path}` is {:.1} MB, and is inlined into the single file",
        bytes.len() as f64 / (1 << 20) as f64
      );
    }
    Ok(bytes)
  }

  fn text(&mut self, path: &str) -> Result<String> {
    Ok(String::from_utf8_lossy(self.bytes(path)?).into_owned())
  }

  fn data_uri(&mut self, path: &str) -> Result<String> {
    let mime = media_type(path);
    if mime == "text/css" {
      // Inlined itself first: its own relative references would otherwise
      // resolve against the `data:` URI, which has no directory.
      let css = self.css(path)?;
      return Ok(format!("data:text/css;base64,{}", base64(css.as_bytes())));
    }
    let bytes = self.bytes(path)?;
    Ok(format!("data:{mime};base64,{}", base64(bytes)))
  }

  fn include(&mut self, include: &Include) -> Result<String> {
    Ok(match include {
      Include::Css { path, id } => format!(
        "  <style{}>\n{}\n  </style>\n",
        id_attr(*id),
        escape_style(&self.css(path)?)
      ),
      Include::Js(path) => {
        format!("  <script>\n{}\n  </script>\n", escape_script(&self.text(path)?))
      }
      Include::InlineJs(code) => format!("  <script>{code}</script>\n"),
      Include::Embed(path) => format!(
        "  <script type=\"text/x-aquascope-embed\" id=\"aquascope-embed\">\n{}\n  </script>\n",
        escape_script(&self.text(path)?)
      ),
    })
  }

  /// The stylesheet at `path`, [slimmed](stylesheet::slim): preceded by the
  /// stylesheets it imports, and its other relative `url()`s and `@import`s
  /// replaced by `data:` URIs.
  fn css(&mut self, path: &str) -> Result<String> {
    if !self.importing.insert(path.to_string()) {
      bail!("`{path}` imports itself");
    }
    let slim = stylesheet::slim(&self.text(path)?);
    let mut out = String::new();
    for url in &slim.imports {
      let target = resolve(dir_of(path), url).map_err(anyhow::Error::msg)?;
      out.push_str(&self.css(&target)?);
      out.push('\n');
    }
    let mut css = slim.css;
    let mut refs = css_refs(&css);
    refs.reverse();
    for r in refs {
      let target = resolve(dir_of(path), &r.url).map_err(anyhow::Error::msg)?;
      let uri = self.data_uri(&target)?;
      css.replace_range(r.range, &format!("url(\"{uri}\")"));
    }
    self.importing.remove(path);
    out.push_str(&css);
    Ok(out)
  }

  /// The slides, with every relative reference inlined.
  fn html(&mut self, html: &str) -> Result<String> {
    let mut out = html.to_string();
    let mut refs = html_refs(html);
    refs.reverse();
    for r in refs {
      let target = resolve("", &r.url).map_err(anyhow::Error::msg)?;
      if target == PAGE {
        // The deck itself, which in one file is the document: keep only the
        // fragment, as `./#/3` would.
        let fragment = r.url.find('#').map_or("", |i| &r.url[i ..]);
        out.replace_range(
          r.range,
          &fragment.replace('&', "&amp;").replace('"', "&quot;"),
        );
        continue;
      }
      if r.link {
        bail!(
          "slide {}: `{}` is a link to a file, which a single .html cannot \
           carry -- browsers refuse to open a `data:` URI from a link. Use \
           --dist dir for this deck.",
          slide_of(html, r.at),
          r.url
        );
      }
      let uri = self.data_uri(&target)?;
      out.replace_range(r.range, &match (r.css, r.in_attr) {
        (true, true) => format!("url(&quot;{uri}&quot;)"),
        (true, false) => format!("url(\"{uri}\")"),
        (false, _) => uri,
      });
    }
    Ok(out)
  }
}

/// A relative reference found in HTML or CSS.
#[derive(Debug, PartialEq)]
struct Ref {
  /// What it says, unescaped.
  url: String,
  /// What to replace to inline it: the attribute value, or for CSS the
  /// whole `url(...)` or `@import` string.
  range: Range<usize>,
  /// Where it is, for naming the slide.
  at: usize,
  /// A CSS `@import`, whose target is a stylesheet with references of its
  /// own.
  import: bool,
  /// The range is a whole `url(...)` or `@import` string, replaced by a
  /// `url(...)`; otherwise it is an attribute value, replaced by the URI.
  css: bool,
  /// In an attribute, so quotes in the replacement are written `&quot;`.
  in_attr: bool,
  /// A link to a page or a download (`<a href>`), not something the page
  /// shows.
  link: bool,
}

/// Whether `url` refers to a file next to the page: not a scheme, not
/// protocol-relative, not a fragment, not empty.
fn is_relative(url: &str) -> bool {
  let url = url.trim();
  if url.is_empty() || url.starts_with('#') || url.starts_with("//") {
    return false;
  }
  let scheme = url
    .find(':')
    .map(|colon| {
      let s = &url[.. colon];
      !s.is_empty()
        && s.starts_with(|c: char| c.is_ascii_alphabetic())
        && s
          .chars()
          .all(|c| c.is_ascii_alphanumeric() || "+.-".contains(c))
    })
    .unwrap_or(false);
  !scheme
}

/// The site path `url` names, relative to the directory `base` (`""` for the
/// page). A directory names its `index.html`, as a server would serve it. An
/// error says why it names none.
fn resolve(base: &str, url: &str) -> Result<String, String> {
  let path = url.trim();
  let path = &path[.. path.find(['?', '#']).unwrap_or(path.len())];
  if path.starts_with('/') {
    return Err(format!(
      "`{url}` is relative to the server's root, which a deck opened as a \
       file or hosted in a subdirectory does not have; drop the leading `/`"
    ));
  }
  let path = percent_decode(path);
  let mut parts: Vec<&str> =
    base.split('/').filter(|p| !p.is_empty()).collect();
  let dir = matches!(path.rsplit('/').next(), Some("" | "." | ".."));
  for part in path.split('/') {
    match part {
      "" | "." => {}
      ".." => {
        if parts.pop().is_none() {
          return Err(format!("`{url}` points outside the deck"));
        }
      }
      part => parts.push(part),
    }
  }
  if dir {
    parts.push(PAGE);
  }
  Ok(parts.join("/"))
}

fn dir_of(path: &str) -> &str {
  path.rfind('/').map(|i| &path[.. i]).unwrap_or("")
}

fn percent_decode(s: &str) -> String {
  let bytes = s.as_bytes();
  let mut out = Vec::with_capacity(bytes.len());
  let mut i = 0;
  while i < bytes.len() {
    if bytes[i] == b'%' && i + 2 < bytes.len() {
      let hex = std::str::from_utf8(&bytes[i + 1 .. i + 3]).unwrap_or("");
      if let Ok(b) = u8::from_str_radix(hex, 16) {
        out.push(b);
        i += 3;
        continue;
      }
    }
    out.push(bytes[i]);
    i += 1;
  }
  String::from_utf8_lossy(&out).into_owned()
}

fn unescape_html(s: &str) -> String {
  s.replace("&quot;", "\"")
    .replace("&#39;", "'")
    .replace("&lt;", "<")
    .replace("&gt;", ">")
    .replace("&amp;", "&")
}

/// The slide `offset` in `body` is on, counting from 1. A slide is a
/// `<section>` holding no other: the wrapper of a vertical stack is not one.
fn slide_of(body: &str, offset: usize) -> usize {
  let mut n = 0;
  let mut from = 0;
  while let Some(i) = body[from ..].find("<section") {
    let start = from + i;
    if start > offset {
      break;
    }
    let after = body[start ..]
      .find('>')
      .map(|end| body[start + end + 1 ..].trim_start())
      .unwrap_or("");
    if !after.starts_with("<section") {
      n += 1;
    }
    from = start + 1;
  }
  n.max(1)
}

/// Attributes whose value is a file the page shows, for any element.
const SRC_ATTRS: &[&str] = &[
  "src",
  "poster",
  "data-src",
  "data-background-image",
  "data-background-video",
];

/// Every relative reference in `html`: `src`-like attributes, `href`s,
/// `style` attributes and `<style>` elements. Comments, scripts and text
/// are skipped -- code on a slide is escaped text, never a tag.
fn html_refs(html: &str) -> Vec<Ref> {
  let bytes = html.as_bytes();
  let mut refs = Vec::new();
  let mut i = 0;
  while let Some(lt) = html[i ..].find('<') {
    let start = i + lt;
    if html[start ..].starts_with("<!--") {
      i = html[start ..]
        .find("-->")
        .map(|e| start + e + 3)
        .unwrap_or(html.len());
      continue;
    }
    let name_len = html[start + 1 ..]
      .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
      .unwrap_or(html.len() - start - 1);
    if name_len == 0 {
      i = start + 1;
      continue;
    }
    let tag = html[start + 1 .. start + 1 + name_len].to_ascii_lowercase();

    // The attributes, up to the `>` that is not inside a quoted value.
    let mut j = start + 1 + name_len;
    loop {
      while j < bytes.len() && bytes[j].is_ascii_whitespace() {
        j += 1;
      }
      if j >= bytes.len() || bytes[j] == b'>' {
        break;
      }
      if bytes[j] == b'/' {
        j += 1;
        continue;
      }
      let name_start = j;
      while j < bytes.len()
        && !bytes[j].is_ascii_whitespace()
        && !b"=>/".contains(&bytes[j])
      {
        j += 1;
      }
      let name = html[name_start .. j].to_ascii_lowercase();
      while j < bytes.len() && bytes[j].is_ascii_whitespace() {
        j += 1;
      }
      if j >= bytes.len() || bytes[j] != b'=' {
        continue;
      }
      j += 1;
      while j < bytes.len() && bytes[j].is_ascii_whitespace() {
        j += 1;
      }
      let value = if j < bytes.len() && (bytes[j] == b'"' || bytes[j] == b'\'')
      {
        let quote = bytes[j];
        let v_start = j + 1;
        let v_end = html[v_start ..]
          .find(quote as char)
          .map(|e| v_start + e)
          .unwrap_or(html.len());
        j = (v_end + 1).min(html.len());
        v_start .. v_end
      } else {
        let v_start = j;
        while j < bytes.len()
          && !bytes[j].is_ascii_whitespace()
          && bytes[j] != b'>'
        {
          j += 1;
        }
        v_start .. j
      };

      let raw = &html[value.clone()];
      let link = name == "href" && (tag == "a" || tag == "area")
        || name == "data-background-iframe";
      if SRC_ATTRS.contains(&name.as_str()) || name == "href" || link {
        let url = unescape_html(raw);
        if is_relative(&url) {
          refs.push(Ref {
            url,
            range: value.clone(),
            at: start,
            import: false,
            css: false,
            in_attr: false,
            link,
          });
        }
      } else if name == "style" {
        let css = unescape_html(raw);
        // Only when unescaping changed nothing do the ranges still line up,
        // which is every style attribute short of one with an entity in it.
        if css == raw {
          for r in css_refs(raw) {
            refs.push(Ref {
              range: value.start + r.range.start .. value.start + r.range.end,
              at: start,
              in_attr: true,
              ..r
            });
          }
        }
      }
    }
    i = (j + 1).min(html.len());

    // A script's text is not markup; a style's is CSS.
    if tag == "script" || tag == "style" {
      let close = format!("</{tag}");
      let end = find_ci(&html[i ..], &close)
        .map(|e| i + e)
        .unwrap_or(html.len());
      if tag == "style" {
        for r in css_refs(&html[i .. end]) {
          refs.push(Ref {
            range: i + r.range.start .. i + r.range.end,
            at: start,
            ..r
          });
        }
      }
      i = end;
    }
  }
  refs
}

/// Every relative reference in `css`: `url(...)`, quoted or not, and
/// `@import "..."`. The range covers the whole `url(...)`, or the import's
/// string, so either is replaced by a `url(...)`.
fn css_refs(css: &str) -> Vec<Ref> {
  let mut refs = Vec::new();
  let mut i = 0;
  while i < css.len() {
    let rest = &css[i ..];
    if rest.starts_with("/*") {
      i += rest.find("*/").map(|e| e + 2).unwrap_or(rest.len());
      continue;
    }
    if let Some(after) = strip_prefix_ci(rest, "@import") {
      let skip = after.len() - after.trim_start().len();
      let s = i + "@import".len() + skip;
      let q = css.as_bytes().get(s).copied();
      if q == Some(b'"') || q == Some(b'\'') {
        if let Some(e) = css[s + 1 ..].find(q.unwrap() as char) {
          let url = css[s + 1 .. s + 1 + e].to_string();
          let end = s + 2 + e;
          if is_relative(&url) {
            refs.push(Ref {
              url,
              range: s .. end,
              at: s,
              import: true,
              css: true,
              in_attr: false,
              link: false,
            });
          }
          i = end;
          continue;
        }
      }
      // `@import url(...)` is found as a url() below, flagged by looking
      // back at what precedes it.
      i += "@import".len();
      continue;
    }
    if let Some(after) = strip_prefix_ci(rest, "url(") {
      let open = i + 4;
      let inner = after.trim_start();
      let lead = after.len() - inner.len();
      let (url, close) = match inner.as_bytes().first() {
        Some(&q) if q == b'"' || q == b'\'' => {
          match inner[1 ..].find(q as char) {
            Some(e) => {
              let url = &inner[1 .. 1 + e];
              let after_q = open + lead + 2 + e;
              let close = css[after_q ..].find(')').map(|c| after_q + c + 1);
              (url.to_string(), close)
            }
            None => (String::new(), None),
          }
        }
        _ => match inner.find(')') {
          Some(e) => {
            (inner[.. e].trim().to_string(), Some(open + lead + e + 1))
          }
          None => (String::new(), None),
        },
      };
      let Some(close) = close else {
        break;
      };
      if is_relative(&url) {
        let before = css[.. i].trim_end();
        refs.push(Ref {
          url,
          range: i .. close,
          at: i,
          import: ends_with_ci(before, "@import"),
          css: true,
          in_attr: false,
          link: false,
        });
      }
      i = close;
      continue;
    }
    i += rest.chars().next().map(char::len_utf8).unwrap_or(1);
  }
  refs
}

fn strip_prefix_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
  (s.len() >= prefix.len()
    && s.is_char_boundary(prefix.len())
    && s[.. prefix.len()].eq_ignore_ascii_case(prefix))
  .then(|| &s[prefix.len() ..])
}

fn ends_with_ci(s: &str, suffix: &str) -> bool {
  s.len() >= suffix.len()
    && s.is_char_boundary(s.len() - suffix.len())
    && s[s.len() - suffix.len() ..].eq_ignore_ascii_case(suffix)
}

fn find_ci(haystack: &str, needle: &str) -> Option<usize> {
  haystack
    .as_bytes()
    .windows(needle.len())
    .position(|w| w.eq_ignore_ascii_case(needle.as_bytes()))
}

/// Inserts `insert` after the first `at` bytes of every case-insensitive
/// occurrence of `needle` in `s`, keeping the occurrence as it was written.
fn insert_ci(s: &str, needle: &str, at: usize, insert: &str) -> String {
  let mut out = String::with_capacity(s.len());
  let mut rest = s;
  while let Some(i) = find_ci(rest, needle) {
    out.push_str(&rest[.. i + at]);
    out.push_str(insert);
    out.push_str(&rest[i + at .. i + needle.len()]);
    rest = &rest[i + needle.len() ..];
  }
  out.push_str(rest);
  out
}

/// Script text that cannot end its `<script>` early. `<\/script` and
/// `<\!--` mean the same in a JavaScript string, regex or comment, the only
/// places either can occur.
fn escape_script(js: &str) -> String {
  insert_ci(&insert_ci(js, "</script", 1, "\\"), "<!--", 1, "\\")
}

/// The same for a `<style>`: `\/` is `/` in a CSS string.
fn escape_style(css: &str) -> String {
  insert_ci(css, "</style", 1, "\\")
}

/// The media type a file is served or inlined as.
pub fn media_type(path: &str) -> &'static str {
  let ext = path.rsplit_once('.').map(|(_, e)| e).unwrap_or_default();
  match ext.to_ascii_lowercase().as_str() {
    "html" | "htm" => "text/html",
    "css" => "text/css",
    "js" | "mjs" => "text/javascript",
    "json" | "map" => "application/json",
    "txt" => "text/plain",
    "svg" => "image/svg+xml",
    "png" => "image/png",
    "jpg" | "jpeg" => "image/jpeg",
    "gif" => "image/gif",
    "ico" => "image/x-icon",
    "webp" => "image/webp",
    "avif" => "image/avif",
    "woff2" => "font/woff2",
    "woff" => "font/woff",
    "ttf" => "font/ttf",
    "otf" => "font/otf",
    "eot" => "application/vnd.ms-fontobject",
    "wasm" => "application/wasm",
    "mp4" => "video/mp4",
    "webm" => "video/webm",
    "ogv" => "video/ogg",
    "mp3" => "audio/mpeg",
    "ogg" | "oga" => "audio/ogg",
    "wav" => "audio/wav",
    "pdf" => "application/pdf",
    _ => "application/octet-stream",
  }
}

fn base64(bytes: &[u8]) -> String {
  const ALPHABET: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
  let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
  for chunk in bytes.chunks(3) {
    let n = chunk
      .iter()
      .enumerate()
      .fold(0u32, |n, (i, &b)| n | (b as u32) << (16 - 8 * i));
    for i in 0 .. 4 {
      if i <= chunk.len() {
        out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
      } else {
        out.push('=');
      }
    }
  }
  out
}

/// Writes the site into `dir` as it is, over whatever is there.
pub fn write_dir(site: &Site, dir: &Path) -> Result<()> {
  for (path, bytes) in &site.files {
    let to = dir.join(path);
    if let Some(parent) = to.parent() {
      fs::create_dir_all(parent)?;
    }
    fs::write(&to, bytes)
      .with_context(|| format!("writing {}", to.display()))?;
  }
  fs::create_dir_all(dir)?;
  fs::write(dir.join(PAGE), site.render_dir())?;
  Ok(())
}

/// A sibling of `path` named after it, for building next to it and renaming
/// into place: on the same file system, so the rename cannot fail for being
/// across devices.
fn sibling(path: &Path, what: &str) -> Result<PathBuf> {
  let name = path
    .file_name()
    .with_context(|| format!("{} has no file name", path.display()))?
    .to_string_lossy();
  Ok(path.with_file_name(format!(".{name}.{what}-{}", std::process::id())))
}

/// Writes the site as a directory at `dir`, replacing one an earlier
/// `--dist dir` wrote. The new one is complete before the old one goes.
pub fn dist_dir(site: &Site, dir: &Path) -> Result<()> {
  site.check_refs()?;

  if dir.exists() {
    if !dir.is_dir() {
      bail!("{} exists and is not a directory", dir.display());
    }
    let empty = fs::read_dir(dir)?.next().is_none();
    if !empty && !dir.join(MARKER).exists() {
      bail!(
        "{} is not empty and was not written by `--dist dir`, so it is left \
         alone. Choose another directory with -o, or empty this one.",
        dir.display()
      );
    }
  }
  if let Some(parent) = dir.parent().filter(|p| !p.as_os_str().is_empty()) {
    fs::create_dir_all(parent)?;
  }

  let fresh = sibling(dir, "new")?;
  let _ = fs::remove_dir_all(&fresh);
  let written = write_dir(site, &fresh).and_then(|()| {
    fs::write(
      fresh.join(MARKER),
      "Written by aquascope-reveal --dist dir. The next build replaces this \
       whole directory.\n",
    )?;
    Ok(())
  });
  if let Err(e) = written {
    let _ = fs::remove_dir_all(&fresh);
    return Err(e);
  }

  if dir.exists() {
    let old = sibling(dir, "old")?;
    fs::rename(dir, &old)
      .with_context(|| format!("moving {} aside", dir.display()))?;
    if let Err(e) = fs::rename(&fresh, dir) {
      // Put the old one back rather than leave nothing.
      let _ = fs::rename(&old, dir);
      let _ = fs::remove_dir_all(&fresh);
      return Err(e).with_context(|| format!("replacing {}", dir.display()));
    }
    fs::remove_dir_all(&old)
      .with_context(|| format!("removing the old {}", dir.display()))?;
  } else {
    fs::rename(&fresh, dir)
      .with_context(|| format!("writing {}", dir.display()))?;
  }
  Ok(())
}

/// Writes the site as the one file `file`, replacing it once complete.
pub fn dist_single(site: &Site, file: &Path) -> Result<usize> {
  if file.is_dir() {
    bail!(
      "{} is a directory; --dist single writes a file",
      file.display()
    );
  }
  let html = site.render_single()?;
  if let Some(parent) = file.parent().filter(|p| !p.as_os_str().is_empty()) {
    fs::create_dir_all(parent)?;
  }
  let fresh = sibling(file, "new")?;
  fs::write(&fresh, &html)
    .with_context(|| format!("writing {}", fresh.display()))?;
  fs::rename(&fresh, file)
    .with_context(|| format!("writing {}", file.display()))?;
  Ok(html.len())
}

#[cfg(test)]
mod test {
  use super::*;

  fn page(body: &str) -> Page {
    Page {
      title: "t".into(),
      head: vec![Include::Css {
        path: "a/style.css".into(),
        id: Some("theme"),
      }],
      body: body.into(),
      tail: vec![
        Include::Js("a/app.js".into()),
        Include::Embed("a/embed.js".into()),
      ],
    }
  }

  fn site(body: &str) -> Site {
    let mut site = Site::new(page(body));
    site.add(
      "a/style.css",
      "@import url(./fonts/f.css);\nbody { background: url('img/bg.png'); }\n\
       .x { background: url(https://example.com/x.png); }\n",
    );
    site.add(
      "a/fonts/f.css",
      "@font-face { src: url(\"./f.woff\") format('woff'); }",
    );
    site.add("a/fonts/f.woff", vec![0u8, 1, 2]);
    site.add("a/img/bg.png", vec![9u8]);
    site.add("a/app.js", "if (a </script> b) {} // <!-- x");
    site.add("a/embed.js", "var s = '</SCRIPT>';");
    site.add("img/x.png", vec![1u8, 2, 3]);
    site
  }

  #[test]
  fn base64_matches_the_rfc_vectors() {
    for (raw, enc) in [
      ("", ""),
      ("f", "Zg=="),
      ("fo", "Zm8="),
      ("foo", "Zm9v"),
      ("foob", "Zm9vYg=="),
      ("fooba", "Zm9vYmE="),
      ("foobar", "Zm9vYmFy"),
    ] {
      assert_eq!(base64(raw.as_bytes()), enc);
    }
  }

  #[test]
  fn only_relative_references_count() {
    for url in ["img/x.png", "./x", "../x", "x.png?v=1", "/x.png"] {
      assert!(is_relative(url), "{url}");
    }
    for url in [
      "https://a/b",
      "data:image/png;base64,AA",
      "#frag",
      "//cdn/x",
      "mailto:a@b",
      "",
    ] {
      assert!(!is_relative(url), "{url}");
    }
  }

  #[test]
  fn resolving_normalizes_and_refuses_to_leave_the_deck() {
    assert_eq!(resolve("a/b", "../c/./d.png?x#y").unwrap(), "a/c/d.png");
    assert_eq!(resolve("", "my%20pic.png").unwrap(), "my pic.png");
    assert!(resolve("", "../x").unwrap_err().contains("outside"));
    assert!(resolve("", "/x").unwrap_err().contains("root"));
    assert_eq!(resolve("", "./").unwrap(), "index.html");
    assert_eq!(resolve("", "./#/3").unwrap(), "index.html");
    assert_eq!(resolve("a", "..").unwrap(), "index.html");
    assert_eq!(resolve("", "img/").unwrap(), "img/index.html");
  }

  #[test]
  fn finds_references_in_markup_but_not_in_code_or_scripts() {
    let html = r##"<section data-background-image="img/bg.png"><img src="img/x.png" alt="a > b">
<pre class="code">&lt;img src=&quot;no.png&quot;&gt;</pre>
<!-- <img src="comment.png"> -->
<script>var s = '<img src="script.png">';</script>
<a href="notes.pdf">notes</a> <a href="https://x/y">web</a> <a href="#/3">jump</a>
<div style="background: url('img/s.png')"></div>
<style>.k { background: url(img/k.png) }</style></section>"##;
    let urls: Vec<_> = html_refs(html)
      .into_iter()
      .map(|r| (r.url, r.link, r.css))
      .collect();
    assert_eq!(urls, [
      ("img/bg.png".to_string(), false, false),
      ("img/x.png".to_string(), false, false),
      ("notes.pdf".to_string(), true, false),
      ("img/s.png".to_string(), false, true),
      ("img/k.png".to_string(), false, true),
    ]);
  }

  #[test]
  fn finds_imports_and_urls_in_css() {
    let css = "@import url(a.css);\n@import 'b.css';\n/* url(c.png) */\n\
               x { y: url( \"d e.png\" ) url(data:x) url(e.png) }";
    let refs: Vec<_> = css_refs(css)
      .into_iter()
      .map(|r| (r.url, r.import, css[r.range].to_string()))
      .collect();
    assert_eq!(refs, [
      ("a.css".into(), true, "url(a.css)".into()),
      ("b.css".into(), true, "'b.css'".into()),
      ("d e.png".into(), false, "url( \"d e.png\" )".into()),
      ("e.png".into(), false, "url(e.png)".into()),
    ]);
  }

  #[test]
  fn names_the_slide_a_reference_is_on() {
    let body = "<section>one</section>\n<section>\n<section>two</section>\n\
                <section><img src=\"x\"></section>\n</section>";
    let at = body.find("<img").unwrap();
    assert_eq!(slide_of(body, at), 3);
  }

  #[test]
  fn broken_references_are_reported_by_slide_and_stylesheet() {
    let mut s = site(
      "<section>a</section><section><img src=\"img/missing.png\"></section>",
    );
    s.add("a/fonts/f.css", "@font-face { src: url(gone.woff); }");
    let broken = s.broken_refs();
    assert_eq!(broken.len(), 2, "{broken:?}");
    assert!(
      broken[0].starts_with("slide 2: `img/missing.png`"),
      "{broken:?}"
    );
    assert!(
      broken[1].starts_with("a/fonts/f.css: `gone.woff`"),
      "{broken:?}"
    );
    assert!(s.check_refs().is_err());
    assert!(site("<section><img src=\"img/x.png\"></section>")
      .check_refs()
      .is_ok());
  }

  #[test]
  fn a_single_file_refers_to_nothing_local() {
    let html = site(
      "<section><img src=\"img/x.png\"><div style=\"background: url(img/x.png)\"></div>\
       <style>.k { background: url('img/x.png') }</style></section>",
    )
    .render_single()
    .unwrap();
    // Everything relative became a data URI.
    assert!(
      html.contains("<img src=\"data:image/png;base64,AQID\">"),
      "{html}"
    );
    assert!(
      html.contains("url(&quot;data:image/png;base64,AQID&quot;)"),
      "{html}"
    );
    assert!(
      html.contains("url(\"data:image/png;base64,CQ==\")"),
      "{html}"
    );
    assert!(
      html.contains(".k { background: url(\"data:image/png;base64,AQID\") }"),
      "{html}"
    );
    // The import was put in its place, ahead of the rest of the stylesheet,
    // and its font inlined; not a stylesheet base64'd into a stylesheet.
    let font = html
      .find("url(\"data:font/woff;base64,AAEC\")")
      .expect("font inlined");
    assert!(font < html.find("data:image/png;base64,CQ==").unwrap());
    assert!(!html.contains("@import"), "{html}");
    assert!(!html.contains("data:text/css"), "{html}");
    // Remote references are left alone.
    assert!(html.contains("url(https://example.com/x.png)"), "{html}");
    assert_eq!(html_refs(&html), [], "left a local reference");
    let text = unescape_html(&html);
    assert!(css_refs(&text).is_empty(), "{:?}", css_refs(&text));
    // Scripts cannot close their own tag, in any case.
    assert!(html.contains(r"a <\/script> b"), "{html}");
    assert!(html.contains(r"// <\!-- x"), "{html}");
    assert!(html.contains(r"'<\/SCRIPT>'"), "{html}");
    // The theme keeps its id, the embed bundle is inert.
    assert!(html.contains("<style id=\"theme\">"), "{html}");
    assert!(
      html.contains(
        "<script type=\"text/x-aquascope-embed\" id=\"aquascope-embed\">"
      ),
      "{html}"
    );
  }

  #[test]
  fn a_link_to_the_deck_itself_is_not_broken() {
    let s =
      site("<section><a href=\"./\">a</a><a href=\"./#/2\">b</a></section>");
    assert!(s.check_refs().is_ok(), "{:?}", s.broken_refs());
    let html = s.render_single().unwrap();
    assert!(html.contains("<a href=\"\">a</a>"), "{html}");
    assert!(html.contains("<a href=\"#/2\">b</a>"), "{html}");
    assert!(site("<section><a href=\"img/\">c</a></section>")
      .check_refs()
      .is_err());
  }

  #[test]
  fn a_single_file_cannot_carry_a_link_to_a_file() {
    let mut s = site("<section><a href=\"img/x.png\">x</a></section>");
    s.add("img/x.png", vec![1u8]);
    let err = s.render_single().unwrap_err().to_string();
    assert!(err.contains("--dist dir"), "{err}");
  }

  #[test]
  fn the_runtime_assets_are_mapped_for_the_page_to_swap_in() {
    let mut s = site("<section></section>");
    s.add("img/ferris/does_not_compile.svg", "<svg/>");
    let html = s.render_single().unwrap();
    assert!(
      html.contains(
        "window.AQUASCOPE_ASSETS = {\"img/ferris/does_not_compile.svg\":\"data:image/svg+xml;base64,PHN2Zy8+\"};"
      ),
      "{html}"
    );
  }

  fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
      "aquascope-reveal-site-{name}-{}",
      std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
  }

  #[test]
  fn dist_dir_replaces_only_what_it_wrote() {
    let root = scratch("dist");
    let out = root.join("dist");
    let s = site("<section></section>");

    dist_dir(&s, &out).unwrap();
    assert!(out.join(MARKER).exists());
    assert!(out.join("index.html").exists());

    // A leftover is gone after the next build: the directory is replaced.
    fs::write(out.join("stale.txt"), "x").unwrap();
    dist_dir(&s, &out).unwrap();
    assert!(!out.join("stale.txt").exists());
    assert!(out.join("a/fonts/f.woff").exists());

    // A directory it did not write is left alone.
    let theirs = root.join("theirs");
    fs::create_dir_all(&theirs).unwrap();
    fs::write(theirs.join("keep.txt"), "x").unwrap();
    let err = dist_dir(&s, &theirs).unwrap_err().to_string();
    assert!(err.contains("not empty"), "{err}");
    assert!(theirs.join("keep.txt").exists());

    // Nothing is left beside it, and a failing build writes nothing.
    let bad = site("<section><img src=\"nope.png\"></section>");
    assert!(dist_dir(&bad, &root.join("bad")).is_err());
    let mut left: Vec<_> = fs::read_dir(&root)
      .unwrap()
      .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
      .collect();
    left.sort();
    assert_eq!(left, ["dist", "theirs"]);
    fs::remove_dir_all(root).unwrap();
  }

  #[test]
  fn dist_single_writes_one_file() {
    let root = scratch("single");
    let file = root.join("deck.html");
    dist_single(&site("<section></section>"), &file).unwrap();
    assert_eq!(
      fs::read_dir(&root).unwrap().count(),
      1,
      "only the file itself"
    );
    assert!(dist_single(&site("<section></section>"), &root).is_err());
    fs::remove_dir_all(root).unwrap();
  }
}
