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
	/// The CSS selector of the container element. Ex: ".page_text". Multiple can be provided, which will be iterated over until the first match.
	#[clap(short, long)]
	css_selectors: Vec<String>,
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

	let content_blocks = parse(&cli.url, cli.css_selectors).await.unwrap();
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

async fn parse(url: &str, css_selector_strings: Vec<String>) -> Result<Vec<String>> {
	let client = Client::builder()
		// Using a common browser user agent can help avoid blocking by some websites
		.user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/91.0.4472.124 Safari/537.36")
		.build()?;

	let response = client.get(url).send().await?;

	// Ensure the request was successful
	if !response.status().is_success() {
		bail!("Failed to retrieve the webpage. Status code: {}", response.status());
	}

	let html_content = response.text().await?;
	let document = Html::parse_document(&html_content);

	let mut css_selectors = Vec::with_capacity(css_selector_strings.len());
	for s in css_selector_strings.iter() {
		let selector = Selector::parse(s).map_err(|e| eyre!("Invalid CSS selector: {}. Error: {}", s, e))?;
		css_selectors.push(selector);
	}

	let container = {
		assert!(css_selectors.len() > 0, "No CSS selectors provided");
		let mut i = 0;
		loop {
			match document.select(&css_selectors[i]).next() /*We assume there's only one main container matching the selector*/
			{
				Some(container) => {
					tracing::info!("Got a match on css selector: {}", css_selector_strings[i]);
					break container
				},
				None => {
					i += 1;
					if i >= css_selectors.len() {
						bail!("No matching container found for any of the provided CSS selectors");
					}
				}
			}
		}
	};

	// Create a single selector that matches all desired content elements:
	// headings (h1-h6), paragraphs (p), and specific divs (div.subtitle)
	// The scraper `select` method inherently returns elements in their document order.
	let content_selector = Selector::parse("h1, h2, h3, h4, h5, h6, p, div.subtitle") // Added div.subtitle here
		.map_err(|e| eyre!("Internal error: Invalid content block selector: {}", e))?; // This selector should always be valid

	let mut content_blocks = Vec::new();

	// Iterate through all matching elements within the container, preserving document order
	for element in container.select(&content_selector) {
		// Extract and clean up the text content of the element
		let text = element.text().collect::<Vec<_>>().join("").trim().to_string();

		// Skip elements that contain only whitespace or are empty
		if text.is_empty() {
			continue;
		}

		// Get the HTML tag name (e.g., "h1", "p", "div") to determine formatting
		let tag_name = element.value().name();

		// Format the text based on the element's tag type
		let formatted_block = match tag_name {
			"p" => text,
			"h1" => format!("# {}", text),
			"h2" => format!("## {}", text),
			"h3" => format!("### {}", text),
			"h4" => format!("#### {}", text),
			"h5" => format!("##### {}", text),
			"h6" => format!("###### {}", text),
			"div" => {
				// a website I care to scrape has this delimiter standard.
				if element.value().has_class("subtitle", scraper::CaseSensitivity::CaseSensitive) && text == "* * *" {
					"#### * * *".to_owned()
				} else {
					continue;
				}
			}
			_ => continue, // Should not happen with our specific selector, but acts as a safeguard
		};

		content_blocks.push(formatted_block);
	}

	// Check if any content was actually extracted
	if content_blocks.is_empty() {
		bail!("No content blocks (paragraphs, headings, or subtitles) found in the specified container");
	}

	Ok(content_blocks)
}
