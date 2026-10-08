//! External crates a deck's blocks may use: the `dependencies` key of the
//! front matter.
//!
//! ```markdown
//! ---
//! dependencies:
//!   trpl: "0.2"
//!   tokio: { version: "1", features: [full] }
//! ---
//! ```
//!
//! The value of each key is what Cargo.toml would say, in YAML. They are built
//! once, by cargo, into a crate of their own in the user's cache directory,
//! and every later `rustc` -- the build-time check of each block and the Run
//! button under `--serve` -- is pointed at what that build produced with
//! `--extern` and `-L dependency=`. That leaves the blocks themselves compiled
//! exactly as before, by a bare `rustc` on one file, and keeps the cost to one
//! cargo invocation per build, which is a no-op once the crates are built.
//!
//! The crate's directory is named by a hash of its manifest and of
//! `rustc -vV`, because an rlib only loads into the compiler that built it: a
//! toolchain update gets a fresh build rather than `E0514` on every block.

use std::{
  collections::{hash_map::DefaultHasher, HashMap},
  env,
  ffi::OsString,
  fs,
  hash::{Hash, Hasher},
  path::{Path, PathBuf},
  process::{Command, Stdio},
  sync::{Arc, RwLock},
};

use anyhow::{bail, Context, Result};
use serde_json::{Map, Value};

/// A deck's dependencies, built, as the flags that let rustc use them. The
/// default is a deck with none, which adds no flags.
#[derive(Default, Debug, Clone)]
pub struct Deps {
  /// One `--extern name=path` per direct dependency, under the name a block
  /// uses for it.
  externs: Vec<(String, PathBuf)>,
  /// Where their own dependencies were built, for `-L dependency=`.
  search: Option<PathBuf>,
  /// The `[dependencies]` lines and the crate they were built in, for
  /// Aquascope, which compiles each block as a cargo project of its own.
  aquascope: Option<mdbook_aquascope::Dependencies>,
}

/// The deck's current dependencies, shared with the server: a rebuild under
/// `--watch` may change them while the Run endpoint is answering.
pub type Shared = Arc<RwLock<Deps>>;

impl Deps {
  /// The arguments to add to a `rustc` command line.
  pub fn rustc_args(&self) -> Vec<OsString> {
    let mut args = Vec::new();
    if let Some(search) = &self.search {
      let mut arg = OsString::from("dependency=");
      arg.push(search);
      args.push("-L".into());
      args.push(arg);
    }
    for (name, path) in &self.externs {
      let mut arg = OsString::from(format!("{name}="));
      arg.push(path);
      args.push("--extern".into());
      args.push(arg);
    }
    args
  }

  pub fn is_empty(&self) -> bool {
    self.externs.is_empty()
  }

  /// The same crates, as Aquascope's block projects declare them.
  pub fn aquascope(&self) -> Option<mdbook_aquascope::Dependencies> {
    self.aquascope.clone()
  }
}

/// Builds `dependencies` and returns the flags for them. A relative `path`
/// dependency is relative to `base`, the deck's directory, as it would be to a
/// Cargo.toml sitting next to the deck.
pub fn build(dependencies: &Map<String, Value>, base: &Path) -> Result<Deps> {
  build_under(&cache_root(), dependencies, base)
}

/// [`build`], with the crate under `cache` rather than the user's cache
/// directory.
fn build_under(
  cache: &Path,
  dependencies: &Map<String, Value>,
  base: &Path,
) -> Result<Deps> {
  if dependencies.is_empty() {
    return Ok(Deps::default());
  }

  let table = table(dependencies, base)?;
  let manifest = format!("{MANIFEST_HEAD}{table}");
  let dir = crate_dir(cache, &manifest)?;
  fs::create_dir_all(&dir)
    .with_context(|| format!("creating {}", dir.display()))?;
  // Written once: the directory is named by the manifest, and rewriting
  // either file would make cargo rebuild the crate on every build.
  for (file, contents) in [("Cargo.toml", manifest.as_str()), ("lib.rs", "")] {
    let path = dir.join(file);
    if !path.exists() {
      fs::write(&path, contents)?;
    }
  }

  // cargo's progress and diagnostics go straight to the terminal: the first
  // build of a crate like tokio takes a while, and is worth seeing happen.
  // Only the artifact messages on stdout are read.
  let output = Command::new("cargo")
    .args(["build", "--message-format=json-render-diagnostics"])
    .current_dir(&dir)
    .stderr(Stdio::inherit())
    .output()
    .context("could not run cargo, which builds the deck's dependencies")?;
  if !output.status.success() {
    bail!(
      "cargo could not build the deck's dependencies (its output is above). \
       The crate is in {}",
      dir.display()
    );
  }
  let libs = libraries(&String::from_utf8_lossy(&output.stdout));

  // Not `--offline`: metadata resolves the dependencies of every platform,
  // and the first time may have to download some -- a wasm-only crate, say
  // -- that the build for this one never needed.
  let metadata = Command::new("cargo")
    .args(["metadata", "--format-version", "1"])
    .current_dir(&dir)
    .output()
    .context("could not run cargo metadata")?;
  if !metadata.status.success() {
    bail!(
      "cargo metadata failed for the deck's dependencies:\n{}",
      String::from_utf8_lossy(&metadata.stderr)
    );
  }
  let metadata: Value = serde_json::from_slice(&metadata.stdout)
    .context("reading cargo metadata's output")?;

  let mut externs = Vec::new();
  for (name, package) in direct_dependencies(&metadata)? {
    let Some(path) = libs.get(&package) else {
      bail!("cargo built no library for the dependency `{name}`");
    };
    externs.push((name, path.clone()));
  }

  Ok(Deps {
    externs,
    search: Some(dir.join("target").join("debug").join("deps")),
    // Aquascope builds with its own nightly, so it gets a target directory
    // of its own rather than churning this one.
    aquascope: Some(mdbook_aquascope::Dependencies {
      table,
      lockfile: Some(dir.join("Cargo.lock")),
      target_dir: dir.join("target-aquascope"),
    }),
  })
}

