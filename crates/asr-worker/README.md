# ASR worker

The worker speaks versioned JSON lines on stdin/stdout; diagnostics go to stderr.
`notetaker-asr-proto` documents the types and provides `AsrClient` with process
ownership, response timeouts and crash detection. PCM input is base64 of
little-endian f32 samples at 16 kHz mono. `channel` is a caller-owned label;
response IDs associate segments with the original channel. `offset_ms` shifts
all segment timestamps. Requests are sequential; failed model loads unload the
previous model to keep memory use bounded. A worker/client pair handles one
request at a time.

CPU is the portable default. `num_threads: 0` (also the default when absent)
selects min(physical cores, 8); 1-8 overrides ASR, Language ID, VAD and diarization.
`model_id: "auto"` requires GigaAM, Parakeet, Whisper tiny and Silero VAD;
`download auto` installs them with pinned checksums. Auto routes each VAD chunk
by Whisper language ID, returns optional `lang`, and biases both ASR models with
hotwords. The protocol version remains 1; older segments omit `lang`.
`provider: "auto"` in this CPU worker resolves to CPU. Frontends can select a
separate optional GPU worker. DirectML requests fail explicitly in this build.
See [ASR details and GPU investigation](../../docs/asr.md).

`notetaker-asr bench input.wav --model auto --provider cpu --threads 0 --runs 3`
prints model loading time, pass duration and RTF (excluding load/diarization).
`check-provider cpu|directml` checks build capabilities without loading models.

For explicit `provider: "cuda"`, build with `--features cuda` and point
`SHERPA_ONNX_LIB_DIR` to a compatible CUDA-enabled sherpa-onnx **shared** native
library directory. This feature switches native linking to shared libraries;
it does not install CUDA. Distribute the native DLLs/shared libraries with the
worker, and install the matching CUDA/cuDNN runtime. Without that feature,
`cuda` requests return an error. sherpa may fall back to CPU if its native CUDA
provider cannot initialize; inspect stderr. GPU execution has not been tested.

Model integration tests are opt-in and serialize model use:

```sh
NOTETAKER_MODELS_DIR=/path/to/models cargo test -p notetaker-asr-worker --test real_models -j 2 -- --ignored --nocapture
```

`download diarization` installs pyannote 3.0 int8 + ERes2Net base, separately
from ASR. `check-model diarization` checks both without loading weights.
`diarize {id, wav_path, channel, speakers: null|1..32, embeddings: bool}` returns
`diarized {id, result: {turns: [{start_ms, end_ms, speaker}], embeddings: {id: [...]}}}`.
`embed {id, wav_path, ranges: [{start_ms, end_ms}]}` returns `embedded {id, embedding}`.
Both accept mono 16 kHz f32/i16 WAV; error replies preserve request ids.
These requests unload ASR weights; load an ASR model again before transcribing.
Speaker vectors are optional and never passed to the GUI inside transcripts.
See [speaker documentation](../../docs/speakers.md) for storage and licenses.

```sh
NOTETAKER_MODELS_DIR=/path/to/models cargo test -p notetaker-asr-worker --test diarization -- --ignored --nocapture
```
