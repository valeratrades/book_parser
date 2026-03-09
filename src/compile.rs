use std::{
	fs,
	io::Write as _,
	path::{Path, PathBuf},
};

use color_eyre::eyre::{Result, eyre};
use zip::{ZipWriter, write::FileOptions};

use crate::section::{PageRange, Stage, book_root, collect_numbered, escape_xml, md_title};

pub fn run(name: &str, format: &str, force: bool, dir: &Path, out_dir: &Path) -> Result<()> {
	let root = book_root(dir, name);

	let (stage, sections) = Stage::resolve_latest(root)?;

	if sections.is_empty() {
		return Err(eyre!("no section files found in any stage directory"));
	}

	let parsed = collect_numbered(&root.join(Stage::Raw.dir_name()), "section_", ".md")?;
	let range = if sections.len() < parsed.len() {
		let first_t = sections.first().unwrap().0;
		let last_t = sections.last().unwrap().0;
		let first_p = parsed.first().map(|p| p.0).unwrap_or(first_t);
		let last_p = parsed.last().map(|p| p.0).unwrap_or(last_t);
		PageRange {
			since: (first_t != first_p).then_some(first_t),
			until: (last_t != last_p).then_some(last_t),
		}
	} else {
		PageRange::all()
	};

	let out_ext = match format {
		"epub" => "epub",
		"md" | "markdown" => "md",
		_ => return Err(eyre!("unsupported format '{format}', expected epub or md")),
	};
	let out_path = out_dir.join(format!("{name}{range}.{out_ext}"));

	if out_path.exists() && !force {
		return Err(eyre!("output file '{}' already exists (use --force to overwrite)", out_path.display()));
	}

	match out_ext {
		"epub" => compile_epub(&sections, &out_path)?,
		"md" => compile_markdown(&sections, &out_path)?,
		_ => unreachable!(),
	}

	println!("compiled {} sections ({stage}) -> {}", sections.len(), out_path.display());
	Ok(())
}

fn compile_epub(sections: &[(u32, PathBuf)], out: &Path) -> Result<()> {
	let file = fs::File::create(out)?;
	let mut zip = ZipWriter::new(file);

	let opts_stored: FileOptions<'_, ()> = FileOptions::default().compression_method(zip::CompressionMethod::Stored);
	let opts: FileOptions<'_, ()> = FileOptions::default().compression_method(zip::CompressionMethod::Deflated);

	zip.start_file("mimetype", opts_stored)?;
	zip.write_all(b"application/epub+zip")?;

	zip.start_file("META-INF/container.xml", opts.clone())?;
	zip.write_all(
		b"<?xml version=\"1.0\" encoding=\"utf-8\"?>\n\
		  <container version=\"1.0\" xmlns=\"urn:oasis:names:tc:opendocument:xmlns:container\">\n\
		  <rootfiles>\n\
		  <rootfile full-path=\"OEBPS/content.opf\" media-type=\"application/oebps-package+xml\"/>\n\
		  </rootfiles>\n\
		  </container>\n",
	)?;

	for (num, path) in sections {
		let md = fs::read_to_string(path)?;
		let xhtml = md_to_xhtml(&md, *num);
		zip.start_file(format!("OEBPS/section_{num}.xhtml"), opts.clone())?;
		zip.write_all(xhtml.as_bytes())?;
	}

	let mut opf = String::from(
		"<?xml version=\"1.0\" encoding=\"utf-8\"?>\n\
		 <package xmlns=\"http://www.idpf.org/2007/opf\" version=\"3.0\" unique-identifier=\"uid\">\n\
		 <metadata xmlns:dc=\"http://purl.org/dc/elements/1.1/\">\n\
		 <dc:identifier id=\"uid\">process-book-output</dc:identifier>\n\
		 <dc:title>Translated Book</dc:title>\n\
		 <dc:language>de</dc:language>\n\
		 <meta property=\"dcterms:modified\">2025-01-01T00:00:00Z</meta>\n\
		 </metadata>\n\
		 <manifest>\n\
		 <item id=\"nav\" href=\"nav.xhtml\" media-type=\"application/xhtml+xml\" properties=\"nav\"/>\n",
	);
	for (num, _) in sections {
		opf.push_str(&format!("<item id=\"s{num}\" href=\"section_{num}.xhtml\" media-type=\"application/xhtml+xml\"/>\n"));
	}
	opf.push_str("</manifest>\n<spine>\n");
	for (num, _) in sections {
		opf.push_str(&format!("<itemref idref=\"s{num}\"/>\n"));
	}
	opf.push_str("</spine>\n</package>\n");

	zip.start_file("OEBPS/content.opf", opts.clone())?;
	zip.write_all(opf.as_bytes())?;

	let mut nav = String::from(
		"<?xml version=\"1.0\" encoding=\"utf-8\"?>\n\
		 <html xmlns=\"http://www.w3.org/1999/xhtml\" xmlns:epub=\"http://www.idpf.org/2007/ops\">\n\
		 <head><title>Navigation</title></head>\n\
		 <body>\n\
		 <nav epub:type=\"toc\">\n\
		 <ol>\n",
	);
	for (num, path) in sections {
		let md = fs::read_to_string(path)?;
		let title = md_title(&md).unwrap_or_else(|| format!("Page {num}"));
		nav.push_str(&format!("<li><a href=\"section_{num}.xhtml\">{}</a></li>\n", escape_xml(&title)));
	}
	nav.push_str("</ol>\n</nav>\n</body>\n</html>\n");

	zip.start_file("OEBPS/nav.xhtml", opts)?;
	zip.write_all(nav.as_bytes())?;

	zip.finish()?;
	Ok(())
}

fn compile_markdown(sections: &[(u32, PathBuf)], out: &Path) -> Result<()> {
	let mut f = fs::File::create(out)?;
	for (i, (num, path)) in sections.iter().enumerate() {
		if i > 0 {
			f.write_all(b"\n")?;
		}
		let md = fs::read_to_string(path)?;
		if md_title(&md).is_none() {
			writeln!(f, "## Page {num}\n")?;
		}
		f.write_all(md.as_bytes())?;
	}
	Ok(())
}

fn md_to_xhtml(md: &str, page_num: u32) -> String {
	let mut s = String::from(
		"<?xml version=\"1.0\" encoding=\"utf-8\"?>\n\
		 <html xmlns=\"http://www.w3.org/1999/xhtml\">\n\
		 <head><title></title></head>\n\
		 <body>\n",
	);
	if md_title(md).is_none() {
		s.push_str(&format!("<h2>Page {page_num}</h2>\n"));
	}
	for line in md.lines() {
		if let Some(title) = line.strip_prefix("# ") {
			let t = title.trim();
			if !t.is_empty() {
				s.push_str(&format!("<h1>{}</h1>\n", escape_xml(t)));
			}
		} else if !line.trim().is_empty() {
			s.push_str(&format!("<p>{}</p>\n", escape_xml(line)));
		}
	}
	s.push_str("</body>\n</html>\n");
	s
}
