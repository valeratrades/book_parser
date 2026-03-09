use super::*;

#[test]
fn chunk_plaintext_small_input_single_chunk() {
	let text = "Hello world\nThis is a test\n";
	let chunks = chunk_plaintext(text);
	assert_eq!(chunks.len(), 1);
	assert_eq!(chunks[0], text);
}

#[test]
fn chunk_plaintext_splits_at_paragraph_boundaries() {
	// Create text that exceeds CHUNK_LIMIT
	let paragraph = "A".repeat(200) + "\n";
	let text = paragraph.repeat(40); // 40 * 201 = 8040 chars, > 5000
	let chunks = chunk_plaintext(&text);
	assert!(chunks.len() > 1, "expected multiple chunks, got {}", chunks.len());
	// Every chunk except the last should end with newline (paragraph boundary)
	for (i, chunk) in chunks.iter().enumerate() {
		if i < chunks.len() - 1 {
			assert!(chunk.ends_with('\n'), "chunk {i} doesn't end with newline");
			assert!(chunk.len() <= CHUNK_LIMIT + 200, "chunk {i} too large: {}", chunk.len());
		}
	}
	// Concatenated chunks equal original
	let reassembled: String = chunks.concat();
	assert_eq!(reassembled, text);
}

#[test]
fn chunk_plaintext_no_newlines_cuts_at_limit() {
	let text = "A".repeat(CHUNK_LIMIT * 2);
	let chunks = chunk_plaintext(&text);
	assert_eq!(chunks.len(), 2);
	assert_eq!(chunks[0].len(), CHUNK_LIMIT);
	assert_eq!(chunks[1].len(), CHUNK_LIMIT);
}

#[test]
fn max_expansion_rejects_degenerate_output() {
	// Simulates the guard: 9× expansion (real case from translategemma:4b) must fail
	let input_len = 5000;
	let output_len = 45000; // 9×
	let ratio = output_len as f32 / input_len as f32;
	assert!(ratio > MAX_EXPANSION, "9× expansion should exceed MAX_EXPANSION ({MAX_EXPANSION})");
}

#[test]
fn max_expansion_allows_normal_translation() {
	// German is ~1.2× English; 2× is generous but valid
	let input_len = 5000;
	let output_len = 10000; // 2×
	let ratio = output_len as f32 / input_len as f32;
	assert!(ratio < MAX_EXPANSION, "2× expansion should be within MAX_EXPANSION ({MAX_EXPANSION})");
}
