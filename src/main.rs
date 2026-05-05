use std::{fs, path::PathBuf};

use clap::{Parser, Subcommand, ValueEnum};
use color_eyre::eyre::{Result, bail, eyre};
use v_utils::io::{ConfirmResult, confirmation};

mod annotate;
mod compile;
mod load;
mod parse;
mod retry;
mod section;
mod translate;
mod tts;

#[derive(Parser)]
#[command(author, version, about = "Book processing pipeline")]
struct Cli {
	/// Base directory for all books
	#[arg(short, long, default_value_os_t = default_dir())]
	dir: PathBuf,
	/// Max parallel jobs
	#[arg(short = 'j', long, default_value_t = 2)]
	max_jobs: usize,
	/// Overwrite existing files instead of skipping
	#[arg(short, long)]
	force: bool,
	/// Assume yes for all confirmation prompts
	#[arg(short, long)]
	yes: bool,
	/// Book name (directory under --dir); cached across runs.
	/// On `from`: overrides the auto-derived name (URL-derived for `load`, file-stem-derived for `parse`).
	/// On `apply`/`compile`: selects which book to operate on (falls back to the last cached name).
	#[arg(short, long, global = true)]
	name: Option<String>,
	#[command(subcommand)]
	cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
	/// Ingest a book from a local file or URL (use subcommand to specify)
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
		/// Output format
		#[arg(short, long, default_value = "epub")]
		format: OutputFormat,
		/// Output directory
		#[arg(short, long, default_value_os_t = default_out_dir())]
		out: PathBuf,
	},
	/// Synthesize speech from a .txt/.md file (standalone — not part of the book pipeline)
	Tts {
		/// Input file (.txt or .md)
		input: PathBuf,
		/// Output: either an existing directory (file named after input) or a `.wav` file path
		output: PathBuf,
		#[command(flatten)]
		model: TtsModelChoice,
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
	/// The trailing `N..M` (exclusive) or `N..=M` (inclusive) is stripped and each
	/// page number substituted in its place to build per-page URLs. The range must
	/// sit at the very end of the URL (an optional trailing `/` is allowed). Open-ended
	/// ranges like `5..` or `..=20` are NOT supported here (only in `apply --range`).
	///
	/// Examples:
	///   # path-segment range, inclusive (chapter/1/, chapter/2/, ..., chapter/2980/)
	///   book_parser from load 'https://lightnovelworld.org/novel/shadow-slave/chapter/1..=2980/' \
	///     --css-text '#chapter-container'
	///
	///   # exclusive: 1..100 fetches pages 1..=99
	///   book_parser from load 'https://example.com/b/123/read#t1..100' --css-text '.content'
	///
	///   # multiple selector fallbacks (first matching wins per page)
	///   book_parser from load 'https://site.com/novel/foo/ch-1..=500' \
	///     --css-text '#chapter-content' --css-text 'article.post' --css-text '.entry-content'
	///
	///   # also extract a chapter title; new-chapter detection by Levenshtein ratio
	///   book_parser from load 'https://site.com/novel/foo/chapter/1..=500/' \
	///     --css-text '#chapter-container' --css-title 'h1.chapter-title'
	Load {
		/// URL whose trailing `N..M` or `N..=M` is replaced by each page number.
		/// E.g. `https://example.com/b/123/chapter/1..=50/` expands to `.../chapter/1/`..`.../chapter/50/`.
		url: String,
		/// CSS selectors for content extraction (can be repeated)
		#[arg(long, required = true)]
		css_text: Vec<String>,
		/// Optional CSS selector for the chapter-title element. If a page's title differs from
		/// the previous kept title by more than 25% (Levenshtein ratio), that page starts a new
		/// chapter and gets a `# title` heading.
		#[arg(long)]
		css_title: Option<String>,
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
		/// Target language
		#[arg(short, long)]
		language: String,
		/// Section range, e.g. 1..50, 1..=50, 5.., ..=20
		#[arg(short, long)]
		range: Option<String>,
	},
	/// Annotate infrequent words (via translate_infrequent)
	Annotate {
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
	/// Retry all failed sections (reads settings from .fail files)
	Retry,
}
/// `--fast` (Kokoro-82M, default) vs `--best` (Chatterbox).
#[derive(clap::Args)]
#[group(required = false, multiple = false)]
struct TtsModelChoice {
	/// Kokoro-82M: fast, CPU-friendly (default)
	#[arg(long)]
	fast: bool,
	/// Chatterbox: larger, higher quality, wants a GPU
	#[arg(long)]
	best: bool,
}

impl From<TtsModelChoice> for tts::Model {
	fn from(c: TtsModelChoice) -> Self {
		if c.best { tts::Model::Best } else { tts::Model::Fast }
	}
}
#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
	v_utils::clientside!();
	let cli = Cli::parse();

	match cli.cmd {
		Cmd::From { source } => match source {
			FromCmd::Parse { file, chapter_pattern } => {
				parse::run(&file, chapter_pattern.as_deref(), &cli.dir, cli.name.as_deref())?;
			}
			FromCmd::Load {
				url,
				css_text,
				css_title,
				parallel,
				timeout,
			} => {
				load::run(&url, &css_text, css_title.as_deref(), parallel, timeout, cli.force, &cli.dir, cli.name.as_deref()).await?;
			}
		},
		Cmd::Apply { stage } => {
			let name = resolve_name(cli.name, cli.yes)?;
			match stage {
				ApplyCmd::Translate { language, range } => {
					translate::run(&name, &language, range.as_deref(), cli.max_jobs, cli.force, cli.yes, &cli.dir).await?;
				}
				ApplyCmd::Annotate { language, wlimit, range } => {
					annotate::run(&name, &language, &wlimit, range.as_deref(), cli.max_jobs, cli.force, &cli.dir).await?;
				}
				ApplyCmd::Retry => {
					retry::run(&name, cli.max_jobs, cli.force, cli.yes, &cli.dir).await?;
				}
			}
		}
		Cmd::Compile { format, out } => {
			let name = resolve_name(cli.name, cli.yes)?;
			compile::run(&name, &format.to_string(), cli.force, &cli.dir, &out)?;
		}
		Cmd::Tts { input, output, model } => {
			tts::run(&input, &output, model.into()).await?;
		}
	}

	Ok(())
}
fn default_dir() -> PathBuf {
	dirs::home_dir().expect("no home directory").join("tmp/process_book")
}

fn default_out_dir() -> PathBuf {
	dirs::home_dir().expect("no home directory").join("Downloads")
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

fn resolve_name(provided: Option<String>, yes: bool) -> Result<String> {
	let cache_path = v_utils::xdg_cache_file!("last_book_name");
	match provided {
		Some(name) => {
			fs::write(&cache_path, &name)?;
			Ok(name)
		}
		None => {
			let cached = fs::read_to_string(&cache_path).map_err(|_| eyre!("no book name provided and no cached name found"))?;
			let cached = cached.trim().to_string();
			if cached.is_empty() {
				bail!("no book name provided and cached name is empty");
			}
			if !yes && confirmation(&format!("proceed with '{cached}'?")).flush_blocking() != ConfirmResult::Yes {
				bail!("aborted");
			}
			Ok(cached)
		}
	}
}
