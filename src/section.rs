use std::{
	fmt, fs,
	path::{Path, PathBuf},
	sync::OnceLock,
};

use color_eyre::eyre::{Result, eyre};
use regex::Regex;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Stage {
	Raw,
	Translated,
	Annotated,
}

impl Stage {
	/// All stages above Raw, in descending order (highest first).
	const PROCESSED: [Stage; 2] = [Stage::Annotated, Stage::Translated];

	pub fn dir_name(self) -> &'static str {
		match self {
			Stage::Raw => "sections",
			Stage::Translated => "sections_translated",
			Stage::Annotated => "sections_annotated",
		}
	}

	pub fn fail_dir_name(self) -> Option<&'static str> {
		match self {
			Stage::Raw => None,
			Stage::Translated => Some("failed_translate"),
			Stage::Annotated => Some("failed_annotate"),
		}
	}

	/// Find the highest stage that has any sections, then return only the section numbers
	/// present at that stage. If no processed stage has files, falls back to Raw.
	pub fn resolve_latest(root: &Path) -> Result<(Stage, Vec<(u32, PathBuf)>)> {
		for stage in Self::PROCESSED {
			let dir = root.join(stage.dir_name());
			if dir.exists() {
				let sections = collect_numbered(&dir, "section_", ".md")?;
				if !sections.is_empty() {
					return Ok((stage, sections));
				}
			}
		}
		let dir = root.join(Stage::Raw.dir_name());
		let sections = collect_numbered(&dir, "section_", ".md")?;
		Ok((Stage::Raw, sections))
	}
}

impl fmt::Display for Stage {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		f.write_str(self.dir_name())
	}
}

#[derive(Clone)]
pub struct PageRange {
	pub since: Option<u32>,
	pub until: Option<u32>,
}

impl PageRange {
	pub fn contains(&self, n: u32) -> bool {
		self.since.map_or(true, |s| n >= s) && self.until.map_or(true, |u| n <= u)
	}

	pub fn all() -> Self {
		Self { since: None, until: None }
	}
}

pub fn parse_range(s: &str) -> Result<PageRange> {
	if let Ok(n) = s.parse::<u32>() {
		return Ok(PageRange { since: Some(n), until: Some(n) });
	}
	let re = Regex::new(r"^(\d+)?\.\.(=?)(\d+)?$").unwrap();
	let caps = re.captures(s).ok_or_else(|| eyre!("invalid range '{s}', expected e.g. 517, 1..50, 1..=50, 5.., ..=20"))?;
	let since = caps.get(1).map(|m| m.as_str().parse::<u32>()).transpose()?;
	let inclusive = &caps[2] == "=";
	let end_raw = caps.get(3).map(|m| m.as_str().parse::<u32>()).transpose()?;
	let until = match (inclusive, end_raw) {
		(true, Some(n)) => Some(n),
		(false, Some(0)) => return Err(eyre!("empty range: {s}")),
		(false, Some(n)) => Some(n - 1),
		(_, None) => None,
	};
	if let (Some(s), Some(u)) = (since, until) {
		if u < s {
			return Err(eyre!("empty range: {s}"));
		}
	}
	Ok(PageRange { since, until })
}

impl fmt::Display for PageRange {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		match (self.since, self.until) {
			(Some(s), Some(u)) => write!(f, "_{s}..={u}"),
			(Some(s), None) => write!(f, "_{s}.."),
			(None, Some(u)) => write!(f, "_..={u}"),
			(None, None) => Ok(()),
		}
	}
}

pub fn book_root(base: &Path, name: &str) -> &'static PathBuf {
	static ROOT: OnceLock<PathBuf> = OnceLock::new();
	ROOT.get_or_init(|| base.join(name))
}

pub fn collect_numbered(dir: &Path, prefix: &str, suffix: &str) -> Result<Vec<(u32, PathBuf)>> {
	let num_re = Regex::new(r"([0-9]+)").unwrap();
	let mut v = Vec::new();
	if !dir.exists() {
		return Ok(v);
	}
	for e in fs::read_dir(dir)? {
		let e = e?;
		let p = e.path();
		if !p.is_file() {
			continue;
		}
		let name = e.file_name().to_string_lossy().to_string();
		if name.starts_with(prefix) && name.ends_with(suffix) {
			if let Some(c) = num_re.captures(&name) {
				if let Ok(n) = c[1].parse::<u32>() {
					v.push((n, p));
				}
			}
		}
	}
	v.sort_by_key(|(n, _)| *n);
	Ok(v)
}

pub fn glob_fails(dir: &Path) -> Result<Vec<PathBuf>> {
	let mut v = Vec::new();
	if !dir.exists() {
		return Ok(v);
	}
	for e in fs::read_dir(dir)? {
		let e = e?;
		let p = e.path();
		if p.is_file() && p.extension().is_some_and(|x| x == "fail") {
			v.push(p);
		}
	}
	Ok(v)
}

pub fn paragraphs_to_md(title: Option<&str>, paragraphs: &[&str]) -> String {
	let mut s = String::new();
	if let Some(t) = title {
		s.push_str("# ");
		s.push_str(t);
		s.push('\n');
	}
	for p in paragraphs {
		let trimmed = p.trim();
		if !trimmed.is_empty() {
			s.push('\n');
			s.push_str(trimmed);
			s.push('\n');
		}
	}
	s
}

pub fn md_title(md: &str) -> Option<String> {
	for line in md.lines() {
		if let Some(title) = line.strip_prefix("# ") {
			let t = title.trim();
			if !t.is_empty() {
				return Some(t.to_string());
			}
		}
	}
	None
}

pub fn md_to_plaintext(md: &str) -> String {
	let mut out = String::new();
	for line in md.lines() {
		if line.starts_with("# ") || line.trim().is_empty() {
			continue;
		}
		out.push_str(line);
		out.push('\n');
	}
	out
}

pub fn decode_entities(s: &str) -> String {
	s.replace("&amp;", "&").replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'")
}

pub fn shell_escape(s: &str) -> String {
	if s.bytes().all(|b| b.is_ascii_alphanumeric()) {
		return s.to_string();
	}
	format!("'{}'", s.replace('\'', r"'\''"))
}

/// Find the first gap in `start..=end` where `section_N.md` is missing.
/// Remove all section files at and after the gap. Returns the gap page, if any.
pub fn enforce_contiguous(dir: &Path, start: u32, end: u32) -> Option<u32> {
	let mut gap = None;
	for page in start..=end {
		let path = dir.join(format!("section_{page}.md"));
		if gap.is_some() {
			let _ = fs::remove_file(path);
		} else if !path.exists() {
			gap = Some(page);
		}
	}
	gap
}

pub fn escape_xml(s: &str) -> String {
	s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}