/// The Cargo.toml of the crate the dependencies are built in, up to its
/// `[dependencies]` table.
///
/// An empty `[workspace]` keeps cargo from looking for one above the cache
/// directory, and the library is an empty file: the crate exists only to
/// have dependencies.
const MANIFEST_HEAD: &str = "[package]
name = \"aquascope-reveal-deps\"
version = \"0.0.0\"
edition = \"2021\"
publish = false

[lib]
path = \"lib.rs\"

[workspace]

[dependencies]
";

/// The lines of the `[dependencies]` table, one per dependency.
fn table(dependencies: &Map<String, Value>, base: &Path) -> Result<String> {
  let mut out = String::new();
  for (name, spec) in dependencies {
    let spec = absolute_path(spec, base);
    let value = toml(&spec)
      .with_context(|| format!("in the front matter's dependency `{name}`"))?;
    out.push_str(&format!("{} = {value}\n", toml_string(name)));
  }
  Ok(out)
}

/// `spec` with a relative `path` made absolute against `base`, since the
/// manifest is written somewhere else entirely.
fn absolute_path(spec: &Value, base: &Path) -> Value {
  let mut spec = spec.clone();
  if let Some(Value::String(path)) =
    spec.as_object_mut().and_then(|table| table.get_mut("path"))
  {
    if Path::new(path.as_str()).is_relative() {
      *path = base.join(&*path).to_string_lossy().into_owned();
    }
  }
  spec
}

/// A YAML value, as parsed, written as an inline TOML value.
fn toml(value: &Value) -> Result<String> {
  Ok(match value {
    Value::String(s) => toml_string(s),
    Value::Bool(b) => b.to_string(),
    Value::Number(n) => n.to_string(),
    Value::Array(items) => {
      let items: Result<Vec<_>> = items.iter().map(toml).collect();
      format!("[{}]", items?.join(", "))
    }
    Value::Object(table) => {
      let entries: Result<Vec<_>> = table
        .iter()
        .map(|(k, v)| Ok(format!("{} = {}", toml_string(k), toml(v)?)))
        .collect();
      format!("{{ {} }}", entries?.join(", "))
    }
    Value::Null => bail!("TOML has no null"),
  })
}

/// A TOML basic string. JSON's escapes are a subset of TOML's, so serde_json
/// does the quoting.
fn toml_string(s: &str) -> String {
  Value::String(s.to_string()).to_string()
}

/// Where the crate for `manifest` lives: one directory per manifest and
/// toolchain, under `cache`.
fn crate_dir(cache: &Path, manifest: &str) -> Result<PathBuf> {
  let rustc = Command::new("rustc").arg("-vV").output().context(
    "could not run rustc, which the deck's blocks are compiled with",
  )?;
  let mut hasher = DefaultHasher::new();
  manifest.hash(&mut hasher);
  rustc.stdout.hash(&mut hasher);
  Ok(
    cache
      .join("aquascope-reveal")
      .join("deps")
      .join(format!("{:016x}", hasher.finish())),
  )
}

fn cache_root() -> PathBuf {
  env::var_os("XDG_CACHE_HOME")
    .filter(|dir| !dir.is_empty())
    .map(PathBuf::from)
    .or_else(|| {
      env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache"))
    })
    .unwrap_or_else(env::temp_dir)
}

/// The library each package was built as, from cargo's JSON messages: an
/// rlib, or the shared object of a proc macro. Keyed by package id.
fn libraries(messages: &str) -> HashMap<String, PathBuf> {
  let mut libs = HashMap::new();
  for line in messages.lines() {
    let Ok(message) = serde_json::from_str::<Value>(line) else {
      continue;
    };
    if message["reason"] != "compiler-artifact" {
      continue;
    }
    let Some(id) = message["package_id"].as_str() else {
      continue;
    };
    let kinds = message["target"]["kind"].as_array();
    let is_lib = kinds.is_some_and(|kinds| {
      kinds.iter().any(|kind| {
        matches!(kind.as_str(), Some("lib" | "rlib" | "dylib" | "proc-macro"))
      })
    });
    if !is_lib {
      continue;
    }
    let file = message["filenames"].as_array().and_then(|files| {
      files.iter().filter_map(Value::as_str).find(|file| {
        [".rlib", ".so", ".dylib", ".dll"]
          .iter()
          .any(|ext| file.ends_with(ext))
      })
    });
    if let Some(file) = file {
      libs.insert(id.to_string(), PathBuf::from(file));
    }
  }
  libs
}

