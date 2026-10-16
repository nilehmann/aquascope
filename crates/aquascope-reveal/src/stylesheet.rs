//! Slimming a stylesheet before `--dist single` inlines it.
//!
//! A browser loads one source from each `@font-face`, the first whose format
//! it reads, but inlining cannot know which and would carry them all: Font
//! Awesome's five formats are 1.3 MB, of which the browser uses its 77 KB
//! woff2. So each `@font-face` is cut down to the one source every browser a
//! deck runs in reads. And an `@import` would be inlined as a `data:` URI
//! inside a `data:` URI, base64 of base64; a plain one is handed back to be
//! put in place as text instead.

use lightningcss::{
  printer::PrinterOptions,
  rules::{
    font_face::{
      FontFaceProperty, FontFaceRule, FontFormat, Source, UrlSource,
    },
    CssRule, CssRuleList,
  },
  stylesheet::{ParserOptions, StyleSheet},
};

pub struct Slim {
  /// What the stylesheet `@import`s, in order, to be put before `css`.
  pub imports: Vec<String>,
  /// The stylesheet without those imports, minified.
  pub css: String,
}

/// `css` slimmed, or as it is if lightningcss cannot parse it: a stylesheet
/// that is merely larger than it could be is no reason to fail a build.
pub fn slim(css: &str) -> Slim {
  try_slim(css).unwrap_or_else(|| Slim {
    imports: Vec::new(),
    css: css.to_string(),
  })
}

fn try_slim(css: &str) -> Option<Slim> {
  let mut sheet = StyleSheet::parse(css, ParserOptions::default()).ok()?;
  let rules = &mut sheet.rules.0;

  // Only when every import is unconditional: one kept would have to stay
  // first, ahead of the text that replaces the others.
  let imports: Vec<_> = rules
    .iter()
    .filter_map(|rule| match rule {
      CssRule::Import(import) => Some(import),
      _ => None,
    })
    .collect();
  let plain = imports.iter().all(|import| {
    import.layer.is_none()
      && import.supports.is_none()
      && import.media.always_matches()
  });
  let imports = if plain {
    let urls = imports
      .iter()
      .map(|import| import.url.to_string())
      .collect();
    rules.retain(|rule| !matches!(rule, CssRule::Import(_)));
    urls
  } else {
    Vec::new()
  };

  one_source_each(&mut sheet.rules);
  let css = sheet
    .to_css(PrinterOptions {
      minify: true,
      ..PrinterOptions::default()
    })
    .ok()?
    .code;
  Some(Slim { imports, css })
}

fn one_source_each(rules: &mut CssRuleList) {
  for rule in &mut rules.0 {
    match rule {
      CssRule::FontFace(face) => one_source(face),
      CssRule::Media(rule) => one_source_each(&mut rule.rules),
      CssRule::Supports(rule) => one_source_each(&mut rule.rules),
      CssRule::LayerBlock(rule) => one_source_each(&mut rule.rules),
      _ => {}
    }
  }
}

/// Leaves `face` with one `src`: the `local()`s of its last, which is the
/// one that counts, and the best of all its `url()`s -- best only if every
/// browser reads it, or it is left alone.
fn one_source(face: &mut FontFaceRule) {
  let is_src = |p: &FontFaceProperty| matches!(p, FontFaceProperty::Source(_));
  let best = face
    .properties
    .iter()
    .filter_map(|p| match p {
      FontFaceProperty::Source(sources) => Some(sources),
      _ => None,
    })
    .flatten()
    .filter_map(|source| match source {
      Source::Url(url) => rank(url).map(|rank| (rank, url)),
      Source::Local(_) => None,
    })
    .min_by_key(|(rank, _)| *rank)
    .map(|(_, url)| url.clone());
  let (Some(best), Some(last)) =
    (best, face.properties.iter().rposition(is_src))
  else {
    return;
  };
  let FontFaceProperty::Source(sources) = &face.properties[last] else {
    unreachable!()
  };
  let mut src: Vec<_> = sources
    .iter()
    .filter(|source| matches!(source, Source::Local(_)))
    .cloned()
    .collect();
  src.push(Source::Url(best));
  face.properties[last] = FontFaceProperty::Source(src);
  let mut i = 0;
  face.properties.retain(|p| {
    i += 1;
    i - 1 == last || !is_src(p)
  });
}

