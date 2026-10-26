# ASR worker

The worker speaks versioned JSON lines on stdin/stdout; diagnostics go to stderr.
`notetaker-asr-proto` documents the types and provides `AsrClient` with process
ownership, response timeouts and crash detection. PCM input is base64 of
little-endian f32 samples at 16 kHz mono. `channel` is a caller-owned label;
response IDs associate segments with the original channel. `offset_ms` shifts
all segment timestamps. Requests are sequential; failed model loads unload the
previous model to keep memory use bounded. A worker/client pair handles one
request at a time.

CPU (two ASR threads, one VAD thread) is the default; `auto` currently selects CPU.
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
