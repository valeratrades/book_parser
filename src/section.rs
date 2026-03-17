use std::{
	collections::BTreeSet,
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

/// `None` means "all sections". `Some(set)` means only those exact numbers.
#[derive(Clone)]
pub struct PageRange(Option<BTreeSet<u32>>);

impl PageRange {
	pub fn all() -> Self {
		Self(None)
	}

	pub fn is_all(&self) -> bool {
		self.0.is_none()
	}

	pub fn contains(&self, n: u32) -> bool {
		match &self.0 {
			None => true,
			Some(set) => set.contains(&n),
		}
	}

	pub fn from_sorted(nums: &[u32]) -> Self {
		Self(Some(nums.iter().copied().collect()))
	}
}

/// Parse a comma-separated list of ranges/numbers.
/// Each element can be: a single number (`5`), or a range (`1..50`, `1..=50`, `5..`, `..=20`).
pub fn parse_range(s: &str) -> Result<PageRange> {
	let range_re = Regex::new(r"^(\d+)?\.\.(=?)(\d+)?$").unwrap();
	let mut set = BTreeSet::new();

	for part in s.split(',') {
		let part = part.trim();
		if part.is_empty() {
			continue;
		}
		if let Ok(n) = part.parse::<u32>() {
			set.insert(n);
			continue;
		}
		let caps = range_re
			.captures(part)
			.ok_or_else(|| eyre!("invalid range '{part}', expected e.g. 517, 1..50, 1..=50, 5.., ..=20, 1,2,4"))?;
		let since = caps.get(1).map(|m| m.as_str().parse::<u32>()).transpose()?;
		let inclusive = &caps[2] == "=";
		let end_raw = caps.get(3).map(|m| m.as_str().parse::<u32>()).transpose()?;
		let until = match (inclusive, end_raw) {
			(true, Some(n)) => Some(n),
			(false, Some(0)) => return Err(eyre!("empty range: {part}")),
			(false, Some(n)) => Some(n - 1),
			(_, None) => return Err(eyre!("open-ended ranges not supported in lists: {part}")),
		};
		let since = since.ok_or_else(|| eyre!("open-ended ranges not supported in lists: {part}"))?;
		let until = until.unwrap(); // guaranteed Some by above
		if until < since {
			return Err(eyre!("empty range: {part}"));
		}
		for n in since..=until {
			set.insert(n);
		}
	}
	if set.is_empty() {
		return Err(eyre!("empty range: '{s}'"));
	}
	Ok(PageRange(Some(set)))
}

impl fmt::Display for PageRange {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		let set = match &self.0 {
			None => return Ok(()),
			Some(s) => s,
		};
		if set.is_empty() {
			return Ok(());
		}
		let min = *set.iter().next().unwrap();
		let max = *set.iter().next_back().unwrap();
		// Check if contiguous
		if (max - min + 1) as usize == set.len() {
			if min == max { write!(f, "_{min}") } else { write!(f, "_{min}..={max}") }
		} else {
			write!(f, "_[{min},{max}]")
		}
	}
}

pub fn book_root(base: &Path, name: &str) -> &'static PathBuf {
	static ROOT: OnceLock<PathBuf> = OnceLock::new();
	ROOT.get_or_init(|| base.join(name))
}

const LANGUAGE_FILE: &str = ".language";

pub fn persist_language(root: &Path, language: &str) -> Result<()> {
	fs::write(root.join(LANGUAGE_FILE), language)?;
	Ok(())
}

pub fn load_language(root: &Path) -> Option<String> {
	let s = fs::read_to_string(root.join(LANGUAGE_FILE)).ok()?;
	let s = s.trim().to_string();
	if s.is_empty() { None } else { Some(s) }
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

pub struct FailInfo {
	pub num: u32,
	pub stage: String,
	pub settings: Vec<(String, String)>,
	pub path: PathBuf,
}

impl FailInfo {
	pub fn setting(&self, key: &str) -> Option<&str> {
		self.settings.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
	}
}

pub fn glob_fails(dir: &Path) -> Result<Vec<FailInfo>> {
	let num_re = Regex::new(r"section_(\d+)\.fail$").unwrap();
	let mut v = Vec::new();
	if !dir.exists() {
		return Ok(v);
	}
	for e in fs::read_dir(dir)? {
		let e = e?;
		let p = e.path();
		if !p.is_file() || !p.extension().is_some_and(|x| x == "fail") {
			continue;
		}
		let fname = e.file_name().to_string_lossy().to_string();
		let num: u32 = num_re
			.captures(&fname)
			.and_then(|c| c[1].parse().ok())
			.ok_or_else(|| eyre!("cannot parse section number from fail file: {fname}"))?;

		let content = fs::read_to_string(&p)?;
		let mut lines = content.lines();
		let stage = lines.next().ok_or_else(|| eyre!("empty .fail file: {}", p.display()))?.to_string();
		let settings: Vec<(String, String)> = lines
			.filter_map(|line| {
				let (k, v) = line.split_once('=')?;
				Some((k.to_string(), v.to_string()))
			})
			.collect();
		v.push(FailInfo { num, stage, settings, path: p });
	}
	v.sort_by_key(|f| f.num);
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