/// How good a source is, lower being better: `None` for a format some
/// browser does not read (eot, svg) or one not known.
fn rank(url: &UrlSource) -> Option<u8> {
  let format = match &url.format {
    Some(format) => format.clone(),
    None => {
      let path = &url.url.url;
      let path = &path[.. path.find(['?', '#']).unwrap_or(path.len())];
      match path.rsplit('.').next()?.to_ascii_lowercase().as_str() {
        "woff2" => FontFormat::WOFF2,
        "woff" => FontFormat::WOFF,
        "ttf" => FontFormat::TrueType,
        "otf" => FontFormat::OpenType,
        _ => return None,
      }
    }
  };
  match format {
    FontFormat::WOFF2 => Some(0),
    FontFormat::WOFF => Some(1),
    FontFormat::TrueType | FontFormat::OpenType => Some(2),
    _ => None,
  }
}

#[cfg(test)]
mod test {
  use super::*;

  #[test]
  fn a_font_keeps_its_best_source() {
    // Font Awesome 4.7's, eot first for Internet Explorer.
    let css = slim(
      "@font-face{font-family:'FontAwesome';\
       src:url('../fonts/f.eot?v=4.7.0');\
       src:url('../fonts/f.eot?#iefix&v=4.7.0') format('embedded-opentype'),\
       url('../fonts/f.woff2?v=4.7.0') format('woff2'),\
       url('../fonts/f.woff?v=4.7.0') format('woff'),\
       url('../fonts/f.svg?v=4.7.0#fa') format('svg');\
       font-weight:normal}",
    )
    .css;
    assert!(css.contains("f.woff2?v=4.7.0"), "{css}");
    for gone in ["f.eot", "f.woff?", "f.svg"] {
      assert!(!css.contains(gone), "{css}");
    }
    assert_eq!(css.matches("src:").count(), 1, "{css}");
    assert!(css.contains("font-weight:"), "{css}");
  }

  #[test]
  fn a_font_without_format_hints_is_ranked_by_extension() {
    let css = slim(
      "@media screen{@font-face{font-family:x;\
       src:local(X),url(a.ttf),url(a.woff)}}",
    )
    .css;
    assert!(css.contains("local(X)"), "{css}");
    assert!(css.contains("a.woff"), "{css}");
    assert!(!css.contains("a.ttf"), "{css}");
  }

  #[test]
  fn a_font_with_only_old_formats_is_left_alone() {
    let css = slim("@font-face{font-family:x;src:url(a.eot),url(a.svg)}").css;
    assert!(css.contains("a.eot") && css.contains("a.svg"), "{css}");
  }

  #[test]
  fn plain_imports_are_handed_back() {
    let s = slim("@import url(./fonts/a.css);@import 'b.css';.r{color:red}");
    assert_eq!(s.imports, ["./fonts/a.css", "b.css"]);
    assert!(!s.css.contains("@import"), "{}", s.css);
    assert!(s.css.contains(".r{color:red}"), "{}", s.css);

    // One with a condition keeps them all where they are.
    let s = slim("@import 'a.css';@import 'p.css' print;.r{color:red}");
    assert!(s.imports.is_empty());
    assert_eq!(s.css.matches("@import").count(), 2, "{}", s.css);
  }

  #[test]
  fn what_cannot_be_parsed_is_kept() {
    let css = "@font-face{src:url(a.eot)} .r{";
    assert!(slim(css).css.contains("a.eot"));
  }
}
