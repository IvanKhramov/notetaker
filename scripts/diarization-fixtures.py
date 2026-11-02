#!/usr/bin/env python3
"""Build deterministic, untracked call-like fixtures from LibriSpeech test-clean.

Source: https://www.openslr.org/12/ (Panayotov et al., CC BY 4.0).
Requires ffmpeg (with libopus). No Python packages. Audio stays under target/.
"""
import argparse
import array
import json
from pathlib import Path
import random
import subprocess
import tarfile
import urllib.request
import wave

RATE = 16000


def ffmpeg(*args):
    subprocess.run(["ffmpeg", "-v", "error", "-y", *map(str, args)], check=True)


def read(path):
    with wave.open(str(path)) as wav:
        assert (wav.getnchannels(), wav.getsampwidth(), wav.getframerate()) == (1, 2, RATE)
        return array.array("h", wav.readframes(wav.getnframes()))


def write(path, samples):
    with wave.open(str(path), "wb") as wav:
        wav.setparams((1, 2, RATE, 0, "NONE", "not compressed"))
        wav.writeframes(samples.tobytes())


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--dir", type=Path, default=Path("target/diarization-eval"))
    args = parser.parse_args()
    root = args.dir
    root.mkdir(parents=True, exist_ok=True)
    archive = root / "test-clean.tar.gz"
    if not archive.exists():
        urllib.request.urlretrieve("https://www.openslr.org/resources/12/test-clean.tar.gz", archive)
    speakers = ["1089", "121", "1221", "1320"]
    counts = dict.fromkeys(speakers, 0)
    sources = {s: array.array("h") for s in speakers}
    with tarfile.open(archive) as tar:
        for member in tar:
            parts = Path(member.name).parts
            if len(parts) != 5 or parts[2] not in sources or not member.name.endswith(".flac"):
                continue
            speaker = parts[2]
            if counts[speaker] >= 8:
                continue
            flac = root / Path(member.name).name
            if not flac.exists():
                flac.write_bytes(tar.extractfile(member).read())
            wav = flac.with_suffix(".wav")
            ffmpeg("-i", flac, "-ac", 1, "-ar", RATE, wav)
            sources[speaker].extend(read(wav))
            counts[speaker] += 1
    assert all(n == 8 for n in counts.values()), counts
    manifest = []
    for count, seconds, degraded in [(2, 45, False)] + [
        (n, duration, True) for duration in (45, 300) for n in (2, 3, 4)
    ]:
        name = f"{count}-{'call' if degraded else 'clean'}-{seconds}s"
        rng = random.Random(2026 + count)
        samples = array.array("h", [0]) * (seconds * RATE)
        truth = []
        cursor = RATE // 2
        turn = 0
        while cursor < len(samples) - RATE:
            speaker = turn % count
            short = turn % 4 == 3
            length = int(RATE * (rng.uniform(0.35, 0.8) if short else rng.uniform(2, 7)))
            length = min(length, len(samples) - cursor)
            source = sources[speakers[speaker]]
            offset = rng.randrange(max(1, len(source) - length))
            # Changing gain and short overlaps imitate different call conditions.
            gain = rng.choice([0.35, 0.7, 1.0]) if degraded else 0.7
            for i, value in enumerate(source[offset:offset + length]):
                samples[cursor + i] = max(-32768, min(32767, samples[cursor + i] + int(value * gain)))
            truth.append([cursor // 16, (cursor + length) // 16, speaker])
            cursor += length + (int(RATE * rng.uniform(-0.3, 0.3)) if degraded else RATE // 3)
            turn += 1
        raw = root / f"{name}-raw.wav"
        wav = root / f"{name}.wav"
        write(raw, samples)
        if degraded:
            opus = root / f"{name}.opus"
            ffmpeg("-i", raw, "-af", "lowpass=f=3400,highpass=f=200", "-ar", 8000,
                   "-c:a", "libopus", "-b:a", "12k", "-application", "voip", opus)
            ffmpeg("-i", opus, "-ar", RATE, "-ac", 1, wav)
        else:
            write(wav, samples)
        manifest.append({"name": name, "file": wav.name, "speakers": count, "truth": truth})
    (root / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
    print(root / "manifest.json")


if __name__ == "__main__":
    main()
