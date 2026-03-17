use std::{fs, path::Path};

use color_eyre::eyre::{Result, eyre};
use v_utils::io::{ConfirmResult, confirmation};

use crate::section::{PageRange, Stage, book_root, collect_numbered, glob_fails, md_title, md_to_plaintext, paragraphs_to_md, parse_range, persist_language};

#[cfg(test)]
mod tests;

pub const CHUNK_LIMIT: usize = 5000;
/// If a translated chunk is longer than this multiple of its input, the model degenerated (repetition loop).
pub const MAX_EXPANSION: f32 = 3.0;
const OLLAMA_BASE: &str = "http://localhost:11434";
const OLLAMA_MODEL: &str = "translategemma:4b";

async fn ollama_reachable() -> bool {
	reqwest::Client::new()
		.get(format!("{OLLAMA_BASE}/api/tags"))
		.timeout(std::time::Duration::from_secs(3))
		.send()
		.await
		.is_ok()
}

/// Verify Ollama is running and the translate model is available.
/// Offers to start Ollama and/or pull the model if needed.
pub async fn preflight_ollama(yes: bool) -> Result<()> {
	if !ollama_reachable().await {
		if !yes && confirmation("Ollama is not running. Start it?").flush().await != ConfirmResult::Yes {
			return Err(eyre!("Ollama is not reachable at {OLLAMA_BASE}"));
		}
		tokio::process::Command::new("ollama")
			.arg("serve")
			.stdin(std::process::Stdio::null())
			.stdout(std::process::Stdio::null())
			.stderr(std::process::Stdio::null())
			.spawn()
			.map_err(|e| eyre!("failed to start `ollama serve`: {e}"))?;

		// wait for it to come up
		for _ in 0..20 {
			tokio::time::sleep(std::time::Duration::from_millis(500)).await;
			if ollama_reachable().await {
				break;
			}
		}
		if !ollama_reachable().await {
			return Err(eyre!("started `ollama serve` but it didn't become reachable within 10s"));
		}
		eprintln!("Ollama started.");
	}

	let url = format!("{OLLAMA_BASE}/api/tags");
	let resp = reqwest::Client::new().get(&url).send().await?;
	let body: serde_json::Value = resp.json().await.map_err(|e| eyre!("bad response from Ollama /api/tags: {e}"))?;
	let models = body["models"].as_array().ok_or_else(|| eyre!("unexpected Ollama /api/tags response"))?;

	let base_name = OLLAMA_MODEL.split(':').next().unwrap_or(OLLAMA_MODEL);
	let found = models
		.iter()
		.any(|m| m["name"].as_str().is_some_and(|n| n == OLLAMA_MODEL || n.starts_with(&format!("{base_name}:"))));
	if !found {
		let available: Vec<&str> = models.iter().filter_map(|m| m["name"].as_str()).collect();
		if !available.is_empty() {
			eprintln!("available models: {available:?}");
		}
		if !yes && confirmation(&format!("model '{OLLAMA_MODEL}' not found. Pull it?")).flush().await != ConfirmResult::Yes {
			return Err(eyre!("model '{OLLAMA_MODEL}' not available in Ollama"));
		}
		eprintln!("pulling {OLLAMA_MODEL}...");
		let status = tokio::process::Command::new("ollama")
			.args(["pull", OLLAMA_MODEL])
			.status()
			.await
			.map_err(|e| eyre!("failed to run `ollama pull`: {e}"))?;
		if !status.success() {
			return Err(eyre!("`ollama pull {OLLAMA_MODEL}` failed"));
		}
	}

	Ok(())
}

/// Run a batch of futures, recording failures instead of aborting.
/// Returns the count of failures in this batch.
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

