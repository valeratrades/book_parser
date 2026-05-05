# /// script
# requires-python = ">=3.10"
# dependencies = [
#     "chatterbox-tts",
#     "torch",
#     "torchaudio",
# ]
# ///
"""Chatterbox TTS. Reads text from argv[1], writes WAV to argv[2].

Splits text into paragraph-sized chunks because Chatterbox has a token limit
per generation, and concatenates the resulting waveforms."""

import sys
import re
import torch
import torchaudio as ta
from chatterbox.tts import ChatterboxTTS

input_path = sys.argv[1]
output_path = sys.argv[2]

with open(input_path, "r", encoding="utf-8") as f:
    text = f.read()

if not text.strip():
    print("error: input file is empty", file=sys.stderr)
    sys.exit(1)

# Split on blank lines, falling back to sentences for very long paragraphs.
def chunks(t: str, soft_limit: int = 600):
    paragraphs = [p.strip() for p in re.split(r"\n\s*\n", t) if p.strip()]
    for p in paragraphs:
        if len(p) <= soft_limit:
            yield p
            continue
        sentences = re.split(r"(?<=[.!?])\s+", p)
        buf = ""
        for s in sentences:
            if len(buf) + len(s) + 1 > soft_limit and buf:
                yield buf
                buf = s
            else:
                buf = f"{buf} {s}".strip()
        if buf:
            yield buf

device = "cuda" if torch.cuda.is_available() else "cpu"
print(f"chatterbox: loading model on {device}", file=sys.stderr)
model = ChatterboxTTS.from_pretrained(device=device)

waves = []
for i, chunk in enumerate(chunks(text)):
    print(f"chatterbox: chunk {i + 1} ({len(chunk)} chars)", file=sys.stderr)
    waves.append(model.generate(chunk))

if not waves:
    print("error: no audio generated", file=sys.stderr)
    sys.exit(1)

combined = torch.cat(waves, dim=-1)
ta.save(output_path, combined, model.sr)
print(f"wrote {output_path} ({combined.shape[-1] / model.sr:.1f}s)")
