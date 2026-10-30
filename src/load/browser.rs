use std::time::Duration;

use chromiumoxide::{Browser, BrowserConfig, Handler, cdp::js_protocol::runtime::EvaluateParams, error::CdpError};
use color_eyre::eyre::{Result, ensure, eyre};
use futures::StreamExt;

/// Covers SPA render plus the first-use download of the on-device translation model.
const PAGE_TIMEOUT: Duration = Duration::from_secs(120);

/// Waits for the SPA to settle, then rewrites every content block (and the title) in place through Chrome's
/// on-device `Translator`. Chrome's page-translate races SPA hydration and can leave content untranslated.
const TRANSLATE_JS: &str = r#"async (sels, blocks, titleSel, target) => {
	const src = document.documentElement.lang;
	if (!src) throw new Error('page has no <html lang>');
	let tr;
	for (let attempt = 1; !tr; attempt++) {
		// After a relaunch Chrome registers installed language packs lazily; creates racing that registration fail.
		try { tr = await Translator.create({ sourceLanguage: src, targetLanguage: target }); }
		catch (e) {
			if (attempt === 5) throw new Error(`Translator ${src} -> ${target}: ${e}`);
			await new Promise(r => setTimeout(r, 1000));
		}
	}
	const container = () => sels.map(s => document.querySelector(s)).find(Boolean);
	let prev;
	for (;;) {
		const t = container()?.textContent.trim();
		if (t && t === prev) break;
		prev = t;
		await new Promise(r => setTimeout(r, 1000));
	}
	const els = [...container().querySelectorAll(blocks)];
	const title = titleSel && document.querySelector(titleSel);
	if (title) els.push(title);
	const todo = els.filter(e => /\p{L}/u.test(e.textContent));
	await Promise.all(todo.map(async e => { e.textContent = await tr.translate(e.textContent); }));
	return todo.length;
}"#;

pub(super) struct Session {
	browser: Browser,
	target_lang: String,
}

impl Session {
	/// Chrome's CDP handler must be polled for any page command to progress, so it is raced against `f`; it ending first is an error.
	pub(super) async fn run<T>(target_lang: &str, f: impl AsyncFnOnce(&Session) -> Result<T>) -> Result<T> {
		let cfg = BrowserConfig::builder()
			.new_headless_mode()
			.request_timeout(PAGE_TIMEOUT) // the evaluate request stays open while the page renders and translates
			.user_data_dir(v_utils::xdg_cache_dir!("chrome_profile"))
			.build()
			.map_err(|e| eyre!("chrome config: {e}"))?;
		let (browser, mut handler) = Browser::launch(cfg).await?;
		let mut session = Session {
			browser,
			target_lang: target_lang.to_owned(),
		};
		let r = tokio::select! {
			r = f(&session) => r,
			e = drain(&mut handler) => Err(match e {
				Ok(()) => eyre!("chrome CDP handler exited before loading finished"),
				Err(e) => eyre!("chrome CDP handler failed: {e}"),
			}),
		};
		let (closed, drained) = tokio::join!(session.browser.close(), drain(&mut handler)); // closing ends the handler stream
		closed?;
		drained?;
		session.browser.wait().await?;
		r
	}

	pub(super) async fn fetch_translated(&self, url: &str, css_text: &[String], css_title: Option<&str>) -> Result<String> {
		let page = self.browser.new_page(url).await?;
		let expr = format!(
			"({TRANSLATE_JS})({}, {}, {}, {})",
			serde_json::to_string(css_text)?,
			serde_json::to_string(super::CONTENT_BLOCKS)?,
			serde_json::to_string(&css_title)?,
			serde_json::to_string(&self.target_lang)?,
		);
		let params = EvaluateParams::builder()
			.expression(expr)
			.await_promise(true)
			.user_gesture(true) // `Translator.create` needs user activation to download a model
			.build()
			.map_err(|e| eyre!("evaluate params: {e}"))?;
		let translated: u64 = tokio::time::timeout(PAGE_TIMEOUT, page.evaluate_expression(params))
			.await
			.map_err(|_| eyre!("{url}: not rendered and translated within {PAGE_TIMEOUT:?}"))?
			.map_err(|e| match e {
				CdpError::JavascriptException(e) => eyre!("{url}: {}", e.exception.and_then(|o| o.description).unwrap_or(e.text)),
				e => e.into(),
			})?
			.into_value()?;
		ensure!(translated > 0, "{url}: no text to translate");
		let html = page.content().await?;
		page.close().await?;
		Ok(html)
	}
}

async fn drain(handler: &mut Handler) -> Result<(), CdpError> {
	while let Some(ev) = handler.next().await {
		ev?;
	}
	Ok(())
}
