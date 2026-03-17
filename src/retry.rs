use std::{fs, path::Path};

use color_eyre::eyre::{Result, eyre};

use crate::{
	section::{Stage, book_root, glob_fails},
	translate,
};

pub async fn run(name: &str, max_jobs: usize, force: bool, yes: bool, dir: &Path) -> Result<()> {
	let root = book_root(dir, name);

	let translate_fail_dir = root.join(Stage::Translated.fail_dir_name().unwrap());
	let annotate_fail_dir = root.join(Stage::Annotated.fail_dir_name().unwrap());

	let translate_fails = glob_fails(&translate_fail_dir)?;
	for f in &translate_fails {
		if f.stage != "translate" {
			return Err(eyre!("unexpected stage '{}' in {}", f.stage, f.path.display()));
		}
	}
	let annotate_fails = glob_fails(&annotate_fail_dir)?;
	for f in &annotate_fails {
		if f.stage != "annotate" {
			return Err(eyre!("unexpected stage '{}' in {}", f.stage, f.path.display()));
		}
	}

	if translate_fails.is_empty() && annotate_fails.is_empty() {
		println!("no .fail files found — nothing to retry");
		return Ok(());
	}

	println!(
		"found {} .fail files (translate: {}, annotate: {})",
		translate_fails.len() + annotate_fails.len(),
		translate_fails.len(),
		annotate_fails.len(),
	);

	if !translate_fails.is_empty() {
		translate::preflight_ollama(yes).await?;
	}

	let max_output_tokens = translate::CHUNK_LIMIT * translate::MAX_EXPANSION as usize;
	let sections_dir = root.join(Stage::Raw.dir_name());
	let translated_dir = root.join(Stage::Translated.dir_name());
	let annotated_dir = root.join(Stage::Annotated.dir_name());

	// Process translate failures
	if !translate_fails.is_empty() {
		// Collect (num, section_path, language) upfront so borrows are stable
		let items: Vec<_> = translate_fails
			.iter()
			.map(|fail| {
				let language = fail.setting("language").expect(".fail file missing language setting").to_string();
				let section_path = sections_dir.join(format!("section_{}.md", fail.num));
				if force {
					let _ = fs::remove_file(translated_dir.join(format!("section_{}.md", fail.num)));
				}
				let _ = fs::remove_file(&fail.path);
				(fail.num, section_path, language)
			})
			.collect();

		for chunk in items.chunks(max_jobs) {
			let futs: Vec<_> = chunk
				.iter()
				.map(|(num, path, language)| translate::translate_section(path, *num, language, max_output_tokens, &translated_dir, &translate_fail_dir))
				.collect();
			run_batch(futs).await;
		}
	}

	// Process annotate failures
	if !annotate_fails.is_empty() {
		let source_dir = root.join(Stage::Translated.dir_name());
		let items: Vec<_> = annotate_fails
			.iter()
			.map(|fail| {
				let language = fail.setting("language").expect(".fail file missing language setting").to_string();
				let wlimit = fail.setting("wlimit").expect(".fail file missing wlimit setting").to_string();
				if force {
					let _ = fs::remove_file(annotated_dir.join(format!("section_{}.md", fail.num)));
				}
				let _ = fs::remove_file(&fail.path);
				(fail.num, language, wlimit)
			})
			.collect();

		for chunk in items.chunks(max_jobs) {
			let futs: Vec<_> = chunk
				.iter()
				.map(|(num, language, wlimit)| crate::annotate::annotate_section(*num, language, wlimit, &source_dir, &annotated_dir, &annotate_fail_dir))
				.collect();
			run_batch(futs).await;
		}
	}

	// Report any newly-created .fail files
	let remaining_translate = glob_fails(&translate_fail_dir).map(|v| v.len()).unwrap_or(0);
	let remaining_annotate = glob_fails(&annotate_fail_dir).map(|v| v.len()).unwrap_or(0);
	let remaining = remaining_translate + remaining_annotate;
	if remaining > 0 {
		return Err(eyre!("{remaining} sections still failing after retry (see .fail files)"));
	}

	println!("retry done");
	Ok(())
}

async fn run_batch(futs: Vec<impl std::future::Future<Output = Result<()>>>) {
	let results = futures::future::join_all(futs).await;
	for r in results {
		if let Err(e) = r {
			eprintln!("  {e}");
		}
	}
}
