use std::{fs, path::Path};

use color_eyre::eyre::{Result, bail, eyre};
use regex::Regex;
use reqwest::Client;
use scraper::{Html, Selector};

use crate::section::{book_root, decode_entities, enforce_contiguous, paragraphs_to_md};

pub async fn run(url: &str, css: &[String], parallel: usize, timeout: u64, force: bool, dir: &Path) -> Result<()> {
	let (url_template, start, end) = parse_load_url(url)?;
	let name = book_name_from_url(url);
	fs::write(v_utils::xdg_cache_file!("last_book_name"), &name)?;
	let root = book_root(dir, &name);
	let sections_dir = root.join("sections");
	fs::create_dir_all(&sections_dir)?;

	if force {
		println!("--force: will overwrite existing pages");
	} else if let Some(gap) = enforce_contiguous(&sections_dir, start, end) {
		println!("cleaned post-gap sections (gap at {gap})");
	}

	let mut pages_to_load = Vec::new();
	let mut skipped = 0u32;
	for page in start..=end {
		let path = sections_dir.join(format!("section_{page}.md"));
		if path.exists() && !force {
			skipped += 1;
			continue;
		}
		pages_to_load.push(page);
	}

	if skipped > 0 {
		eprintln!("warning: skipped {skipped} already-loaded pages (use --force to overwrite)");
	}

	if pages_to_load.is_empty() {
		println!("all {} pages already loaded", end - start + 1);
		println!("book name: {name}");
		return Ok(());
	}

	let n_chunks = (pages_to_load.len() + parallel - 1) / parallel;
	println!("loading {} pages in {} chunks of {} -> {}", pages_to_load.len(), n_chunks, parallel, sections_dir.display());

	let client = Client::builder()
		.user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/91.0.4472.124 Safari/537.36")
		.build()?;

	for (chunk_idx, chunk) in pages_to_load.chunks(parallel).enumerate() {
		if chunk_idx > 0 && timeout > 0 {
			println!("  waiting {timeout}s between chunks...");
			tokio::time::sleep(std::time::Duration::from_secs(timeout)).await;
		}

		let futs: Vec<_> = chunk.iter().map(|&page| load_page(&client, &url_template, page, css, &sections_dir)).collect();
		if let Err(e) = futures::future::try_join_all(futs).await {
			enforce_contiguous(&sections_dir, start, end);
			return Err(e);
		}
	}

	if let Some(gap) = enforce_contiguous(&sections_dir, start, end) {
		let loaded = gap - start;
		println!("loaded {loaded} contiguous pages ({start}..={})", gap - 1);
		return Err(eyre!("stopped at page {gap} (gap in sequence)"));
	}

	println!("loaded all {} pages ({start}..={end})", end - start + 1);
	println!("book name: {name}");
	Ok(())
}

fn parse_load_url(url: &str) -> Result<(String, u32, u32)> {
	let range_re = Regex::new(r"(\d+)\.\.(=?)(\d+)/?$").unwrap();
	let caps = range_re.captures(url).ok_or_else(|| eyre!("URL must end with a range like 1..100 or 1..=100 (trailing / ok)"))?;

	let start: u32 = caps[1].parse()?;
	let inclusive = &caps[2] == "=";
	let end_raw: u32 = caps[3].parse()?;
	let end = if inclusive { end_raw } else { end_raw - 1 };

	if end < start {
		return Err(eyre!("empty range: {start}..{end_raw}"));
	}

	let m = caps.get(0).unwrap();
	let suffix = &url[m.end()..];
	let base = format!("{}{{}}{suffix}", &url[..caps.get(1).unwrap().start()]);

	Ok((base, start, end))
}

fn book_name_from_url(url: &str) -> String {
	let range_re = Regex::new(r"\d+\.\.=?\d+/?$").unwrap();
	let stripped = range_re.replace(url, "");
	let stripped = stripped.strip_prefix("https://").or_else(|| stripped.strip_prefix("http://")).unwrap_or(&stripped);
	let stripped = stripped.split('#').next().unwrap_or(stripped);
	let stripped = stripped.split('?').next().unwrap_or(stripped);
	let parts: Vec<&str> = stripped.split('/').skip(1).filter(|s| !s.is_empty()).collect();
	if parts.is_empty() {
		return "book".to_string();
	}
	parts.join("_")
}

async fn load_page(client: &Client, url_template: &str, page: u32, css_selectors: &[String], outdir: &Path) -> Result<()> {
	let url = url_template.replace("{}", &page.to_string());
	let out_path = outdir.join(format!("section_{page}.md"));

	let content_blocks = scrape_page(client, &url, css_selectors).await?;
	let text = content_blocks.join("\n\n");
	let decoded = decode_entities(&text);
	let lines: Vec<&str> = decoded.lines().collect();
	let md = paragraphs_to_md(None, &lines);
	fs::write(out_path, md)?;
	println!("  page {page} ok");

	Ok(())
}

async fn scrape_page(client: &Client, url: &str, css_selector_strings: &[String]) -> Result<Vec<String>> {
	let response = client.get(url).send().await?;

	if !response.status().is_success() {
		bail!("Failed to retrieve page. Status code: {}", response.status());
	}

	let html_content = response.text().await?;
	let document = Html::parse_document(&html_content);

	let mut css_selectors = Vec::with_capacity(css_selector_strings.len());
	for s in css_selector_strings.iter() {
		let selector = Selector::parse(s).map_err(|e| eyre!("Invalid CSS selector: {}. Error: {}", s, e))?;
		css_selectors.push(selector);
	}

	assert!(css_selectors.len() > 0, "No CSS selectors provided");
	let container = {
		let mut i = 0;
		loop {
			match document.select(&css_selectors[i]).next() {
				Some(container) => {
					tracing::info!("Got a match on css selector: {}", css_selector_strings[i]);
					break container;
				}
				None => {
					i += 1;
					if i >= css_selectors.len() {
						bail!("No matching container found for any of the provided CSS selectors");
					}
				}
			}
		}
	};

	let content_selector = Selector::parse("h1, h2, h3, h4, h5, h6, p, div.subtitle").map_err(|e| eyre!("Internal error: Invalid content block selector: {}", e))?;

	let mut content_blocks = Vec::new();

	for element in container.select(&content_selector) {
		let text = element.text().collect::<Vec<_>>().join("").trim().to_string();

		if text.is_empty() {
			continue;
		}

		let tag_name = element.value().name();

		let formatted_block = match tag_name {
			"p" => text,
			"h1" => format!("# {}", text),
			"h2" => format!("## {}", text),
			"h3" => format!("### {}", text),
			"h4" => format!("#### {}", text),
			"h5" => format!("##### {}", text),
			"h6" => format!("###### {}", text),
			"div" =>
				if element.value().has_class("subtitle", scraper::CaseSensitivity::CaseSensitive) && text == "* * *" {
					"#### * * *".to_owned()
				} else {
					continue;
				},
			_ => continue,
		};

		content_blocks.push(formatted_block);
	}

	if content_blocks.is_empty() {
		bail!("No content blocks (paragraphs, headings, or subtitles) found in the specified container");
	}

	Ok(content_blocks)
}
