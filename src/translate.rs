use std::{fs, path::Path};

use color_eyre::eyre::{Result, eyre};

use crate::section::{PageRange, Stage, book_root, collect_numbered, glob_fails, md_title, md_to_plaintext, paragraphs_to_md, parse_range};

const CHUNK_LIMIT: usize = 5000;

pub async fn run(name: &str, language: &str, range: Option<&str>, max_jobs: usize, force: bool, dir: &Path) -> Result<()> {
	let root = book_root(dir, name);
	let sections_dir = root.join(Stage::Raw.dir_name());
	let translated_dir = root.join(Stage::Translated.dir_name());
	let fail_dir = root.join(Stage::Translated.fail_dir_name().unwrap());

	if !sections_dir.exists() {
		return Err(eyre!("sections not found at '{}' — run `from parse` or `from load` first", sections_dir.display()));
	}

	let range = match range {
		Some(s) => parse_range(s)?,
		None => PageRange::all(),
	};
	fs::create_dir_all(&translated_dir)?;
	fs::create_dir_all(&fail_dir)?;

	let all = collect_numbered(&sections_dir, "section_", ".md")?;
	let sections: Vec<_> = all.into_iter().filter(|(n, _)| range.contains(*n)).collect();

	println!(
		"translating {} sections{}",
		sections.len(),
		if range.since.is_some() || range.until.is_some() {
			format!(" (range: {range})")
		} else {
			String::new()
		}
	);

	let client = ask_llm::Client::default().model(ask_llm::Model::Translate);

	// main pass
	{
		let mut to_translate: Vec<(u32, std::path::PathBuf)> = Vec::new();
		let mut skipped = 0u32;
		for (num, path) in &sections {
			if !force && translated_dir.join(format!("section_{num}.md")).exists() {
				skipped += 1;
				continue;
			}
			to_translate.push((*num, path.clone()));
		}
		if skipped > 0 {
			eprintln!("warning: skipped {skipped} already-translated sections (use --force to overwrite)");
		}
		for chunk in to_translate.chunks(max_jobs) {
			let futs: Vec<_> = chunk
				.iter()
				.map(|(num, path)| translate_section(&client, path, *num, language, &translated_dir, &fail_dir))
				.collect();
			futures::future::try_join_all(futs).await?;
		}
	}

	// retry failures
	{
		let fails = glob_fails(&fail_dir)?;
		let mut to_retry: Vec<(u32, std::path::PathBuf)> = Vec::new();
		for fail in fails {
			let num: u32 = fs::read_to_string(&fail)?.trim().parse()?;
			if !range.contains(num) {
				continue;
			}
			let _ = fs::remove_file(translated_dir.join(format!("section_{num}.md")));
			let _ = fs::remove_file(&fail);
			to_retry.push((num, sections_dir.join(format!("section_{num}.md"))));
		}
		for chunk in to_retry.chunks(max_jobs) {
			let futs: Vec<_> = chunk
				.iter()
				.map(|(num, path)| translate_section(&client, path, *num, language, &translated_dir, &fail_dir))
				.collect();
			futures::future::try_join_all(futs).await?;
		}
	}

	println!("translation done");
	Ok(())
}

/// Split text into chunks of roughly `CHUNK_LIMIT` chars, breaking at paragraph boundaries (`\n`).
/// The last chunk gets whatever remains without size-checking.
fn chunk_plaintext(text: &str) -> Vec<&str> {
	if text.len() <= CHUNK_LIMIT {
		return vec![text];
	}

	let n_chunks = (text.len() + CHUNK_LIMIT - 1) / CHUNK_LIMIT;
	let mut chunks = Vec::with_capacity(n_chunks);
	let mut offset = 0;

	for _ in 0..n_chunks - 1 {
		let target = offset + CHUNK_LIMIT;
		// step back from target to find the last newline (paragraph boundary)
		let cut = match text[offset..target].rfind('\n') {
			Some(pos) => offset + pos + 1, // include the newline in the current chunk
			None => target,                // no newline found, cut at limit
		};
		chunks.push(&text[offset..cut]);
		offset = cut;
	}
	// last chunk: everything remaining
	chunks.push(&text[offset..]);
	chunks
}

async fn translate_section(client: &ask_llm::Client, section: &Path, num: u32, language: &str, out_dir: &Path, fail_dir: &Path) -> Result<()> {
	let md = fs::read_to_string(section)?;
	let plaintext = md_to_plaintext(&md);
	let chunks = chunk_plaintext(&plaintext);

	let n_chunks = chunks.len();
	if n_chunks > 1 {
		tracing::info!("section {num}: {n_chunks} chunks ({} chars)", plaintext.len());
	}

	let mut translated_parts: Vec<String> = Vec::with_capacity(n_chunks);
	for (i, chunk) in chunks.into_iter().enumerate() {
		let q = format!("Translate provided text to {language}: ```{chunk}```. Output as a codeblock.");
		let answer = match client.ask(q).await {
			Ok(a) => a,
			Err(e) => {
				fs::write(fail_dir.join(format!("section_{num}.fail")), format!("{num}\n"))?;
				return Err(eyre!("LLM failed for section {num} chunk {i}: {e}"));
			}
		};
		tracing::info!("section {num} chunk {}/{n_chunks} cost (cents): {}", i + 1, answer.cost_cents);

		let part = match answer.extract_codeblock(None) {
			Ok(cb) => cb,
			Err(_) => {
				fs::write(fail_dir.join(format!("section_{num}.fail")), format!("{num}\n"))?;
				return Err(eyre!("LLM failed to produce codeblock for section {num} chunk {i}"));
			}
		};
		translated_parts.push(part);
	}

	let translated = translated_parts.join("\n");
	let title = md_title(&md);
	let lines: Vec<&str> = translated.lines().collect();
	let out_md = paragraphs_to_md(title.as_deref(), &lines);
	fs::write(out_dir.join(format!("section_{num}.md")), out_md)?;
	println!("  section {num} translated");

	Ok(())
}
