use reqwest::Client;
use serde::Deserialize;
use serde_json::json;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TranslatedText {
	translated_text: String,
}

#[derive(Debug, Deserialize)]
struct TranslationResponse {
	data: TranslationResponseData,
}

#[derive(Debug, Deserialize)]
struct TranslationResponseData {
	translations: Vec<TranslatedText>,
}

async fn batch_translate_german_words(words: Vec<String>, api_key: &str) -> Result<Vec<String>, Box<dyn std::error::Error>> {
	let client = Client::new();

	// Build the Google Translate API URL with your API key
	let url = format!("https://translation.googleapis.com/language/translate/v2?key={}", api_key);

	let request_body = json!({
		"q": words,
		"source": "de",
		"target": "en",
		"format": "text"
	});

	let response = client.post(&url).json(&request_body).send().await?;

	if !response.status().is_success() {
		let error_text = response.text().await?;
		return Err(format!("API error: {}", error_text).into());
	}

	let response_data: TranslationResponse = response.json().await?;

	// Extract just the translated texts into a Vec<String>
	let translations: Vec<String> = response_data.data.translations.into_iter().map(|t| t.translated_text).collect();

	Ok(translations)
}

#[tokio::main]
async fn main() {
	// Your Google Cloud API key
	let api_key = std::env::var("GOOGLE_TRANSLATE_API_KEY").expect("GOOGLE_TRANSLATE_API_KEY environment variable must be set");

	// Example list of German words
	let german_words = vec!["Haus".to_string(), "Baum".to_string(), "Straße".to_string(), "Zeit".to_string(), "Leben".to_string()];

	match batch_translate_german_words(german_words, &api_key).await {
		Ok(translations) => {
			println!("Translations:");
			for (i, translation) in translations.iter().enumerate() {
				println!("  {}: {}", i + 1, translation);
			}
		}
		Err(e) => {
			eprintln!("Error translating words: {}", e);
		}
	}
}
