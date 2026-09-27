//TODO!!!: This. Should be the entry point to all integration tests of rust projects, following https://matklad.github.io/2021/02/27/delete-cargo-integration-tests.html
use std::{
	path::PathBuf,
	process::Output,
	sync::atomic::{AtomicUsize, Ordering},
};

static CASE: AtomicUsize = AtomicUsize::new(0);

fn parse(ext: &str, book: &str) -> (Output, PathBuf) {
	let dir = std::env::temp_dir().join(format!("book_parser_it_{}_{}", std::process::id(), CASE.fetch_add(1, Ordering::Relaxed)));
	std::fs::create_dir_all(&dir).unwrap();
	let input = dir.join(format!("book.{ext}"));
	std::fs::write(&input, book).unwrap();
	let out = std::process::Command::new(env!("CARGO_BIN_EXE_book_parser"))
		.env("HOME", &dir) // v_utils indexes app dirs under $HOME
		.env("XDG_CACHE_HOME", dir.join(".cache"))
		.args(["--dir".as_ref(), dir.as_os_str(), "from".as_ref(), "parse".as_ref(), "-f".as_ref(), input.as_os_str()])
		.output()
		.unwrap();
	(out, dir.join("book/sections"))
}

#[test]
fn txt_duplicate_chapter_number_errors() {
	let (out, _) = parse("txt", "Глава 1 A\nfirst\nГлава 1 A\nsecond\n");
	assert!(!out.status.success(), "silently overwrote section_1: {}", String::from_utf8_lossy(&out.stdout));
	assert!(String::from_utf8_lossy(&out.stderr).contains("duplicate section number 1"));
}

#[test]
fn fb2_duplicate_chapter_number_errors() {
	let (out, _) = parse(
		"fb2",
		"<FictionBook><body><section><title><p>Глава 1</p></title><p>first</p></section><section><title><p>Глава 1</p></title><p>second</p></section></body></FictionBook>",
	);
	assert!(!out.status.success(), "silently overwrote section_1: {}", String::from_utf8_lossy(&out.stdout));
	assert!(String::from_utf8_lossy(&out.stderr).contains("duplicate section number 1"));
}

#[test]
fn txt_chapters_become_sections() {
	let (out, sections) = parse("txt", "Глава 1 A\nfirst\nГлава 2 B\nsecond\n");
	assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
	assert!(std::fs::read_to_string(sections.join("section_2.md")).unwrap().contains("second"));
}
