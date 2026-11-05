use std::{fs, path::Path};

use color_eyre::eyre::{Result, bail};
use tokio::process::Command;

use crate::section::{PageRange, Stage, book_root, collect_numbered, glob_fails, md_title, md_to_plaintext, paragraphs_to_md, parse_range, persist_language, write_atomic};

const SECTION_SEP: &str = "\n\x1e\n"; // \x1e is whitespace to translate_infrequent's tokenizer, so it passes through recompose untouched
#[derive(clap::Args)]
pub struct Args {
	/// Target language
	#[arg(short, long)]
	language: String,
	/// Word frequency limit
	#[arg(short, long)]
	wlimit: String,
	/// Section range
	#[arg(short, long)]
	range: Option<String>,
	/// Append IPA to each gloss: `word {translation [ipa]}`
	#[arg(short, long)]
	phonetic: bool,
}

pub async fn run(name: &str, args: Args, force: bool, dir: &Path) -> Result<()> {
	let Args { language, wlimit, range, phonetic } = args;
	let (language, wlimit, range) = (language.as_str(), wlimit.as_str(), range.as_deref());
	let root = book_root(dir, name);
	let source_dir = root.join(Stage::Translated.dir_name());
	let annotated_dir = root.join(Stage::Annotated.dir_name());
	let fail_dir = root.join(Stage::Annotated.fail_dir_name().unwrap());

	if !source_dir.exists() {
		bail!("source dir not found at '{}' — run `apply translate` first", source_dir.display());
	}

	persist_language(root, language)?;

	let range = match range {
		Some(s) => parse_range(s)?,
		None => PageRange::all(),
	};
	fs::create_dir_all(&annotated_dir)?;
	fs::create_dir_all(&fail_dir)?;

	let all = collect_numbered(&source_dir, "section_", ".md")?;
	let explicit_range = !range.is_all();
	let sections: Vec<_> = all.into_iter().filter(|(n, _)| range.contains(*n)).collect();

	let mut total_failed = 0u32;

	// main pass
	{
		let mut to_annotate: Vec<u32> = Vec::new();
		let mut skipped = 0u32;
		for (num, _) in &sections {
			if !force && annotated_dir.join(format!("section_{num}.md")).exists() {
				skipped += 1;
				continue;
			}
			to_annotate.push(*num);
		}
		println!(
			"found {} translated sections{}, {} already annotated, annotating {}",
			sections.len(),
			if explicit_range { format!(" (range: {range})") } else { String::new() },
			skipped,
			to_annotate.len(),
		);
		if !to_annotate.is_empty()
			&& let Err(e) = annotate_sections(&to_annotate, language, wlimit, phonetic, &source_dir, &annotated_dir, &fail_dir).await
		{
			eprintln!("  {e}");
			total_failed += to_annotate.len() as u32;
		}
	}

	// retry failures
	{
		let fails = glob_fails(&fail_dir)?;
		let mut to_retry: Vec<u32> = Vec::new();
		for fail in fails {
			if !range.contains(fail.num) {
				continue;
			}
			let _ = fs::remove_file(annotated_dir.join(format!("section_{}.md", fail.num)));
			let _ = fs::remove_file(&fail.path);
			to_retry.push(fail.num);
		}
		if !to_retry.is_empty()
			&& let Err(e) = annotate_sections(&to_retry, language, wlimit, phonetic, &source_dir, &annotated_dir, &fail_dir).await
		{
			eprintln!("  {e}");
			total_failed += to_retry.len() as u32;
		}
	}

	if total_failed > 0 {
		bail!("{total_failed} sections failed to annotate (see .fail files). Re-run to retry.");
	}
	println!("annotation done");
	Ok(())
}

/// One `translate_infrequent` call over sections `min(nums)..=max(nums)`, with everything before as `--history`, so its memory of seen words runs in reading order. Only `nums` are written.
pub async fn annotate_sections(nums: &[u32], language: &str, wlimit: &str, phonetic: bool, source_dir: &Path, out_dir: &Path, fail_dir: &Path) -> Result<()> {
	let (lo, hi) = (*nums.iter().min().expect("callers skip empty batches"), *nums.iter().max().unwrap());
	let all = collect_numbered(source_dir, "section_", ".md")?;
	let mut history = String::new();
	let mut fed: Vec<(u32, String)> = Vec::new();
	for (n, path) in &all {
		match *n {
			n if n < lo => history.push_str(&md_to_plaintext(&fs::read_to_string(path)?)),
			n if n <= hi => fed.push((n, fs::read_to_string(path)?)),
			_ => break,
		}
	}
	for n in nums {
		assert!(fed.iter().any(|(m, _)| m == n), "section_{n}.md is not in {}", source_dir.display());
	}

	let tmp_in = out_dir.join(format!("sections_{lo}-{hi}.tmp.txt"));
	fs::write(&tmp_in, fed.iter().map(|(_, md)| md_to_plaintext(md)).collect::<Vec<_>>().join(SECTION_SEP))?;
	let tmp_history = out_dir.join(format!("history_{lo}.tmp.txt"));
	let mut cmd = Command::new("translate_infrequent");
	cmd.args(["--cache", "-l", language, "-w", wlimit, "-f"]).arg(&tmp_in);
	if phonetic {
		cmd.arg("--phonetic");
	}
	if !history.is_empty() {
		fs::write(&tmp_history, &history)?;
		cmd.arg("--history").arg(&tmp_history);
	}
	let out = cmd.output().await;
	let _ = fs::remove_file(&tmp_in);
	let _ = fs::remove_file(&tmp_history); // absent when there was no history
	let out = out?;

	if !out.status.success() {
		for num in nums {
			fs::write(
				fail_dir.join(format!("section_{num}.fail")),
				format!("annotate\nlanguage={language}\nwlimit={wlimit}\nphonetic={phonetic}\n"),
			)?;
		}
		let stderr = String::from_utf8_lossy(&out.stderr);
		let tail: Vec<&str> = stderr.lines().filter(|l| !l.contains(" - DEBUG - ")).collect(); // its DEBUG log is per-word noise
		bail!("translate_infrequent failed for sections {lo}..={hi}:\n{}", tail[tail.len().saturating_sub(15)..].join("\n"));
	}

	let annotated = String::from_utf8(out.stdout)?;
	let parts: Vec<&str> = annotated.split(SECTION_SEP).collect();
	assert_eq!(parts.len(), fed.len(), "translate_infrequent must keep every section separator");
	for ((num, md), part) in fed.iter().zip(parts) {
		if !nums.contains(num) {
			continue; // gap section: fed only to keep memory continuous
		}
		let lines: Vec<&str> = part.lines().collect();
		write_atomic(&out_dir.join(format!("section_{num}.md")), &paragraphs_to_md(md_title(md).as_deref(), &lines))?;
		println!("  section {num} annotated");
	}

	Ok(())
}
