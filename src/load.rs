use std::{
	collections::{BTreeMap, VecDeque},
	fs,
	path::Path,
	sync::atomic::{AtomicU64, AtomicUsize, Ordering},
};

use color_eyre::eyre::{Result, bail, eyre};
use futures::future::join_all;
use regex::Regex;
use reqwest::Client;
use scraper::{Html, Selector};

use crate::section::{book_root, decode_entities, enforce_contiguous, paragraphs_to_md};

/// When a Cloudflare 503 is detected, parallelism is clamped down to this.
const CF_FALLBACK_PARALLEL: usize = 4;
/// ...and the inter-chunk wait is clamped up to (at least) this many seconds.
const CF_FALLBACK_TIMEOUT_SECS: u64 = 1;

pub async fn run(url: &str, css_text: &[String], css_title: Option<&str>, parallel: usize, timeout: u64, force: bool, dir: &Path, name_override: Option<&str>) -> Result<()> {
	let (url_template, start, end) = parse_load_url(url)?;
	let name = name_override.map(str::to_owned).unwrap_or_else(|| book_name_from_url(url));
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

	println!(
		"loading {} pages with parallel<= {parallel}, timeout>= {timeout}s -> {}",
		pages_to_load.len(),
		sections_dir.display()
	);

	let client = BookClient::try_new(parallel, timeout)?;

	let mut queue: VecDeque<u32> = pages_to_load.into();
	let mut chunk_idx = 0u32;
	let mut raw_titles: BTreeMap<u32, String> = BTreeMap::new();
	while !queue.is_empty() {
		let par = client.effective_parallel();
		let to_secs = client.effective_timeout_secs();
		if chunk_idx > 0 && to_secs > 0 {
			println!("  waiting {to_secs}s between chunks...");
			tokio::time::sleep(std::time::Duration::from_secs(to_secs)).await;
		}

		let take = par.min(queue.len());
		let chunk: Vec<u32> = queue.drain(..take).collect();
		let futs = chunk.iter().map(|&p| load_page(&client, &url_template, p, css_text, css_title, &sections_dir));
		let results = join_all(futs).await;

		let mut requeue = Vec::new();
		for (page, res) in chunk.iter().zip(results) {
			match res {
				Ok(PageOutcome::Saved { raw_title }) =>
					if let Some(t) = raw_title {
						raw_titles.insert(*page, t);
					},
				Ok(PageOutcome::Throttled) => requeue.push(*page),
				Err(e) => {
					enforce_contiguous(&sections_dir, start, end);
					return Err(e);
				}
			}
		}
		// retry throttled pages first, preserving their original order
		for p in requeue.into_iter().rev() {
			queue.push_front(p);
		}
		chunk_idx += 1;
	}

	if let Some(gap) = enforce_contiguous(&sections_dir, start, end) {
		let loaded = gap - start;
		println!("loaded {loaded} contiguous pages ({start}..={})", gap - 1);
		bail!("stopped at page {gap} (gap in sequence)");
	}

	if css_title.is_some() {
		let chapters = mark_chapters(&sections_dir, start, end, &raw_titles)?;
		println!("marked {chapters} chapter starts (Levenshtein ratio > 0.25)");
	}

	println!("loaded all {} pages ({start}..={end})", end - start + 1);
	println!("book name: {name}");
	Ok(())
}

/// Walk sections in order. For each page whose raw title was just scraped, decide whether
/// it begins a new chapter (Levenshtein ratio against the last kept title > 25%, or none yet).
/// Chapter starts get a `# title` heading prepended to the .md. Pages absent from `raw_titles`
/// (already processed in a prior run) feed `last_chapter_title` from their existing top-level
/// `# X` heading, if any.
fn mark_chapters(dir: &Path, start: u32, end: u32, raw_titles: &BTreeMap<u32, String>) -> Result<u32> {
	let mut last_chapter_title: Option<String> = None;
	let mut starts = 0u32;
	for page in start..=end {
		let md_path = dir.join(format!("section_{page}.md"));
		let Some(raw) = raw_titles.get(&page) else {
			let md = fs::read_to_string(&md_path)?;
			if let Some(line) = md.lines().next()
				&& let Some(t) = line.strip_prefix("# ")
			{
				last_chapter_title = Some(t.trim().to_string());
			}
			continue;
		};
		assert!(!raw.is_empty(), "scrape_page rejects empty titles");
		let new_chapter = match &last_chapter_title {
			None => true,
			Some(prev) => title_diff_ratio(prev, raw) > 0.25,
		};
		if new_chapter {
			let body = fs::read_to_string(&md_path)?;
			let mut out = String::with_capacity(raw.len() + body.len() + 4);
			out.push_str("# ");
			out.push_str(raw);
			out.push('\n');
			out.push_str(&body);
			fs::write(&md_path, out)?;
			last_chapter_title = Some(raw.clone());
			starts += 1;
		}
	}
	Ok(starts)
}

