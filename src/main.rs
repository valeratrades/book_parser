use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};
use color_eyre::eyre::Result;

mod annotate;
mod compile;
mod load;
mod parse;
mod section;
mod translate;

#[derive(Parser)]
#[command(author, version, about = "Book processing pipeline")]
struct Cli {
	/// Base directory for all books
	#[arg(long, default_value_os_t = default_dir())]
	dir: PathBuf,
	/// Max parallel jobs
	#[arg(long, default_value_t = 2)]
	max_jobs: usize,
	/// Overwrite existing files instead of skipping
	#[arg(long)]
	force: bool,
	#[command(subcommand)]
	cmd: Cmd,
}

fn default_dir() -> PathBuf {
	dirs::home_dir().expect("no home directory").join("tmp/process_book")
}

#[derive(Subcommand)]
enum Cmd {
	/// Ingest a book from a local file or URL
	From {
		#[command(subcommand)]
		source: FromCmd,
	},
	/// Apply a processing stage to sections
	Apply {
		#[command(subcommand)]
		stage: ApplyCmd,
	},
	/// Assemble sections into epub/md
	Compile {
		/// Book name (directory under --dir)
		name: String,
		/// Output format
		#[arg(short, long, default_value = "epub")]
		format: OutputFormat,
	},
}

#[derive(Subcommand)]
enum FromCmd {
	/// Split a local book file into sections
	Parse {
		/// Input book file (.txt, .fb2, or .epub)
		#[arg(short, long)]
		file: PathBuf,
		/// Chapter heading pattern regex (for .txt files)
		#[arg(long)]
		chapter_pattern: Option<String>,
	},
	/// Scrape pages from a URL range
	///
	/// URL format: https://site.com/b/12345/read#t1..100
	Load {
		/// URL with trailing range, e.g. https://example.com/b/123/read#t1..50
		url: String,
		/// CSS selectors for content extraction (can be repeated)
		#[arg(short, long)]
		css: Vec<String>,
		/// Parallel page downloads per chunk
		#[arg(long, default_value_t = 16)]
		parallel: usize,
		/// Seconds to wait between chunks
		#[arg(long, default_value_t = 0)]
		timeout: u64,
	},
}

#[derive(Subcommand)]
enum ApplyCmd {
	/// LLM-translate sections
	Translate {
		/// Book name (directory under --dir)
		name: String,
		/// Target language
		#[arg(short, long)]
		language: String,
		/// Section range, e.g. 1..50, 1..=50, 5.., ..=20
		#[arg(short, long)]
		range: Option<String>,
	},
	/// Annotate infrequent words (via translate_infrequent)
	Annotate {
		/// Book name (directory under --dir)
		name: String,
		/// Target language
		#[arg(short, long)]
		language: String,
		/// Word frequency limit
		#[arg(short, long)]
		wlimit: String,
		/// Section range
		#[arg(short, long)]
		range: Option<String>,
	},
}

#[derive(Clone, ValueEnum)]
enum OutputFormat {
	Epub,
	Md,
	Markdown,
}

impl std::fmt::Display for OutputFormat {
	fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
		match self {
			OutputFormat::Epub => write!(f, "epub"),
			OutputFormat::Md | OutputFormat::Markdown => write!(f, "md"),
		}
	}
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
	v_utils::clientside!();
	let cli = Cli::parse();

	match cli.cmd {
		Cmd::From { source } => match source {
			FromCmd::Parse { file, chapter_pattern } => {
				parse::run(&file, chapter_pattern.as_deref(), &cli.dir)?;
			}
			FromCmd::Load { url, css, parallel, timeout } => {
				load::run(&url, &css, parallel, timeout, cli.force, &cli.dir).await?;
			}
		},
		Cmd::Apply { stage } => match stage {
			ApplyCmd::Translate { name, language, range } => {
				translate::run(&name, &language, range.as_deref(), cli.max_jobs, cli.force, &cli.dir).await?;
			}
			ApplyCmd::Annotate { name, language, wlimit, range } => {
				annotate::run(&name, &language, &wlimit, range.as_deref(), cli.max_jobs, cli.force, &cli.dir).await?;
			}
		},
		Cmd::Compile { name, format } => {
			compile::run(&name, &format.to_string(), cli.force, &cli.dir)?;
		}
	}

	Ok(())
}
