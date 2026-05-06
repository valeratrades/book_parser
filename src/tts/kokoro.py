# /// script
# requires-python = ">=3.10"
# dependencies = [
#     "kokoro>=0.9.4",
#     "soundfile",
#     "numpy",
# ]
# ///
"""Kokoro-82M TTS. Reads text from argv[1], writes WAV to argv[2]."""

import re
import sys
import numpy as np
import soundfile as sf
from kokoro import KPipeline

input_path = sys.argv[1]
output_path = sys.argv[2]

with open(input_path, "r", encoding="utf-8") as f:
    text = f.read()

if not text.strip():
    print("error: input file is empty", file=sys.stderr)
    sys.exit(1)

# Mirror KPipeline's default split (`\n+`) so we know the total upfront and can
# emit progress per segment.
segments = [s for s in re.split(r"\n+", text) if s.strip()]
total = len(segments)
print(f"PROGRESS 0/{total}", flush=True)

pipeline = KPipeline(lang_code="a")
chunks = []
for i, seg in enumerate(segments, 1):
    for _, _, audio in pipeline(seg, voice="af_heart"):
        chunks.append(audio)
    print(f"PROGRESS {i}/{total}", flush=True)

if not chunks:
    print("error: no audio generated", file=sys.stderr)
    sys.exit(1)

combined = np.concatenate(chunks)
sf.write(output_path, combined, 24000)
print(f"wrote {output_path} ({len(combined) / 24000:.1f}s)")
