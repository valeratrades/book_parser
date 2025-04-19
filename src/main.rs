use clap::Parser;
use color_eyre::eyre::{Result, bail, eyre};
use reqwest::Client;
use scraper::{Html, Selector};

#[derive(Debug, Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
	/// The URL of the page to scrape
	#[clap(short, long)]
	url: String,
	/// The CSS selector of the container element. Ex: ".page_text"
	#[clap(short, long)]
	css_selector: String,
	/// Language to translate to (using llms). Ex: "German"
	#[clap(short, long)]
	language: Option<String>,
}
#[derive(Debug, Clone, Copy, derive_more::FromStr)]
enum ServerProtocol {
	Wayland,
	X11,
}

#[tokio::main]
async fn main() {
	v_utils::clientside!();
	let cli: Cli = Cli::parse();

	let content_blocks = parse(&cli.url, &cli.css_selector).await.unwrap();
	let mut text = content_blocks.join("\n\n");

	if let Some(lang) = cli.language {
		text = translate(text, lang).await.unwrap();
	}

	println!("{text:#}");
}

//Q: potentially switch to DeepL API
async fn translate(text: String, language: String) -> Result<String> {
	let q = format!("Translate provided text to {language}: ```{text}```. Output as a codeblock.",);
	let answer = ask_llm::oneshot(q, ask_llm::Model::Medium).await.unwrap();
	tracing::info!("request cost (cents): {}", answer.cost_cents);
	let codeblock = answer
		.extract_codeblock(None)
		.map_err(|_| eyre!("LLM has faltered and wasn't able to provide a codeblock with translated text"))?;
	Ok(codeblock)
}

async fn parse(url: &str, css_selector: &str) -> Result<Vec<String>> {
	let client = Client::builder()
		.user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/91.0.4472.124 Safari/537.36")
		.build()?;

	let response = client.get(url).send().await?;

	if !response.status().is_success() {
		bail!("Failed to retrieve the webpage. Status code: {}", response.status());
	}

	let html_content = response.text().await?;
	let document = Html::parse_document(&html_content);

	let container_selector = Selector::parse(css_selector).map_err(|_| eyre!("Invalid container selector: {}", css_selector))?;

	let container = document
		.select(&container_selector)
		.next()
		.ok_or_else(|| eyre!("Container not found with selector: {}", css_selector))?;

	// Create paragraph selector
	let paragraph_selector = Selector::parse("p").map_err(|_| eyre!("Invalid paragraph selector"))?;

	// Collect all content blocks (paragraphs and headings)
	let mut content_blocks = Vec::new();

	// Process heading tags (h1-h6)
	for heading_level in 1..=6 {
		let heading_selector = Selector::parse(&format!("h{}", heading_level)).map_err(|_| eyre!("Invalid h{} selector", heading_level))?;

		for heading in container.select(&heading_selector) {
			let text = heading.text().collect::<Vec<_>>().join("").trim().to_string();
			if !text.is_empty() {
				// Create the appropriate number of # characters
				let heading_markers = "#".repeat(heading_level);
				content_blocks.push(format!("{} {}", heading_markers, text));
			}
		}
	}

	// Process paragraphs
	for p in container.select(&paragraph_selector) {
		let text = p.text().collect::<Vec<_>>().join("").trim().to_string();
		if !text.is_empty() {
			content_blocks.push(text);
		}
	}

	if content_blocks.is_empty() {
		bail!("No content blocks (paragraphs or headings) found in the specified container");
	}

	Ok(content_blocks)
}
