use std::{
	collections::HashMap,
	fs,
	io::{BufRead, BufReader, Read},
	path::Path,
};

use color_eyre::eyre::{Result, eyre};
use quick_xml::{Reader, events::Event};
use regex::Regex;

use crate::section::{book_root, paragraphs_to_md};

pub fn run(file: &Path, chapter_pattern: Option<&str>, dir: &Path) -> Result<()> {
	if !file.exists() {
		return Err(eyre!("file '{}' does not exist", file.display()));
	}
	let ext = file.extension().and_then(|e| e.to_str()).ok_or_else(|| eyre!("input file has no extension"))?;
	match ext {
		"txt" | "fb2" | "epub" => {}
		_ => return Err(eyre!("unsupported extension '.{ext}', expected .txt, .fb2, or .epub")),
	}
	let stem = file.file_stem().ok_or_else(|| eyre!("input file has no stem"))?.to_string_lossy().to_string();
	fs::write(v_utils::xdg_cache_file!("last_book_name"), &stem)?;

	let root = book_root(dir, &stem);
	let sections_dir = root.join("sections");
	fs::create_dir_all(&sections_dir)?;

	let count = match ext {
		"txt" => {
			let pat = chapter_pattern.unwrap_or(r"^Глава [0-9]+");
			let re = Regex::new(pat)?;
			parse_txt(file, &re, &sections_dir)?
		}
		"fb2" => {
			if chapter_pattern.is_some() {
				return Err(eyre!("--chapter-pattern is not applicable to .fb2 files"));
			}
			parse_fb2(file, &sections_dir)?
		}
		"epub" => {
			if chapter_pattern.is_some() {
				return Err(eyre!("--chapter-pattern is not applicable to .epub files"));
			}
			parse_epub(file, &sections_dir)?
		}
		_ => unreachable!(),
	};

	println!("parsed {count} sections -> {}", sections_dir.display());
	Ok(())
}

fn parse_txt(input: &Path, chapter_re: &Regex, outdir: &Path) -> Result<u32> {
	let f = fs::File::open(input)?;
	let r = BufReader::new(f);
	let num_re = Regex::new(r"[0-9]+").unwrap();
	let mut current_title: Option<String> = None;
	let mut current_lines: Vec<String> = Vec::new();
	let mut current_num: Option<u32> = None;
	let mut count = 0u32;

	let flush = |num: u32, title: Option<&str>, lines: &[String], outdir: &Path| -> Result<()> {
		let refs: Vec<&str> = lines.iter().map(|s| s.as_str()).collect();
		let md = paragraphs_to_md(title, &refs);
		fs::write(outdir.join(format!("section_{num}.md")), md)?;
		Ok(())
	};

	for line in r.lines() {
		let line = line?;
		if chapter_re.is_match(&line) {
			if let Some(m) = num_re.find(&line) {
				if let Some(num) = current_num {
					flush(num, current_title.as_deref(), &current_lines, outdir)?;
					count += 1;
				}
				let num: u32 = line[m.start()..m.end()].parse().unwrap();
				current_num = Some(num);
				current_title = Some(line.clone());
				current_lines.clear();
				continue;
			}
		}
		if current_num.is_some() {
			current_lines.push(line);
		}
	}
	if let Some(num) = current_num {
		flush(num, current_title.as_deref(), &current_lines, outdir)?;
		count += 1;
	}
	Ok(count)
}

fn parse_fb2(input: &Path, outdir: &Path) -> Result<u32> {
	let content = fs::read_to_string(input)?;
	let mut reader = Reader::from_str(&content);
	reader.config_mut().trim_text(true);

	let num_re = Regex::new(r"[0-9]+").unwrap();
	let mut buf = Vec::new();
	let mut in_body = false;
	let mut section_depth: u32 = 0;
	let mut in_section = false;
	let mut in_title = false;
	let mut title_text = String::new();
	let mut current_num: Option<u32> = None;
	let mut paragraphs: Vec<String> = Vec::new();
	let mut current_para = String::new();
	let mut count = 0u32;

	loop {
		match reader.read_event_into(&mut buf) {
			Ok(Event::Start(e)) => {
				let name = e.name();
				if name.as_ref() == b"body" {
					in_body = true;
				} else if in_body && name.as_ref() == b"section" {
					section_depth += 1;
					if section_depth == 1 {
						in_section = true;
						title_text.clear();
						paragraphs.clear();
						current_num = None;
					}
				} else if in_section && name.as_ref() == b"title" {
					in_title = true;
					title_text.clear();
				}
			}
			Ok(Event::End(e)) => {
				let name = e.name();
				if name.as_ref() == b"body" {
					in_body = false;
				} else if name.as_ref() == b"section" {
					if section_depth == 1 {
						if let Some(num) = current_num {
							let refs: Vec<&str> = paragraphs.iter().map(|s| s.as_str()).collect();
							let title = if title_text.is_empty() { None } else { Some(title_text.as_str()) };
							let md = paragraphs_to_md(title, &refs);
							fs::write(outdir.join(format!("section_{num}.md")), md)?;
							count += 1;
						}
						in_section = false;
					}
					section_depth = section_depth.saturating_sub(1);
				} else if name.as_ref() == b"title" {
					in_title = false;
					if current_num.is_none() {
						if let Some(m) = num_re.find(&title_text) {
							current_num = Some(title_text[m.start()..m.end()].parse().unwrap());
						}
					}
				} else if in_section && name.as_ref() == b"p" && !in_title {
					if !current_para.is_empty() {
						paragraphs.push(std::mem::take(&mut current_para));
					}
				}
			}
			Ok(Event::Text(e)) =>
				if in_section {
					let text = e.decode().unwrap_or_default();
					if in_title {
						title_text.push_str(&text);
					} else if current_num.is_some() {
						current_para.push_str(&text);
					}
				},
			Ok(Event::Eof) => break,
			Err(e) => {
				return Err(eyre!("FB2 parse error at {}: {e:?}", reader.buffer_position()));
			}
			_ => {}
		}
		buf.clear();
	}
	Ok(count)
}

