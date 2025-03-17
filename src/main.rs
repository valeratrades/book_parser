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

	let paragraphs = parse(&cli.url, &cli.css_selector).await.unwrap();
	let mut text = paragraphs.join("\n\n");

	if let Some(lang) = cli.language {
		text = translate(text, lang).await.unwrap();
	}

	println!("{text:#}");
}

//Q: potentially switch to DeepL API
async fn translate(text: String, language: String) -> Result<String> {
	let mut q = format!("Translate provided text to {language}: ```{text}```. Output as a codeblock.",);
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

	let paragraph_selector = Selector::parse("p").map_err(|_| eyre!("Invalid paragraph selector"))?;

	let paragraphs: Vec<String> = container.select(&paragraph_selector).map(|p| p.text().collect::<Vec<_>>().join("").trim().to_string()).collect();

	if paragraphs.is_empty() {
		bail!("No paragraphs found in the specified container");
	}

	Ok(paragraphs)
}