/// The root package's normal dependencies, as (the name code uses for it,
/// package id). The name is cargo's own answer, so a renamed dependency or a
/// library named differently from its package comes out right.
fn direct_dependencies(metadata: &Value) -> Result<Vec<(String, String)>> {
  let resolve = &metadata["resolve"];
  let Some(root) = resolve["root"].as_str() else {
    bail!("cargo metadata named no root package");
  };
  let node = resolve["nodes"]
    .as_array()
    .into_iter()
    .flatten()
    .find(|node| node["id"] == root)
    .context("cargo metadata has no entry for the root package")?;

  let mut deps = Vec::new();
  for dep in node["deps"].as_array().into_iter().flatten() {
    let normal = dep["dep_kinds"]
      .as_array()
      .into_iter()
      .flatten()
      .any(|kind| kind["kind"].is_null());
    if let (true, Some(name), Some(package)) =
      (normal, dep["name"].as_str(), dep["pkg"].as_str())
    {
      deps.push((name.to_string(), package.to_string()));
    }
  }
  Ok(deps)
}

#[cfg(test)]
mod test {
  use super::*;

  fn yaml(s: &str) -> Map<String, Value> {
    serde_yaml::from_str(s).unwrap()
  }

  #[test]
  fn writes_cargo_toml_from_yaml() {
    let deps = yaml(
      "trpl: \"0.2\"\n\
       tokio: { version: \"1\", features: [full], default-features: false }\n",
    );
    let manifest = table(&deps, Path::new("/deck")).unwrap();
    assert!(manifest.contains("\"trpl\" = \"0.2\"\n"), "{manifest}");
    assert!(
      // serde_json sorts a table's keys, which TOML does not mind.
      manifest.contains(
        "\"tokio\" = { \"default-features\" = false, \"features\" = \
         [\"full\"], \"version\" = \"1\" }\n"
      ),
      "{manifest}"
    );
  }

  #[test]
  fn relative_paths_are_relative_to_the_deck() {
    let deps = yaml("greet: { path: ../greet }\nabs: { path: /x/abs }\n");
    let manifest = table(&deps, Path::new("/deck")).unwrap();
    assert!(
      manifest.contains("\"path\" = \"/deck/../greet\""),
      "{manifest}"
    );
    assert!(manifest.contains("\"path\" = \"/x/abs\""), "{manifest}");
  }

  #[test]
  fn rejects_null() {
    assert!(table(&yaml("trpl: ~\n"), Path::new("/")).is_err());
  }

  #[test]
  fn no_dependencies_add_no_flags() {
    let deps = build(&Map::new(), Path::new("/")).unwrap();
    assert!(deps.is_empty());
    assert!(deps.rustc_args().is_empty());
  }

  /// A local crate, so the end-to-end test needs no network: built, then used
  /// by a block through the build-time check and through Run.
  #[test]
  fn a_block_can_use_a_dependency() {
    let root = env::temp_dir()
      .join(format!("aquascope-reveal-deps-test-{}", std::process::id()));
    let greet = root.join("greet");
    fs::create_dir_all(greet.join("src")).unwrap();
    fs::write(
      greet.join("Cargo.toml"),
      "[package]\nname = \"greet-lib\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\
       [lib]\nname = \"greet\"\n",
    )
    .unwrap();
    fs::write(
      greet.join("src/lib.rs"),
      "pub fn hi() -> &'static str { \"hi from a dependency\" }\n",
    )
    .unwrap();

    // A cache of its own, so the test neither reads nor fills the user's.
    // The path is relative to `root`, as a deck's is to its directory.
    let deps = build_under(
      &root.join("cache"),
      &yaml("greet-lib: { path: greet }\n"),
      &root,
    )
    .unwrap();

    // The name code uses is the library's, not the package's key.
    assert_eq!(deps.externs.len(), 1);
    assert_eq!(deps.externs[0].0, "greet");

    let program = "fn main() { println!(\"{}\", greet::hi()); }";
    assert_eq!(crate::run::check(program, &deps), Ok(()));
    assert!(crate::run::check(program, &Deps::default()).is_err());

    let body = serde_json::json!({ "code": program }).to_string();
    let response = crate::run::evaluate(body.as_bytes(), &deps);
    let json: Value = serde_json::from_slice(&response).unwrap();
    assert_eq!(json["result"], "hi from a dependency\n");

    let _ = fs::remove_dir_all(&root);
  }
}
