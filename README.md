# notetaker

An open-source AI notebook for online meetings, inspired by Granola and written
in Rust. Windows comes first, with macOS and Linux capture support planned.

**v0.7.0:** Fast speaker corrections with merge/undo and segment selection,
speaker timing statistics and a clickable timeline. Export transcripts as
Markdown, SRT, VTT, CSV or JSON from the app or CLI. Voice-linked renames can
update previous meetings. See [speaker editing](docs/speakers.md),
[calendar setup](docs/calendar.md), and [chat and MCP setup](docs/mcp.md).

## Задачи и подготовка встреч (0.8.0)

Экран «Задачи» собирает флажки из конспектов: фильтры «Мои»/ответственный,
сроки, группировка, переход к строке и сохранение статуса. В «Ближайшие события» доступен
бриф по предыдущим встречам и задачам участников; после конспекта можно
подготовить сообщение команде или клиенту. Имя и алиасы задаются в настройках.
CLI: `notetaker tasks --mine --open`; MCP: `list_tasks(status, owner)`.
Подробности и правила Local: [задачи и брифы](docs/tasks.md).

## Install

Download `notetaker-0.7.0-windows-x64.zip` from this repository's **Releases**
page and extract it into a folder. Keep all files together and run
`notetaker-app.exe`. The binaries are unsigned: Windows SmartScreen may warn;
choose **More info → Run anyway** if you trust the download. A SHA-256 checksum
is available beside the ZIP (`Get-FileHash <zip> -Algorithm SHA256`).
Windows 11 is recommended for capture of an individual application; older
Windows 10 builds fall back to all system audio (see the diagnostics guide).

## Quick start

1. On first launch, accept the GigaAM v3 download (~171 MB including Silero VAD),
   or choose Parakeet v3 in Settings for English and other supported languages.
2. Start a recording, select the call's audio session, and write notes. Windows
   also offers to record detected calls; accepting uses that process and your
   default model/mode. Enable browser detection in Settings for Meet, Толк, or
   Телемост in a browser (disabled by default).
3. Stop the recording and wait for the final transcript. A detected call ending
   offers to stop; silence stops recording after 15 minutes by default
   (Settings: minutes, 0 disables it).
4. Configure an LLM provider and generate a summary when ready.

See [audio capture diagnostics](docs/probe.md) and [LLM setup](docs/llm.md).
Models download separately. Audio stays on your computer; Hybrid mode can send
notes and transcript text to your chosen remote LLM.

## Build

