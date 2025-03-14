use clap::Parser;
use color_eyre::eyre::{Result, bail, eyre};
use reqwest::blocking::Client;
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
}

fn main() {
	color_eyre::install().unwrap();
	let cli: Cli = Cli::parse();

	let paragraphs = parse(&cli.url, &cli.css_selector).unwrap();
	let text = paragraphs.join("\n\n");
	println!("{text}");
}

fn parse(url: &str, css_selector: &str) -> Result<Vec<String>> {
	let client = Client::builder()
		.user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/91.0.4472.124 Safari/537.36")
		.build()?;

	let response = client.get(url).send()?;

	if !response.status().is_success() {
		bail!("Failed to retrieve the webpage. Status code: {}", response.status());
	}

	let html_content = response.text()?;
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
