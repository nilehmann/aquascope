use std::{fs, path::Path};

use anyhow::{ensure, Context, Result};

/// Built by `depot build` in the frontend workspace.
const SRC_DIR: &str = "../../frontend/packages/aquascope-embed/dist/";
const DST_DIR: &str = "./js";

/// The subset of the embed bundle we actually link into a deck. The source map
/// is deliberately excluded -- it is large and only useful when debugging the
/// frontend itself.
const ASSETS: [&str; 2] = ["embed.iife.js", "style.css"];

fn main() -> Result<()> {
  let src = Path::new(SRC_DIR);
  let dst = Path::new(DST_DIR);
  fs::create_dir_all(dst)?;

  for asset in ASSETS {
    let from = src.join(asset);
    ensure!(
      from.exists(),
      "missing {}. Run `depot build` in ./frontend first.",
      from.display()
    );
    fs::copy(&from, dst.join(asset))
      .with_context(|| format!("copying {}", from.display()))?;
    println!("cargo:rerun-if-changed={}", from.display());
  }

  Ok(())
}