Install [Rust via rustup](https://rustup.rs/). The toolchain file selects stable
Rust and installs rustfmt, Clippy, and the Windows MSVC target.
On Debian/Ubuntu, install the [eframe prerequisites](https://github.com/emilk/egui/blob/0.37.0/crates/eframe/README.md)
and Wayland build tools:

```sh
sudo apt-get install pkg-config libxcb-render0-dev libxcb-shape0-dev libxcb-xfixes0-dev libxkbcommon-dev libssl-dev libwayland-dev
```

```sh
cargo build --workspace -j 2
cargo run -p notetaker-app
cargo run -p notetaker-cli -- --help
cargo test --workspace -j 2
```

## Desktop application

Build `notetaker-app` and `notetaker-asr` together and keep the executables in the
same directory (or set `NOTETAKER_ASR_BIN` to the worker's full path):

```sh
cargo build -p notetaker-app -p notetaker-asr-worker -j 2
./target/debug/notetaker-app
```

Start a recording, choose a title, GigaAM v3 / Parakeet v3, Local / Hybrid,
and an audio session or all system audio. Write Markdown notes during the call;
notes save automatically. The transcript fills in live, then the final pass
runs after Stop. Segment edits are available after the final pass and are saved
atomically with `edited=true`. Use the Summary tab to select a built-in or
`<library>/.templates/*.md` template and stream enhanced notes. Failed generation
preserves the previous summary. Notes and summaries have edit/Markdown preview
modes. Search uses the core FTS index, refreshed at startup, on Refresh/search,
and after recording or enhancement.

Settings include the library (Documents/notetaker by default), default model and
mode, audio retention (0 / N days / forever), shortcut and LLM provider fields.
Download buttons run `notetaker-asr download` off the UI thread and show download
and installation stages for the model and Silero VAD. Missing workers/models
produce an actionable error with a link to Settings. App settings use
`ProjectDirs("", "", "notetaker").config_dir()/app.toml`; LLM settings use the
separate `config.toml` described in [LLM setup](docs/llm.md). Enter API key
environment variable names, never key values. Local meetings permit only
loopback LLM providers; Hybrid allows remote providers. Model downloads are an
explicit network operation in either mode. Audio retention applies to new
recordings; expired audio is swept after a recording finishes.

Windows has a tray menu, configurable global start/stop shortcut (Ctrl+Alt+R),
and a small always-on-top recording window with timer and Stop. The shortcut
starts immediately with the current recording options (defaults at startup);
use New Recording to choose another audio session. Closing the main window
hides it in the tray; double-click the icon to reopen, or use Exit to stop,
finish transcription and flush pending saves. Other platforms can browse/edit
libraries and enhance notes; live capture, tray and global shortcut are Windows
only. No GTK/AppIndicator dependencies are added on Linux. See the native
[tray-icon event-loop requirements](https://docs.rs/tray-icon/0.26.1/tray_icon/)
and [global-hotkey requirements](https://docs.rs/global-hotkey/0.9.0/global_hotkey/):
both integrations live on eframe's Win32 event-loop thread and wake egui through
`Context::request_repaint` when native events arrive.

The UI uses egui's embedded Ubuntu Light (Ubuntu Font Licence) and Hack
(MIT / Bitstream Vera) fonts. Both include Cyrillic; a headless test checks the
Russian alphabet including Ё/ё in proportional and monospace families. No extra
font asset is bundled. Their licenses ship with
[epaint_default_fonts](https://github.com/emilk/egui/tree/0.37.0/crates/epaint_default_fonts/fonts).
Markdown uses `egui_commonmark` without image loaders, network fetching, or
syntax-highlighting dependencies.

Windows manual acceptance: record a Zoom/Teams/browser session, type and switch
notes during capture, stop from the indicator/tray/shortcut, check final-pass
progress and Me/Them segments, edit and reopen a transcript, stream a local and
remote summary, verify shortcut conflict errors, hide/reopen/exit from the tray,
and confirm the indicator stays above the call without taking keyboard focus.
CI builds all three Windows binaries into `notetaker-windows-x64`.

## Transcript quality and import

The final pass streams WAV through Silero VAD with a roughly 30-second speech
limit and sample-based timestamps. Unedited microphone duplicates within
300 ms of system speech are dropped (best-effort live too). Jaccard must reach
0.6 and all mic words must occur in order on the system track; short replies,
additional words and manual edits survive. No schema change or energy gating.

Settings **Словарь** stores one term per line in `app.toml` (`vocabulary`).
`<library>/.notetaker/vocabulary.txt` merges terms and overrides matching names:

```text
Deckhouse
Kubernetes
Flant
fesco
werf = верф, вёрф
```

Exact aliases/case and unique single-edit matches (alphabetic tokens and terms
of at least five characters, same script) are corrected after ASR. Transliteration
requires an explicit alias. Summary prompts always receive spelling context.
Both shipped models are NeMo transducers, including GigaAM v3 punct (RNNT) and
Parakeet v3 (TDT). Nonempty vocabulary enables `modified_beam_search`, four paths,
score 1.5 and a temporary BPE vocabulary derived from tokens, following the
[upstream 2.0.0 example](https://github.com/k2-fsa/sherpa-onnx/blob/v2.0.0/nodejs-addon-examples/test_asr_non_streaming_nemo_parakeet_tdt_v2_hotwords.js).
This costs more CPU; only model-encodable spellings can be biased.

Use **Импорт записи** in the sidebar, paste a path or drop a file into the dialog,
then choose model/mode. Progress and the automatic-summary setting are reused.
CLI reads the same app defaults:

```sh
notetaker import meeting.mp4
notetaker import meeting.opus --library /path/to/library --model gigaam-v3 --no-summary
```

wav/mp3/m4a/ogg/opus/flac/mp4/webm/mkv use Symphonia and existing `opus-rs`.
Unsupported codecs, chained Ogg and Opus mappings other than mono/stereo family 0
fall back to ffmpeg on PATH, with an installation error if missing. Compressed
container padding differences may remain. Import creates a system/them meeting,
titled from filename and dated by mtime. Temporary PCM uses about 230 MB/hour
on disk and is cleaned on success/failure; RAM stays bounded by packets and
speech chunks. Opus follows retention settings (zero only after successful ASR).
Long ASR deadlines allow five minutes plus four times the recording duration;
Ctrl+C/exit cancellation is observed at decode and ASR progress boundaries.

## Speakers and voices

Enable **Разделять собеседников** (on by default) and explicitly download its
~47 MB models in Settings. The final system track and imported recordings get
speaker ids `s1`, `s2`, etc.; an optional **Участников** hint controls their count.
Click a speaker label to name them. Search and summaries use confirmed names;
renaming marks an existing summary stale. Missing models leave ASR usable.

**Голоса** is opt-in biometric storage. Name a speaker or click **Это я** to
remember their voice. New matches appear as **Name?** for confirmation; very
confident self matches in imports use `me`. Settings list and delete voices.
Exclude the entire local `<library>/.notetaker` directory from synchronization.
Embeddings stay there and never enter LLM prompts. See
[speakers and privacy](docs/speakers.md) for models, licenses and limitations.
Diarization reads the complete waveform into RAM, about 230 MB per audio hour
plus models, after releasing ASR weights.

## Local transcription

Build both binaries, then download the model (Silero VAD is installed with it):

```sh
cargo build -p notetaker-asr-worker -p notetaker-cli -j 2
./target/debug/notetaker-asr download gigaam-v3
./target/debug/notetaker-asr download parakeet-v3
./target/debug/notetaker transcribe input.wav
./target/debug/notetaker transcribe input.wav --model parakeet-v3
```

Input must be mono WAV at 8–192 kHz (integer PCM or float); the worker resamples
it to 16 kHz using sherpa. Output is one VAD segment
per line: `[00:01.234 - 00:03.456] text`. Timestamps describe speech chunks,
not individual words. GigaAM v3 with punctuation is the default Russian model;
Parakeet TDT v3 int8 supports English and 24 other European languages.
No audio leaves the machine; only the explicit `download` command uses the network.

Models go to `ProjectDirs("", "", "notetaker").data_local_dir()/models`:
`~/.local/share/notetaker/models` on Linux and
`%LOCALAPPDATA%\notetaker\data\models` on Windows. Override with
`NOTETAKER_MODELS_DIR`. Downloads are verified against pinned SHA-256 and size
before extraction, then installed atomically. Models and test WAVs are not in Git.
The CLI finds `notetaker-asr` alongside itself or on PATH; `NOTETAKER_ASR_BIN`
can select another executable.

See [worker documentation](crates/asr-worker/README.md) for the protocol, CUDA setup
and opt-in model integration tests.

## Third-party models

Weights have separate licenses from this application. Downloads are ONNX exports
from the [sherpa-onnx model releases](https://github.com/k2-fsa/sherpa-onnx/releases/tag/asr-models).

- `gigaam-v3`: Sber / Salute Developers, GigaAM v3 RNNT with punctuation,
  [MIT](https://github.com/salute-developers/GigaAM/blob/main/LICENSE),
  [upstream model](https://github.com/salute-developers/GigaAM); archive 170.2 MB.
- `parakeet-v3`: NVIDIA, Parakeet TDT 0.6B v3, quantized ONNX export,
  [CC-BY-4.0](https://creativecommons.org/licenses/by/4.0/),
  [original model and attribution](https://huggingface.co/nvidia/parakeet-tdt-0.6b-v3);
  archive 487.2 MB. Preserve NVIDIA attribution and indicate the ONNX/int8 conversion
  when redistributing these weights.
- `silero-vad`: Silero Team, [MIT](https://github.com/snakers4/silero-vad/blob/master/LICENSE),
  [upstream](https://github.com/snakers4/silero-vad); ONNX file 643.9 KB.

Licensed under either [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option.
