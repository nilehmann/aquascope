use std::{path::PathBuf, process::Command};

use anyhow::{Context, Result, bail};

const TOOLCHAIN_TOML: &str = include_str!("../rust-toolchain.toml");

pub fn run_and_get_output(cmd: &mut Command) -> Result<String> {
  let output = cmd
    .output()
    .with_context(|| format!("Failed to run `{}`", display_command(cmd)))?;
  if !output.status.success() {
    bail!(
      "`{}` failed ({}) with stderr:\n{}",
      display_command(cmd),
      output.status,
      String::from_utf8_lossy(&output.stderr).trim_end()
    );
  }
  let stdout = String::from_utf8(output.stdout)?;
  Ok(stdout.trim_end().to_string())
}

fn display_command(cmd: &Command) -> String {
  std::iter::once(cmd.get_program())
    .chain(cmd.get_args())
    .map(|arg| arg.to_string_lossy())
    .collect::<Vec<_>>()
    .join(" ")
}

pub fn rustc() -> Result<PathBuf> {
  if let Ok(rustc) = std::env::var("RUSTC_PATH") {
    return Ok(PathBuf::from(rustc));
  }

  if let Ok(toolchain) = toolchain() {
    let output = run_and_get_output(Command::new("rustup").args([
      "which",
      "--toolchain",
      &toolchain,
      "rustc",
    ]))?;
    Ok(PathBuf::from(output))
  } else {
    let output = run_and_get_output(Command::new("which").arg("rustc"))?;
    Ok(PathBuf::from(output))
  }
}

pub fn toolchain() -> Result<String> {
  let config: toml::Value = toml::from_str(TOOLCHAIN_TOML)?;
  Ok(
    config
      .get("toolchain")
      .context("Missing toolchain key")?
      .get("channel")
      .context("Missing channel key")?
      .as_str()
      .unwrap()
      .to_string(),
  )
}

pub fn miri_sysroot() -> Result<PathBuf> {
  if let Ok(sysroot) = std::env::var("MIRI_SYSROOT") {
    return Ok(sysroot.into());
  }

  let toolchain = toolchain().ok();
  let mut cmd = Command::new("cargo");
  if let Some(toolchain) = &toolchain {
    cmd.arg(format!("+{}", toolchain));
  }
  cmd.args(["miri", "setup", "--print-sysroot"]);

  let stdout = run_and_get_output(&mut cmd).with_context(|| {
    let mut msg = String::from("Could not locate the Miri sysroot.");
    if let Some(toolchain) = &toolchain {
      msg.push_str(&format!(
        " Aquascope needs the {toolchain} toolchain with Miri installed. \
         If it is missing or broken, reinstall it with:\n\n  \
         rustup toolchain install {toolchain} --force --component {}\n",
        toolchain_components().join(" --component ")
      ));
    }
    msg.push_str("\nAlternatively, set MIRI_SYSROOT to an existing sysroot.");
    msg
  })?;
  Ok(PathBuf::from(stdout))
}

fn toolchain_components() -> Vec<String> {
  let Ok(config) = toml::from_str::<toml::Value>(TOOLCHAIN_TOML) else {
    return vec!["miri".into()];
  };
  config
    .get("toolchain")
    .and_then(|t| t.get("components"))
    .and_then(|c| c.as_array())
    .map(|c| {
      c.iter()
        .filter_map(|v| v.as_str().map(String::from))
        .collect()
    })
    .unwrap_or_else(|| vec!["miri".into()])
}
