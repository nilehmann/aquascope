//! Warnings for unrecognised Aquascope fence specifiers.
//!
//! A misspelt specifier is silently ignored: `souldFail` simply does not match
//! `shouldFail`, so the block renders without the marker and nothing says why.
//! These checks turn that into a warning naming the line.

/// The operations that may appear before the first comma, `+`-separated.
const OPERATIONS: [&str; 2] = ["interpreter", "permissions"];

/// Config keys read by the preprocessor or the editor. Config is passed through
/// generically, so this list is the only thing standing between a typo and
/// silence -- keep it in step with `CommonConfig` in `aquascope-editor`.
const CONFIG: [&str; 10] = [
  "boundaries",
  "concreteTypes",
  "hideCode",
  "horizontal",
  "interpreterControls",
  "run",
  "shouldFail",
  "showFlows",
  "stepper",
  "stepperControls",
];

/// Levenshtein distance, used only to suggest a correction.
fn distance(a: &str, b: &str) -> usize {
  let b_chars: Vec<char> = b.chars().collect();
  let mut prev: Vec<usize> = (0 ..= b_chars.len()).collect();
  let mut cur = vec![0; b_chars.len() + 1];

  for (i, ca) in a.chars().enumerate() {
    cur[0] = i + 1;
    for (j, cb) in b_chars.iter().enumerate() {
      let cost = usize::from(ca != *cb);
      cur[j + 1] = (prev[j] + cost).min(prev[j + 1] + 1).min(cur[j] + 1);
    }
    std::mem::swap(&mut prev, &mut cur);
  }

  prev[b_chars.len()]
}

/// The closest known name, when one is near enough to be worth suggesting.
fn suggest(word: &str, known: &[&str]) -> Option<String> {
  let lower = word.to_lowercase();
  known
    .iter()
    // Compare case-insensitively so `shouldfail` suggests `shouldFail`.
    .map(|k| (distance(&lower, &k.to_lowercase()), *k))
    .filter(|(d, _)| *d <= 3 && *d < word.len())
    .min_by_key(|(d, _)| *d)
    .map(|(_, k)| k.to_string())
}

/// Checks every ```aquascope fence in `markdown`, returning one warning per
/// problem. Line numbers are 1-based and refer to `markdown` as given, so pass
/// the file as it is on disk.
pub fn check(markdown: &str) -> Vec<String> {
  let mut warnings = Vec::new();
  let mut in_block = false;

  for (i, line) in markdown.lines().enumerate() {
    let trimmed = line.trim_start();
    if !trimmed.starts_with("```") {
      continue;
    }
    // Only the opening fence carries specifiers.
    if in_block {
      in_block = false;
      continue;
    }
    in_block = true;

    let Some(spec) = trimmed.strip_prefix("```aquascope") else {
      continue;
    };
    let line_no = i + 1;

    let Some(spec) = spec.strip_prefix(',') else {
      if spec.trim().is_empty() {
        warnings.push(format!(
          "{line_no}: ```aquascope has no operation, so the block is left \
           as plain code. Write ```aquascope,interpreter or \
           ```aquascope,permissions"
        ));
      }
      continue;
    };

    let mut parts = spec.split(',');

    // The first field holds the operations, joined by `+`.
    if let Some(ops) = parts.next() {
      for op in ops.split('+') {
        let op = op.trim();
        if !op.is_empty() && !OPERATIONS.contains(&op) {
          warnings.push(match suggest(op, &OPERATIONS) {
            Some(s) => format!(
              "{line_no}: unknown Aquascope operation `{op}`, did you mean `{s}`?"
            ),
            None => format!(
              "{line_no}: unknown Aquascope operation `{op}`, expected one of {}",
              OPERATIONS.join(", ")
            ),
          });
        }
      }
    }

    for entry in parts {
      // Config may be `key` or `key=value`.
      let key = entry.split('=').next().unwrap_or(entry).trim();
      if key.is_empty() || CONFIG.contains(&key) {
        continue;
      }
      warnings.push(match suggest(key, &CONFIG) {
        Some(s) => format!(
          "{line_no}: unknown Aquascope specifier `{key}`, did you mean `{s}`?"
        ),
        None => format!("{line_no}: unknown Aquascope specifier `{key}`"),
      });
    }
  }

  warnings
}

#[cfg(test)]
mod test {
  use super::check;

  #[test]
  fn accepts_known_specifiers() {
    let w = check(
      "```aquascope,interpreter,horizontal,run,shouldFail\nfn main() {}\n```\n",
    );
    assert!(w.is_empty(), "{w:?}");
  }

  #[test]
  fn suggests_a_correction() {
    let w = check("```aquascope,interpreter,souldFail\nfn main() {}\n```\n");
    assert_eq!(w.len(), 1, "{w:?}");
    assert!(w[0].contains("`souldFail`"), "{}", w[0]);
    assert!(w[0].contains("did you mean `shouldFail`"), "{}", w[0]);
    assert!(w[0].starts_with("1:"), "{}", w[0]);
  }

  #[test]
  fn checks_operations_and_key_value_config() {
    let w = check("```aquascope,interperter,horizontal=false\nx\n```\n");
    assert_eq!(w.len(), 1, "{w:?}");
    assert!(w[0].contains("did you mean `interpreter`"), "{}", w[0]);
  }

  #[test]
  fn accepts_combined_operations() {
    assert!(check("```aquascope,permissions+interpreter\nx\n```\n").is_empty());
  }

  #[test]
  fn flags_a_bare_fence_and_ignores_other_languages() {
    let w = check("```aquascope\nx\n```\n\n```rust,ignore\ny\n```\n");
    assert_eq!(w.len(), 1, "{w:?}");
    assert!(w[0].contains("no operation"), "{}", w[0]);
  }

  #[test]
  fn ignores_specifiers_inside_a_block() {
    // A closing fence, and anything that looks like one inside code, must not
    // be read as an opening fence.
    let w = check("```aquascope,interpreter\nlet s = \"```aquascope,nope\";\n```\n");
    assert!(w.is_empty(), "{w:?}");
  }
}