pub async fn run(name: &str, language: &str, range: Option<&str>, max_jobs: usize, force: bool, yes: bool, dir: &Path) -> Result<()> {
	let root = book_root(dir, name);
	let sections_dir = root.join(Stage::Raw.dir_name());
	let translated_dir = root.join(Stage::Translated.dir_name());
	let fail_dir = root.join(Stage::Translated.fail_dir_name().unwrap());

	if !sections_dir.exists() {
		return Err(eyre!("sections not found at '{}' — run `from parse` or `from load` first", sections_dir.display()));
	}

	preflight_ollama(yes).await?;
	persist_language(root, language)?;

	let range = match range {
		Some(s) => parse_range(s)?,
		None => PageRange::all(),
	};
	fs::create_dir_all(&translated_dir)?;
	fs::create_dir_all(&fail_dir)?;

	let all = collect_numbered(&sections_dir, "section_", ".md")?;
	let explicit_range = !range.is_all();
	let sections: Vec<_> = all.into_iter().filter(|(n, _)| range.contains(*n)).collect();

	// Cap output tokens so a degenerate repetition loop fails fast instead of burning 10+ minutes.
	// Very generous ceiling: even if every output char maps to 1 token (worst case), this is still well above any real translation.
	let max_output_tokens = CHUNK_LIMIT * MAX_EXPANSION as usize;
	let mut total_failed = 0u32;

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
		println!(
			"found {} sections{}, {} already translated, translating {}",
			sections.len(),
			if explicit_range { format!(" (range: {range})") } else { String::new() },
			skipped,
			to_translate.len(),
		);
		for chunk in to_translate.chunks(max_jobs) {
			let futs: Vec<_> = chunk
				.iter()
				.map(|(num, path)| translate_section(path, *num, language, max_output_tokens, &translated_dir, &fail_dir))
				.collect();
			total_failed += run_batch(futs).await;
		}
	}

	// retry failures
	{
		let fails = glob_fails(&fail_dir)?;
		let mut to_retry: Vec<(u32, std::path::PathBuf)> = Vec::new();
		for fail in fails {
			if !range.contains(fail.num) {
				continue;
			}
			let _ = fs::remove_file(translated_dir.join(format!("section_{}.md", fail.num)));
			let _ = fs::remove_file(&fail.path);
			to_retry.push((fail.num, sections_dir.join(format!("section_{}.md", fail.num))));
		}
		for chunk in to_retry.chunks(max_jobs) {
			let futs: Vec<_> = chunk
				.iter()
				.map(|(num, path)| translate_section(path, *num, language, max_output_tokens, &translated_dir, &fail_dir))
				.collect();
			total_failed += run_batch(futs).await;
		}
	}

	if total_failed > 0 {
		return Err(eyre!("{total_failed} sections failed to translate (see .fail files). Re-run to retry."));
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
		let target = text.floor_char_boundary(offset + CHUNK_LIMIT);
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

/// Temperatures to try for each chunk: default (0.0), then increasing jitter to escape repetition loops.
const RETRY_TEMPERATURES: [f32; 3] = [0.0, 0.05, 0.15];

pub async fn translate_section(section: &Path, num: u32, language: &str, max_output_tokens: usize, out_dir: &Path, fail_dir: &Path) -> Result<()> {
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

		let mut last_err = None;
		for (attempt, &temp) in RETRY_TEMPERATURES.iter().enumerate() {
			let client = ask_llm::Client::default().model(ask_llm::Model::Translate).max_tokens(max_output_tokens).temperature(temp);

			let answer = match client.ask(q.clone()).await {
				Ok(a) => a,
				Err(e) => {
					last_err = Some(format!("LLM failed for section {num} chunk {i}: {e}"));
					tracing::warn!("section {num} chunk {i} attempt {attempt} (temp={temp}): LLM error, retrying");
					continue;
				}
			};
			tracing::info!("section {num} chunk {}/{n_chunks} cost (cents): {}", i + 1, answer.cost_cents);

			let part = match answer.extract_codeblock(None) {
				Ok(cb) => cb,
				Err(_) => {
					last_err = Some(format!("LLM failed to produce codeblock for section {num} chunk {i}"));
					tracing::warn!("section {num} chunk {i} attempt {attempt} (temp={temp}): no codeblock, retrying");
					continue;
				}
			};
			let ratio = part.len() as f32 / chunk.len().max(1) as f32;
			if ratio > MAX_EXPANSION {
				last_err = Some(format!(
					"section {num} chunk {i}: translated output is {ratio:.1}× the input size ({} vs {} chars) — likely model repetition loop",
					part.len(),
					chunk.len()
				));
				tracing::warn!("section {num} chunk {i} attempt {attempt} (temp={temp}): {ratio:.1}× expansion, retrying");
				continue;
			}

			last_err = None;
			translated_parts.push(part);
			break;
		}
		if let Some(err) = last_err {
			fs::write(fail_dir.join(format!("section_{num}.fail")), format!("translate\nlanguage={language}\n"))?;
			return Err(eyre!("{err}"));
		}
	}

	let translated = translated_parts.join("\n");
	let title = md_title(&md);
	let lines: Vec<&str> = translated.lines().collect();
	let out_md = paragraphs_to_md(title.as_deref(), &lines);
	fs::write(out_dir.join(format!("section_{num}.md")), out_md)?;
	println!("  section {num} translated");

	Ok(())
}