/// Levenshtein distance / max(len_a, len_b), in chars. Returns 0.0 if both empty.
fn title_diff_ratio(a: &str, b: &str) -> f64 {
	let a: Vec<char> = a.chars().collect();
	let b: Vec<char> = b.chars().collect();
	let (m, n) = (a.len(), b.len());
	if m == 0 && n == 0 {
		return 0.0;
	}
	let mut prev: Vec<usize> = (0..=n).collect();
	let mut curr = vec![0usize; n + 1];
	for i in 1..=m {
		curr[0] = i;
		for j in 1..=n {
			let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
			curr[j] = (prev[j] + 1).min(curr[j - 1] + 1).min(prev[j - 1] + cost);
		}
		std::mem::swap(&mut prev, &mut curr);
	}
	prev[n] as f64 / m.max(n) as f64
}

#[cfg(test)]
mod tests {
	use super::title_diff_ratio;

	#[test]
	fn identical_titles_are_continuation() {
		let r = title_diff_ratio("Chapter 232 - 232: Before the Storm", "Chapter 232 - 232: Before the Storm");
		assert!(r <= 0.25, "expected continuation, got ratio {r}");
	}

	#[test]
	fn different_chapters_cross_threshold() {
		let r = title_diff_ratio("Chapter 232 - 232: Before the Storm", "Chapter 233 - 233: After the Battle");
		assert!(r > 0.25, "expected new chapter, got ratio {r}");
	}

	#[test]
	fn empty_inputs() {
		assert_eq!(title_diff_ratio("", ""), 0.0);
	}
}

struct BookClient {
	http: Client,
	user_parallel: usize,
	user_timeout_secs: u64,
	/// Clamps parallel DOWN. `usize::MAX` = unset.
	force_max_parallel: AtomicUsize,
	/// Clamps inter-chunk timeout UP. `0` = unset.
	force_min_timeout_secs: AtomicU64,
}

impl BookClient {
	fn try_new(parallel: usize, timeout_secs: u64) -> Result<Self> {
		let http = Client::builder()
			.user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/91.0.4472.124 Safari/537.36")
			.build()?;
		Ok(Self {
			http,
			user_parallel: parallel,
			user_timeout_secs: timeout_secs,
			force_max_parallel: AtomicUsize::new(usize::MAX),
			force_min_timeout_secs: AtomicU64::new(0),
		})
	}

	fn effective_parallel(&self) -> usize {
		self.user_parallel.min(self.force_max_parallel.load(Ordering::Relaxed)).max(1)
	}

	fn effective_timeout_secs(&self) -> u64 {
		self.user_timeout_secs.max(self.force_min_timeout_secs.load(Ordering::Relaxed))
	}

	/// Called when we observe a 503 with `server: cloudflare`. Idempotent across concurrent calls.
	fn trip_cloudflare_throttle(&self) {
		let prev = self.force_max_parallel.swap(CF_FALLBACK_PARALLEL, Ordering::Relaxed);
		if prev > CF_FALLBACK_PARALLEL {
			self.force_min_timeout_secs.fetch_max(CF_FALLBACK_TIMEOUT_SECS, Ordering::Relaxed);
			tracing::warn!("Cloudflare 503 detected; clamping parallel <= {CF_FALLBACK_PARALLEL}, timeout >= {CF_FALLBACK_TIMEOUT_SECS}s and re-queuing throttled pages");
		}
	}
}

enum ScrapeOutcome {
	Blocks {
		paragraphs: Vec<String>,
		title: Option<String>,
	},
	/// 503 from Cloudflare — caller should re-queue this page.
	Throttled,
}

