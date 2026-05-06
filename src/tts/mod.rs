use std::{
	fs,
	path::{Path, PathBuf},
	process::Stdio,
};

use color_eyre::eyre::{Result, bail, eyre};
use indicatif::{ProgressBar, ProgressStyle};
use tokio::{
	io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
	process::Command,
};

const KOKORO_SCRIPT: &str = include_str!("kokoro.py");
const CHATTERBOX_SCRIPT: &str = include_str!("chatterbox.py");
const OUT_EXT: &str = "wav";

#[derive(Clone, Copy, Debug)]
pub enum Model {
	/// Kokoro-82M. Fast, runs comfortably on CPU.
	Fast,
	/// Chatterbox. Larger and higher quality, wants a GPU.
	Best,
}

impl Model {
	fn script(self) -> &'static str {
		match self {
			Model::Fast => KOKORO_SCRIPT,
			Model::Best => CHATTERBOX_SCRIPT,
		}
	}

	fn label(self) -> &'static str {
		match self {
			Model::Fast => "kokoro",
			Model::Best => "chatterbox",
		}
	}
}

pub async fn run(input: &Path, output: &Path, model: Model) -> Result<()> {
	let in_ext = input.extension().and_then(|e| e.to_str()).unwrap_or("");
	if in_ext != "txt" && in_ext != "md" {
		bail!("input must be .txt or .md, got '{}'", input.display());
	}
	if !input.is_file() {
		bail!("input file not found: {}", input.display());
	}

	let out_path = resolve_output(input, output)?;
	if let Some(parent) = out_path.parent() {
		fs::create_dir_all(parent)?;
	}

	preflight_uv().await?;

	println!("tts ({}): {} -> {}", model.label(), input.display(), out_path.display());

	let mut child = Command::new("uv")
		.args(["run", "--no-project", "-"])
		.arg(input)
		.arg(&out_path)
		.stdin(Stdio::piped())
		.stdout(Stdio::piped())
		.stderr(Stdio::inherit())
		.spawn()
		.map_err(|e| eyre!("failed to spawn `uv run`: {e}"))?;

	let mut stdin = child.stdin.take().expect("stdin was piped");
	stdin.write_all(model.script().as_bytes()).await?;
	stdin.shutdown().await?;
	drop(stdin);

	let stdout = child.stdout.take().expect("stdout was piped");
	let mut reader = BufReader::new(stdout).lines();
	let bar = ProgressBar::new(0);
	bar.set_style(ProgressStyle::with_template("{bar:40.cyan/blue} {pos}/{len} chunks  elapsed {elapsed_precise}  eta {eta_precise}").expect("static template"));
	while let Some(line) = reader.next_line().await? {
		match parse_progress(&line) {
			Some((cur, total)) => {
				bar.set_length(total);
				bar.set_position(cur);
			}
			None => bar.println(line),
		}
	}
	bar.finish_and_clear();

	let status = child.wait().await?;
	if !status.success() {
		bail!("{} TTS script failed (exit {status})", model.label());
	}
	Ok(())
}

/// Parse a `PROGRESS <cur>/<total>` line emitted by the TTS python scripts.
fn parse_progress(line: &str) -> Option<(u64, u64)> {
	let rest = line.strip_prefix("PROGRESS ")?;
	let (cur, total) = rest.split_once('/')?;
	Some((cur.parse().ok()?, total.parse().ok()?))
}

/// Output is either an existing directory (file written as `<input_stem>.wav` inside),
/// or an explicit file path which must end in `.wav`.
fn resolve_output(input: &Path, output: &Path) -> Result<PathBuf> {
	if output.is_dir() {
		let stem = input.file_stem().ok_or_else(|| eyre!("input path has no file stem: {}", input.display()))?;
		return Ok(output.join(stem).with_extension(OUT_EXT));
	}
	match output.extension().and_then(|e| e.to_str()) {
		Some(ext) if ext.eq_ignore_ascii_case(OUT_EXT) => Ok(output.to_path_buf()),
		Some(ext) => bail!("output extension must be .{OUT_EXT}, got .{ext}"),
		None => bail!("output '{}' is neither an existing directory nor a file path with .{OUT_EXT} extension", output.display()),
	}
}

async fn preflight_uv() -> Result<()> {
	let status = Command::new("uv").arg("--version").stdout(Stdio::null()).stderr(Stdio::null()).status().await;
	match status {
		Ok(s) if s.success() => Ok(()),
		_ => bail!("`uv` is required for the tts command (https://docs.astral.sh/uv/). Install it and re-run."),
	}
}