fn parse_epub(input: &Path, outdir: &Path) -> Result<u32> {
	let file = fs::File::open(input)?;
	let mut archive = zip::ZipArchive::new(BufReader::new(file))?;

	let opf_path = find_opf_path(&mut archive)?;
	let spine_hrefs = read_spine(&mut archive, &opf_path)?;

	let opf_dir = Path::new(&opf_path).parent().map(|p| p.to_string_lossy().to_string()).unwrap_or_default();

	let mut count = 0u32;
	for href in &spine_hrefs {
		let full_path = if opf_dir.is_empty() { href.clone() } else { format!("{opf_dir}/{href}") };

		let mut entry = match archive.by_name(&full_path) {
			Ok(e) => e,
			Err(_) => continue,
		};
		let mut content = String::new();
		entry.read_to_string(&mut content)?;

		let paras = extract_paragraphs_from_xhtml(&content);
		if paras.is_empty() {
			continue;
		}

		count += 1;
		let title = extract_title_from_xhtml(&content);
		let refs: Vec<&str> = paras.iter().map(|s| s.as_str()).collect();
		let md = paragraphs_to_md(title.as_deref(), &refs);
		fs::write(outdir.join(format!("section_{count}.md")), md)?;
	}
	Ok(count)
}

fn find_opf_path(archive: &mut zip::ZipArchive<BufReader<fs::File>>) -> Result<String> {
	let mut container = archive.by_name("META-INF/container.xml")?;
	let mut content = String::new();
	container.read_to_string(&mut content)?;

	let re = Regex::new(r#"full-path="([^"]+\.opf)""#).unwrap();
	re.captures(&content)
		.and_then(|c| c.get(1))
		.map(|m| m.as_str().to_string())
		.ok_or_else(|| eyre!("no .opf path in container.xml"))
}

fn read_spine(archive: &mut zip::ZipArchive<BufReader<fs::File>>, opf_path: &str) -> Result<Vec<String>> {
	let mut opf_entry = archive.by_name(opf_path)?;
	let mut opf = String::new();
	opf_entry.read_to_string(&mut opf)?;

	let item_re = Regex::new(r#"<item\s[^>]*id="([^"]+)"[^>]*href="([^"]+)"[^>]*/?"#).unwrap();
	let mut manifest = HashMap::new();
	for cap in item_re.captures_iter(&opf) {
		manifest.insert(cap[1].to_string(), cap[2].to_string());
	}

	let itemref_re = Regex::new(r#"<itemref\s[^>]*idref="([^"]+)""#).unwrap();
	let mut hrefs = Vec::new();
	for cap in itemref_re.captures_iter(&opf) {
		if let Some(href) = manifest.get(&cap[1]) {
			hrefs.push(href.clone());
		}
	}
	Ok(hrefs)
}

fn extract_paragraphs_from_xhtml(xhtml: &str) -> Vec<String> {
	let mut reader = Reader::from_str(xhtml);
	reader.config_mut().trim_text(true);
	let mut buf = Vec::new();
	let mut paras = Vec::new();
	let mut in_p = false;
	let mut current = String::new();
	loop {
		match reader.read_event_into(&mut buf) {
			Ok(Event::Start(e)) if e.name().as_ref() == b"p" => {
				in_p = true;
				current.clear();
			}
			Ok(Event::End(e)) if e.name().as_ref() == b"p" => {
				in_p = false;
				let trimmed = current.trim().to_string();
				if !trimmed.is_empty() {
					paras.push(trimmed);
				}
			}
			Ok(Event::Text(e)) if in_p => {
				current.push_str(&e.decode().unwrap_or_default());
			}
			Ok(Event::Eof) => break,
			Err(_) => break,
			_ => {}
		}
		buf.clear();
	}
	paras
}

fn extract_title_from_xhtml(xhtml: &str) -> Option<String> {
	let mut reader = Reader::from_str(xhtml);
	reader.config_mut().trim_text(true);
	let mut buf = Vec::new();
	let mut in_h = false;
	let mut title = String::new();
	loop {
		match reader.read_event_into(&mut buf) {
			Ok(Event::Start(e)) => {
				let n = e.name();
				if matches!(n.as_ref(), b"h1" | b"h2" | b"h3") {
					in_h = true;
					title.clear();
				}
			}
			Ok(Event::End(e)) => {
				let n = e.name();
				if matches!(n.as_ref(), b"h1" | b"h2" | b"h3") && in_h {
					let t = title.trim().to_string();
					if !t.is_empty() {
						return Some(t);
					}
					in_h = false;
				}
			}
			Ok(Event::Text(e)) if in_h => {
				title.push_str(&e.decode().unwrap_or_default());
			}
			Ok(Event::Eof) => return None,
			Err(_) => return None,
			_ => {}
		}
		buf.clear();
	}
}
