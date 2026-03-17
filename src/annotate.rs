use std::{fs, path::Path, process::Stdio};

use color_eyre::eyre::{Result, eyre};
use tokio::process::Command;

use crate::section::{PageRange, Stage, book_root, collect_numbered, glob_fails, md_title, md_to_plaintext, paragraphs_to_md, parse_range, persist_language, shell_escape};

async fn run_batch(futs: Vec<impl std::future::Future<Output = Result<()>>>) -> u32 {
	let results = futures::future::join_all(futs).await;
	let mut failed = 0u32;
	for r in results {
		if let Err(e) = r {
			eprintln!("  {e}");
			failed += 1;
		}
	}
	failed
}

pub async fn run(name: &str, language: &str, wlimit: &str, range: Option<&str>, max_jobs: usize, force: bool, dir: &Path) -> Result<()> {
	let root = book_root(dir, name);
	let source_dir = root.join(Stage::Translated.dir_name());
	let annotated_dir = root.join(Stage::Annotated.dir_name());
	let fail_dir = root.join(Stage::Annotated.fail_dir_name().unwrap());

	if !source_dir.exists() {
		return Err(eyre!("source dir not found at '{}' — run `apply translate` first", source_dir.display()));
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
		for chunk in to_annotate.chunks(max_jobs) {
			let futs: Vec<_> = chunk.iter().map(|&num| annotate_section(num, language, wlimit, &source_dir, &annotated_dir, &fail_dir)).collect();
			total_failed += run_batch(futs).await;
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
		for chunk in to_retry.chunks(max_jobs) {
			let futs: Vec<_> = chunk.iter().map(|&num| annotate_section(num, language, wlimit, &source_dir, &annotated_dir, &fail_dir)).collect();
			total_failed += run_batch(futs).await;
		}
	}

	if total_failed > 0 {
		return Err(eyre!("{total_failed} sections failed to annotate (see .fail files). Re-run to retry."));
	}
	println!("annotation done");
	Ok(())
}

pub async fn annotate_section(num: u32, language: &str, wlimit: &str, source_dir: &Path, out_dir: &Path, fail_dir: &Path) -> Result<()> {
	let source_md_path = source_dir.join(format!("section_{num}.md"));
	let md = fs::read_to_string(&source_md_path)?;
	let plaintext = md_to_plaintext(&md);
	let tmp_in = out_dir.join(format!("section_{num}.tmp.txt"));
	fs::write(&tmp_in, &plaintext)?;

	let tmp_out = out_dir.join(format!("section_{num}.txt"));
	let cmd = format!(
		"translate_infrequent -l {} -w {} < '{}' > '{}'",
		shell_escape(language),
		shell_escape(wlimit),
		tmp_in.display(),
		tmp_out.display()
	);
	let status = Command::new("sh").arg("-c").arg(cmd).stdout(Stdio::null()).stderr(Stdio::null()).status().await?;
	let _ = fs::remove_file(&tmp_in);

	if !status.success() {
		fs::write(fail_dir.join(format!("section_{num}.fail")), format!("annotate\nlanguage={language}\nwlimit={wlimit}\n"))?;
		return Err(eyre!("translate_infrequent failed for section {num}"));
	}

	let translated = fs::read_to_string(&tmp_out)?;
	let title = md_title(&md);
	let lines: Vec<&str> = translated.lines().collect();
	let out_md = paragraphs_to_md(title.as_deref(), &lines);
	fs::write(out_dir.join(format!("section_{num}.md")), out_md)?;
	let _ = fs::remove_file(&tmp_out);
	println!("  section {num} annotated");

	Ok(())
}