enum PageOutcome {
	Saved { raw_title: Option<String> },
	Throttled,
}

fn parse_load_url(url: &str) -> Result<(String, u32, u32)> {
	let range_re = Regex::new(r"(\d+)\.\.(=?)(\d+)/?$").unwrap();
	let caps = range_re.captures(url).ok_or_else(|| eyre!("URL must end with a range like 1..100 or 1..=100 (trailing / ok)"))?;

	let start: u32 = caps[1].parse()?;
	let inclusive = &caps[2] == "=";
	let end_raw: u32 = caps[3].parse()?;
	let end = if inclusive { end_raw } else { end_raw - 1 };

	if end < start {
		bail!("empty range: {start}..{end_raw}");
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

async fn load_page(client: &BookClient, url_template: &str, page: u32, css_text: &[String], css_title: Option<&str>, outdir: &Path) -> Result<PageOutcome> {
	let url = url_template.replace("{}", &page.to_string());
	let out_path = outdir.join(format!("section_{page}.md"));

	let (content_blocks, raw_title) = match scrape_page(client, &url, css_text, css_title).await? {
		ScrapeOutcome::Blocks { paragraphs, title } => (paragraphs, title),
		ScrapeOutcome::Throttled => {
			tracing::debug!("page {page} throttled, will retry");
			return Ok(PageOutcome::Throttled);
		}
	};
	let text = content_blocks.join("\n\n");
	let decoded = decode_entities(&text);
	let lines: Vec<&str> = decoded.lines().collect();
	let md = paragraphs_to_md(None, &lines);
	fs::write(out_path, md)?;
	println!("  page {page} ok");

	Ok(PageOutcome::Saved { raw_title })
}

async fn scrape_page(client: &BookClient, url: &str, css_selector_strings: &[String], css_title: Option<&str>) -> Result<ScrapeOutcome> {
	let response = client.http.get(url).send().await?;

	if response.status() == reqwest::StatusCode::SERVICE_UNAVAILABLE
		&& response
			.headers()
			.get(reqwest::header::SERVER)
			.and_then(|v| v.to_str().ok())
			.is_some_and(|s| s.eq_ignore_ascii_case("cloudflare"))
	{
		client.trip_cloudflare_throttle();
		return Ok(ScrapeOutcome::Throttled);
	}

	if !response.status().is_success() {
		bail!("Failed to retrieve page. Status code: {}", response.status());
	}

	let html_content = response.text().await?;
	let document = Html::parse_document(&html_content);

	let mut css_selectors = Vec::with_capacity(css_selector_strings.len());
	for s in css_selector_strings.iter() {
		let selector = Selector::parse(s).map_err(|e| eyre!("Invalid CSS selector: {s}. Error: {e}"))?;
		css_selectors.push(selector);
	}

	assert!(!css_selectors.is_empty(), "clap enforces required = true");
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

	let content_selector = Selector::parse("h1, h2, h3, h4, h5, h6, p, div.subtitle").map_err(|e| eyre!("Internal error: Invalid content block selector: {e}"))?;

	let mut content_blocks = Vec::new();

	for element in container.select(&content_selector) {
		let text = element.text().collect::<Vec<_>>().join("").trim().to_string();

		if text.is_empty() {
			continue;
		}

		let tag_name = element.value().name();

		let formatted_block = match tag_name {
			"p" => text,
			"h1" => format!("# {text}"),
			"h2" => format!("## {text}"),
			"h3" => format!("### {text}"),
			"h4" => format!("#### {text}"),
			"h5" => format!("##### {text}"),
			"h6" => format!("###### {text}"),
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

	let title = match css_title {
		Some(sel) => {
			let parsed = Selector::parse(sel).map_err(|e| eyre!("Invalid title selector: {sel}. Error: {e}"))?;
			let elem = document.select(&parsed).next().ok_or_else(|| eyre!("title selector matched no element: {sel}"))?;
			let t = elem.text().collect::<Vec<_>>().join("").trim().to_string();
			if t.is_empty() {
				bail!("title selector matched empty text: {sel}");
			}
			Some(decode_entities(&t))
		}
		None => None,
	};

	Ok(ScrapeOutcome::Blocks { paragraphs: content_blocks, title })
}
