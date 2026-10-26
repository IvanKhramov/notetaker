# Из чего на Rust собрать десктопный AI-блокнот для встреч (аналог Granola без бота), Windows first, затем macOS и Linux

**Версии:** cpal 0.18.2 (2026-08-16), wasapi 0.26.0 (2026-10-11), windows 0.62.2 (2025-10-06), rubato 5.0.1 (2026-10-01), webrtc-audio-processing 2.1.0 (2026-05-13), aec3 0.4.0 (2026-09-16), cidre 0.29.0 (2026-09-10), screencapturekit 11.1.0 (2026-10-23), pipewire 0.10.1 (2026-08-19), sherpa-onnx 2.0.0 (crate 2026-10-23, релиз репозитория v2.0.1 от 2026-10-26), whisper-rs 0.16.0 (2026-03-12), ort 2.0.0-rc.15 (2026-10-19), llama-cpp-2 0.1.160 (2026-10-15), mistralrs 0.8.1 (2026-04-02), ollama-rs 0.3.6 (2026-07-24), async-openai 0.42.2 (2026-10-10), tauri 2.12.5 (2026-10-21, есть 3.0.0-alpha.5), tauri-plugin-updater 2.14.0 / global-shortcut 2.4.1 (2026-10-14), egui 0.37.0 (2026-10-15), iced 0.14.0 (2025-12-07), slint 1.18.1 (2026-09-21), rusqlite 0.41.0 (2026-10-24), sqlx 0.9.0 (2026-05-21), tantivy 0.27.0 (2026-10-17), sqlite-vec 0.1.9 (стабильная, alpha 0.1.10-alpha.4 от 2026-05-18), lancedb 0.40.0 (2026-10-07), rmcp 3.7.0 (2026-10-22). ОС: Windows 10/11, macOS 14.2+, Linux с PipeWire.
**Дата:** 2026-10-26

Версии и даты сняты с crates.io API на дату документа.

## Короткий ответ

- Каркас: Tauri 2 (стабильная ветка 2.x) + Rust workspace. Два живых open-source аналога Granola на Rust (Anarlog, бывший Hyprnote, и Meetily) оба на Tauri 2.
- Звук на Windows: crate `wasapi` (системный loopback + per-process loopback + AEC-контроль), микрофон через него же или cpal. Две дорожки пишутся раздельно: микрофон = «я», loopback = «они». Ресемплинг в 16 кГц через rubato.
- ASR локально: `sherpa-onnx` (официальный Rust API) с GigaAM v3 (MIT, русский, есть готовые ONNX в sherpa-onnx, в т.ч. с пунктуацией) для постобработки и T-one для живого потока. Для английских кусков и смешанной речи запасной путь whisper-rs (Vulkan или CUDA) с large-v3-turbo.
- Диаризация: каналы как базовый слой, поверх loopback-канала `sherpa-onnx` offline diarization (pyannote segmentation 3.0 + embedding-модель).
- LLM: облако (Claude через сырой HTTP, официального Rust SDK нет) и локально через OpenAI-совместимый API (Ollama / llama-server) или llama-cpp-2 в процессе. Хранение: SQLite (rusqlite) + FTS5, поиск с русской морфологией через tantivy, векторы sqlite-vec. MCP: rmcp, stdio-сервер.

## Разбор

### 1. Захват звука на Windows

**Системный loopback.** WASAPI открывает capture-поток на render-устройстве с флагом `AUDCLNT_STREAMFLAGS_LOOPBACK`. Работает только в shared mode. Событийный режим для loopback поддержан с Windows 10 1703, раньше нужен был обход через render-поток ([MS Learn, Loopback Recording](https://learn.microsoft.com/en-us/windows/win32/coreaudio/loopback-recording)). Аппаратный «Stereo Mix» не нужен: «WASAPI supports loopback recording regardless of whether the audio hardware contains a loopback device» (там же).

**cpal.** Loopback на WASAPI есть: если открыть output-устройство как input, cpal ставит `AUDCLNT_STREAMFLAGS_LOOPBACK` (исходники 0.18.2, `src/host/wasapi/mod.rs` и `device.rs`). В CHANGELOG это не отражено, явно упомянут только loopback на macOS: добавлен в 0.17.0 (2025-12-20) для macOS > 14.6, исправления в 0.18.0 ([cpal CHANGELOG](https://github.com/RustAudio/cpal/blob/master/CHANGELOG.md)). Per-process loopback в cpal 0.18.2 нет: в исходниках нет `PROCESS_LOOPBACK` (проверено grep по архиву тега v0.18.2).

**wasapi crate (HEnquist/wasapi-rs).** Умеет loopback и per-process loopback: `AudioClient::new_application_loopback_client(process_id, include_tree)` использует `AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK` и режимы include/exclude process tree ([исходник src/api.rs](https://github.com/HEnquist/wasapi-rs/blob/master/src/api.rs), примеры `loopback`, `record_application`, `aec` в [README](https://github.com/HEnquist/wasapi-rs)). Ограничения per-process режима из doc-комментария: только `Direction::Capture` и shared; `get_mixformat`, `get_device_period`, `get_current_padding`, `get_audioclock` не работают, `get_buffer_size` возвращает мусор. Значит формат надо задавать самому (пример в доке: float32, autoconvert). Требует Rust 1.85, edition 2024.

**Per-process loopback (Zoom/Teams отдельно).** Структура `AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS`: `TargetProcessId` + `ProcessLoopbackMode` (включить или исключить дерево процессов). Минимальный клиент по документации: **Windows 10 Build 20348**, не 2004/19041, как часто пишут ([MS Learn, структура](https://learn.microsoft.com/en-us/windows/win32/api/audioclientactivationparams/ns-audioclientactivationparams-audioclient_process_loopback_params); [пример Application Loopback](https://learn.microsoft.com/en-us/samples/microsoft/windows-classic-samples/applicationloopbackaudio-sample/) тоже требует 20348). Противоречие с исходной постановкой («2004+») фиксирую: по первоисточнику это 20348, на Windows 11 (сборки 22000+) условие выполнено автоматически. Если целевой процесс ничего не рендерит, приходит тишина (там же). Захват не привязан к конкретному устройству вывода (там же), это плюс: не надо переоткрывать поток при смене наушников.

**AEC (пользователь без наушников).** Варианты:
- Системный AEC Windows: `IAcousticEchoCancellationControl::SetEchoCancellationRenderEndpoint` задаёт render-эндпоинт как reference. Минимум Windows Build 22621, и работает только если на capture-эндпоинте есть AEC-эффект с поддержкой этого контроля, иначе `E_NOINTERFACE` ([MS Learn](https://learn.microsoft.com/en-us/windows/win32/api/audioclient/nn-audioclient-iacousticechocancellationcontrol)). В wasapi crate есть `get_aec_control()`, `is_aec_supported()` и пример `aec` (AEC через категорию потока communications).
- `webrtc-audio-processing` 2.1.0: обёртка над WebRTC APM (AEC, NS, AGC, VAD). По умолчанию линкуется с системной библиотекой, фича `bundled` собирает из исходников, нужны clang/gcc, pkg-config, meson, ninja ([README](https://github.com/tonarino/webrtc-audio-processing)). Сборка под MSVC в README не описана, это риск.
- `aec3` 0.4.0: порт WebRTC AEC3 на чистый Rust, автор помечает API как work in progress ([README](https://github.com/RubyBit/aec3-rs)).
- Нейросетевой AEC на ONNX: так сделано в Anarlog (`crates/aec`, две ONNX-модели 128/256/512, realfft) ([исходники](https://github.com/fastrepl/anarlog/tree/main/crates/aec)). По структуре похоже на DTLN-aec, но в коде название модели не указано, это моя догадка.

**Ресемплинг и сведение.** rubato 5.x: асинхронные sinc-ресемплеры с изменяемым на лету коэффициентом (компенсация дрейфа часов двух устройств) и синхронные FFT-ресемплеры, без аллокаций в процессе обработки; с 5.0 ввод/вывод через `audioadapter` ([README](https://github.com/HEnquist/rubato)). Сводить потоки в один для ASR не обязательно: проще держать две моно-дорожки 16 кГц с общей временной шкалой. Как это сделано у аналога: Anarlog пишет мик и динамик отдельными потоками через `ringbuf`, на Windows для динамика использует `wasapi`, для микрофона cpal, есть модуль `audio-sync` и `joiner` ([crates/audio-actual](https://github.com/fastrepl/anarlog/tree/main/crates/audio-actual)). Его Windows-захват динамика следит за сменой default-эндпоинта (опрос раз в 2 с, переключение после двух подтверждений), берёт первый канал (`speaker/windows.rs`).

**Рекомендация.** Windows: `wasapi` для loopback (системный по умолчанию, per-process как опция при выбранном приложении), микрофон через `wasapi` или cpal. Альтернатива: cpal для всего (кросс-платформенный API, но без per-process). Риски: неизвестный формат в per-process режиме, тишина при отсутствии рендера, смена устройства посреди встречи.

### 2. macOS и Linux (чтобы архитектура сразу была кросс-платформенной)

**macOS.**
- Core Audio process taps: `AudioHardwareCreateProcessTap` доступен с macOS 14.2 ([Apple, JSON-метаданные API](https://developer.apple.com/documentation/coreaudio/audiohardwarecreateprocesstap(_:_:))). Tap подключается как вход aggregate-устройства; tap может выбирать процессы, делать mixdown и даже глушить вывод процесса. Нужен ключ `NSAudioCaptureUsageDescription` в Info.plist; при первом старте записи система спрашивает разрешение на запись системного звука ([Apple sample «Capturing system audio with Core Audio taps»](https://developer.apple.com/documentation/coreaudio/capturing-system-audio-with-core-audio-taps)). Публичного API для проверки/запроса этого разрешения нет, AudioCap использует приватный TCC ([insidegui/AudioCap](https://github.com/insidegui/AudioCap)).
- Противоречие по версиям: Apple API с 14.2; AudioCap таргетит 14.4; cpal требует для loopback 14.6+ ([cpal README](https://github.com/RustAudio/cpal)). В постановке было 14.4+. Безопасный минимум для своего кода на cpal: 14.6.
- ScreenCaptureKit: `SCStreamConfiguration.capturesAudio` с macOS 13.0, `captureMicrophone` с macOS 15.0 ([Apple docs, метаданные](https://developer.apple.com/documentation/screencapturekit/scstreamconfiguration/capturesaudio)). Rust: `screencapturekit` 11.1.0 или `cidre`. Требует разрешения на запись экрана (детали TCC не проверял).
- Аналоги: Anarlog на macOS использует `cidre` (фичи `core_audio`, `av`), Meetily тоже `cidre` ([Anarlog Cargo.toml](https://github.com/fastrepl/anarlog/blob/main/Cargo.toml), [Meetily Cargo.toml](https://github.com/Zackriya-Solutions/meetily/blob/main/frontend/src-tauri/Cargo.toml)).

**Linux.**
- PipeWire: свойство потока `stream.capture.sink` («Try to capture the sink output instead of source output») ([pipewire keys.h](https://gitlab.freedesktop.org/pipewire/pipewire/-/blob/master/src/pipewire/keys.h)). Узел выбирается через `target.object` ([pipewire-props(7)](https://docs.pipewire.org/page_man_pipewire-props_7.html)).
- `pipewire` crate 0.10.1: авторы предупреждают «work-in-progress. Expect frequent breakage», нужен clang для bindgen ([README](https://gitlab.freedesktop.org/pipewire/pipewire-rs)). cpal 0.18 имеет бэкенды PipeWire и PulseAudio ([cpal README](https://github.com/RustAudio/cpal)), но захват монитора синка через cpal я не проверял.
- Anarlog на Linux берёт `pipewire 0.9.2` + `libpulse-binding`.

**Рекомендация.** Трейт `AudioSource` с двумя реализациями на ОС (mic, system/app). Windows: wasapi. macOS: Core Audio taps через cidre (или cpal loopback). Linux: pipewire-rs. Не пытаться закрыть всё одним cpal.

### 3. Детект встречи

- **Микрофон занят другим процессом (Windows).** В Anarlog: перечисление аудиосессий на capture-устройствах через `IAudioSessionManager2`/enumerator (wasapi crate), фильтр `SessionState::Active`, PID в приложение через `sysinfo`, имена из реестра Uninstall (`winreg`). Опрос раз в секунду, события MicStarted/MicStopped ([detect/src/list/windows.rs](https://github.com/fastrepl/anarlog/blob/main/crates/detect/src/list/windows.rs), [detect/src/mic/windows.rs](https://github.com/fastrepl/anarlog/blob/main/crates/detect/src/mic/windows.rs)). Там же есть детект через UI Automation/Accessibility (`meeting_ax`, фича `Win32_UI_Accessibility`) для браузерных встреч.
- **Процессы Zoom/Teams/браузер.** Тот же PID из аудиосессии сразу даёт кандидата для per-process loopback. Для браузера (Meet, веб-Teams) PID будет у процесса браузера, и захват пойдёт по всему браузеру. Как ведёт себя новый Teams (WebView2) с include_tree, не нашёл, требует прототипа.
- **Календарь.** Microsoft Graph: `GET /me/calendarView?startDateTime=...&endDateTime=...`, минимальное право `Calendars.ReadBasic` ([MS Learn](https://learn.microsoft.com/en-us/graph/api/user-list-calendarview)). Google: `GET /calendar/v3/calendars/{calendarId}/events` со scope `calendar.readonly` или `calendar.events.readonly`, `singleEvents=true` раскрывает повторы ([Google docs](https://developers.google.com/workspace/calendar/api/v3/reference/events/list)). Поля ссылки на онлайн-встречу в ресурсах event не проверял.
- **Рекомендация.** Триггер «мик занят процессом X» → уведомление «записывать?» (без автостарта) + подсказка названия встречи из календаря. Календарь вторичен, детект по микрофону первичен.

### 4. ASR локально и в облаке

**Модели для русского, цифры из первоисточников.**

GigaAM (Сбер), лицензия MIT, Conformer 220-600M, экспорт в ONNX штатный (`to_onnx`, fp32 по умолчанию, для GPU советуют fp16). `.transcribe` только до 25 с, длинное аудио через внешний VAD ([README](https://github.com/salute-developers/GigaAM)). WER (%), выдержка из [evaluation.md](https://github.com/salute-developers/GigaAM/blob/main/evaluation.md):

| Набор | V3 RNNT | T-One + LM | Whisper* |
|---|---|---|---|
| Golos Farfield | 3.9 | 12.2 | 16.4 |
| Natural Speech | 6.9 | 14.5 | 13.4 |
| Callcenter | 9.5 | 13.5 | 23.1 |
| OpenSTT Youtube | 10.6 | 21.9 | 17.8 |
| Среднее по 10 наборам | 8.3 | 16.3 | 21.0 |

\* Whisper с постобработкой (без пунктуации, нормализация чисел). Какая именно версия Whisper в этой колонке, в таблице не указано; в README GigaAM в сравнении упоминается Whisper-large-v3. Среднее у V3 CTC 9.1, у E2E-вариантов с пунктуацией 12.0 (CTC) и 11.2 (RNNT).

T-one (T-Bank): потоковый CTC Conformer 71M, чанки 300 мс, только русский алфавит, домен телефония, Apache 2.0 ([README](https://github.com/voicekit-team/T-one)). Цифры авторов T-one расходятся с таблицей Сбера по одним и тем же наборам, это противоречие:

| Набор | T-one (свои) | GigaAM-RNNT v2 | Whisper large-v3 |
|---|---|---|---|
| Call-center | 8.63 | 10.22 | 19.39 |
| Named entities | 5.83 | 9.55 | 17.87 |
| CommonVoice 19 | 5.32 | 2.68 | 5.78 |

У T-one телефонные наборы свои, у Сбера свои; напрямую сравнивать нельзя. Для созвонов (широкополосный звук, не 8 кГц) обе таблицы не про наш домен, нужен свой тест.

Мультиязычная GigaAM (июнь 2026, 220M/600M, CTC): русский FLEURS 3.0 (600M) против 3.1 у Whisper large-v3, но английский FLEURS 9.4 против 3.9 у Whisper ([evaluation.md](https://github.com/salute-developers/GigaAM/blob/main/evaluation.md)). Для английских вставок хуже Whisper.

NVIDIA Parakeet TDT 0.6B v3: русский есть среди 25 языков, CC BY 4.0, коммерческое использование разрешено, пунктуация и таймстемпы; WER ru: FLEURS 5.51, CoVoST 3.00 ([HF model card](https://huggingface.co/nvidia/parakeet-tdt-0.6b-v3)). Есть готовая int8 сборка в sherpa-onnx.

Whisper turbo: 809M, оптимизированный large-v3 «with a minimal degradation in accuracy», не для перевода ([openai/whisper README](https://github.com/openai/whisper)).

**Рантаймы.**
- `sherpa-onnx` 2.0.0, официальный Rust API: offline и online ASR, VAD (Silero и TEN), speaker diarization, punctuation, denoise. По умолчанию статическая линковка, build script сам качает prebuilt-библиотеку с GitHub releases (переопределяется `SHERPA_ONNX_LIB_DIR` / `SHERPA_ONNX_ARCHIVE_DIR`) ([docs, src/lib.rs](https://docs.rs/sherpa-onnx)). В online-конфиге есть `t_one_ctc`, в offline `nemo_ctc`/`transducer`/`whisper`/`canary` и др. В релизе `asr-models` лежат готовые: `sherpa-onnx-nemo-transducer-giga-am-v3-russian-2025-12-16`, `...-punct-giga-am-v3-...` (CTC и transducer), `sherpa-onnx-streaming-t-one-russian-2025-09-08`, `sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8` ([GitHub release asr-models](https://github.com/k2-fsa/sherpa-onnx/releases/tag/asr-models)). Есть `hotwords_file`/`hotwords_score` в конфиге распознавателя (`src/offline_asr.rs`).
  - GPU на Windows: в релизе v2.0.1 есть сборки `win-x64-cuda` под CUDA 12.x и 13.x (onnxruntime 1.28.2). Провайдер `directml` в коде есть, но `SHERPA_ONNX_ENABLE_DIRECTML` по умолчанию OFF ([provider.cc](https://github.com/k2-fsa/sherpa-onnx/blob/master/sherpa-onnx/csrc/provider.cc), [CMakeLists.txt](https://github.com/k2-fsa/sherpa-onnx/blob/master/CMakeLists.txt)), готовых DirectML-сборок в списке ассетов не нашёл: собирать самим.
- `whisper-rs` 0.16.0 (whisper.cpp): фичи `cuda`, `vulkan`, `hipblas`, `intel-sycl`, `metal`, `coreml`, `openblas`, `openmp`; DirectML нет ([Cargo.toml крейта](https://docs.rs/crate/whisper-rs/0.16.0/source/Cargo.toml)). Сборка на Windows по BUILDING.md. Есть `set_initial_prompt` и `set_language` (подсказка терминов вроде Kubernetes, Helm, werf). Репозиторий переехал на Codeberg.
- `ort` 2.0.0-rc.15 (до сих пор RC): EP CUDA, TensorRT, DirectML, CoreML, WebGPU; при неудачной регистрации EP молча падает на CPU, если не вызвать `.error_on_failure()`; WebGPU EP экспериментальный ([ort docs](https://ort.pyke.io/perf/execution-providers)). DirectML у Microsoft в «sustained engineering», новые фичи ушли в WinML; ограничения: нет memory pattern, нет parallel execution, один поток на `Run` в сессии ([ONNX Runtime docs](https://onnxruntime.ai/docs/execution-providers/DirectML-ExecutionProvider.html)).
- Как у аналогов: Meetily на Windows `whisper-rs` + `ort` (`load-dynamic`) для Parakeet, `silero` для VAD, `nnnoiseless`, `rubato` ([Cargo.toml](https://github.com/Zackriya-Solutions/meetily/blob/main/frontend/src-tauri/Cargo.toml)).

**VAD.** Silero VAD: MIT, 8 и 16 кГц, чанк 30+ мс быстрее 1 мс на одном потоке CPU ([README](https://github.com/snakers4/silero-vad)). Брать встроенный в sherpa-onnx (пример `silero_vad_remove_silence.rs`), чтобы не тащить второй ONNX Runtime.

**Стриминг против постобработки.** Живой текст во время встречи: T-one через online API (настоящий стриминг) или VAD-сегменты + offline-модель («simulate streaming», такие примеры есть в sherpa-onnx для Parakeet/SenseVoice). Финальная расшифровка: после встречи прогнать обе дорожки через GigaAM v3 punct (или Whisper для английских сегментов). Как решать, какой сегмент английский, источники не дают; в sherpa-onnx есть `spoken_language_identification` (пример в lib.rs), его применимость к коротким вставкам надо мерить.

**Облако.**

| Сервис | Русский | Стриминг для ru | Источник |
|---|---|---|---|
| Deepgram | Nova-3, Nova-2, Flux multi | Nova-3 «batch or streaming», явно для ru не подтверждено | [docs](https://developers.deepgram.com/docs/models-languages-overview) |
| AssemblyAI | Universal-2 (prerecorded) | нет: streaming multilingual = en/es/de/fr/pt/it | [languages](https://www.assemblyai.com/docs/getting-started/supported-languages), [FAQ streaming](https://assemblyai.com/docs/faq/language-support-for-real-time-transcription) |
| OpenAI | язык не перечислен явно | файлы `stream=true`, live через Realtime | [docs](https://developers.openai.com/api/docs/guides/speech-to-text) |
| Yandex SpeechKit | да, по умолчанию | да, лимит 5 мин и 10 МБ на сессию, 1 канал | [модели](https://aistudio.yandex.ru/docs/ru/speechkit/stt/models), [streaming](https://aistudio.yandex.ru/docs/ru/speechkit/stt/streaming) |
| SaluteSpeech | ru-RU (+en-US и др.) | gRPC v2, многоканальность, speaker separation, hints | [gRPC v2](https://developers.sber.ru/docs/ru/salutespeech/api/grpc/recognition-stream-2) |

OpenAI: `whisper-1`, `gpt-4o-transcribe`, `gpt-4o-mini-transcribe`, `gpt-4o-transcribe-diarize` объявлены deprecated с отключением 26.02.2027, рекомендуемая `gpt-transcribe` (поддерживает `prompt`, `keywords`), файл до 25 МБ (там же). Yandex: часовую встречу в потоке придётся резать на сессии по 5 минут. SaluteSpeech: старый потоковый gRPC не поддерживается, рекомендован v2; чанк до 2 с, пауза между сообщениями до 5 с.

**Рекомендация.** Основной путь: sherpa-onnx + GigaAM v3 (punct) на CPU/CUDA, T-one для живого потока. Альтернатива: whisper-rs large-v3-turbo с Vulkan (единственный вариант с GPU на AMD/Intel без своей сборки ORT). Облако за трейтом, первым кандидатом для русского стриминга SaluteSpeech или Yandex. Риски: английские термины в чисто русских моделях (T-one физически не имеет латиницы в алфавите), качество на нашем домене не измерено никем.

### 5. Диаризация

- **Каналы.** Микрофон = пользователь, loopback = остальные. Бесплатно и точно, пока нет эха; без наушников голос собеседников попадает в микрофон, поэтому AEC из п.1 обязателен до ASR.
- **sherpa-onnx offline diarization.** Сегментация pyannote segmentation 3.0 в ONNX (`sherpa-onnx-pyannote-segmentation-3-0`, также `reverb-diarization-v1/v2`) + embedding-модель + кластеризация (`FastClusteringConfig { num_clusters }` или порог) ([release speaker-segmentation-models](https://github.com/k2-fsa/sherpa-onnx/releases/tag/speaker-segmentation-models), [пример Rust](https://github.com/k2-fsa/sherpa-onnx/blob/master/rust-api-examples/examples/offline_speaker_diarization.rs)). Работает по полному сигналу (`process(&[f32])`), то есть после встречи. Пример использует китайскую embedding-модель 3D-Speaker; какая лучше для русского, источники не говорят.
- **pyannote напрямую.** Python-пайплайн; готовый ONNX сегментатора берётся из sherpa-onnx. Anarlog ходит в облачный pyannote API (`crates/api-pyannote`).
- **Рекомендация.** Канальное разделение + offline diarization только на loopback-дорожке (сколько «их», кто из них говорил). Голосовые отпечатки (speaker embedding manager есть в sherpa-onnx) для «узнавания» коллег между встречами, позже.

### 6. LLM для саммари

- **Облако.** У Anthropic официальные SDK есть для Python, TypeScript, C#, Go, Java, PHP, Ruby, Rust среди них нет ([Anthropic SDKs](https://platform.claude.com/docs/en/api/client-sdks)). Из Rust: сырой HTTP через reqwest к Messages API. Текущие модели: `claude-opus-5-5`, `claude-sonnet-5-5`, `claude-haiku-5-5`, `claude-fable-5-1`, у всех контекст 1M токенов и до 128K вывода ([Models overview](https://platform.claude.com/docs/en/about-claude/models/overview)). Community-крейты (misanthropic 1.0.0-alpha.25, clust 0.9.0 от 2024-06-30) есть, но clust заброшен по дате; не проверял их полноту. OpenAI и OpenAI-совместимые: `async-openai` 0.42.2.
- **Локально.** Ollama/llama-server через OpenAI-совместимый HTTP (async-openai или `ollama-rs` 0.3.6) проще всего в поставке. В процессе: `llama-cpp-2` 0.1.160 с фичами `cuda`, `vulkan`, `metal`, `rocm`, `opencl`, `webgpu` ([Cargo.toml](https://github.com/utilityai/llama-cpp-rs/blob/main/llama-cpp-2/Cargo.toml)). `mistralrs` 0.8.1, последний релиз 2026-04-02, темп выпусков ниже.
- **Контекст на часовую встречу.** По Anthropic 1M токенов это около 555k слов на текущем токенайзере ([Models overview](https://platform.claude.com/docs/en/about-claude/models/overview)). Часовая встреча даже при плотной речи займёт на порядки меньше; точное число для русского текста надо мерить `count_tokens` на своих расшифровках, источника на токены русской речи не нашёл. Для локальных моделей ограничение задаёт `n_ctx` конкретной модели, при нехватке нужен map-reduce по сегментам.
- **Рекомендация.** Трейт `Summarizer` с двумя реализациями: Anthropic HTTP и OpenAI-совместимый (покрывает OpenAI, Ollama, llama-server, LM Studio). Встраивать llama.cpp в бинарник позже, если внешние раннеры неудобны.

### 7. UI и приложение

- **Что выбирают аналоги.** Anarlog: Tauri 2.11 + десятки своих плагинов (tray, overlay, detect, local-stt, mcp, tantivy, updater2) и официальные `global-shortcut`, `updater`, `single-instance`, `autostart`, `deep-link` ([Cargo.toml](https://github.com/fastrepl/anarlog/blob/main/Cargo.toml)). Meetily: Tauri 2.6 c фичей `tray-icon`, Next.js фронт ([Cargo.toml](https://github.com/Zackriya-Solutions/meetily/blob/main/frontend/src-tauri/Cargo.toml)). Оба MIT.
- **Tauri 2.** Трей в ядре под фичей `tray-icon`; на Linux события трея не приходят, работает только контекстное меню ([Tauri system tray](https://v2.tauri.app/learn/system-tray/)). Updater: подпись обновлений обязательна и не отключается, потеря приватного ключа = невозможность обновить установленные копии; на Windows артефакты NSIS и MSI, режим `passive` по умолчанию; статический JSON или динамический сервер; нужен Rust 1.90+ ([updater](https://v2.tauri.app/plugin/updater/)). Ветка 3.0 в alpha, стартовать на 2.x.
- **egui / iced / Slint.** egui 0.37 и iced 0.14 на чистом Rust, но готового трея, оверлея, апдейтера и редактора заметок там нет, придётся собирать отдельно (по аналогам: ни один не выбрал их). Slint 1.18: GPLv3, royalty-free для проприетарного десктопа при обязательном упоминании Slint (AboutSlint или бейдж), коммерческая лицензия ([LICENSE.md](https://github.com/slint-ui/slint/blob/master/LICENSE.md)).
- **Подпись Windows.** OV-сертификаты после 01.06.2023 по процедуре издателя; «EV and OV certificates now build SmartScreen reputation the same way»; Azure Artifact Signing (бывший Trusted Signing) через `signCommand` с `artifact-signing-cli`; кросс-сборка с Linux требует custom sign command ([Tauri Windows signing](https://v2.tauri.app/distribute/sign/windows/)).
- **macOS.** Developer ID Application + нотаризация обязательна для распространения вне App Store, бесплатный аккаунт не нотаризует, $99/год ([Tauri macOS signing](https://v2.tauri.app/distribute/sign/macos/)).
- **Рекомендация.** Tauri 2 + лёгкий фронт (редактор заметок с Markdown). Плавающее окно и хоткеи: окна Tauri + `tauri-plugin-global-shortcut`. Риски: WebView2 на Windows и разные WebView на ОС, размер JS-части.

### 8. Хранение и поиск

- **SQLite.** rusqlite 0.41 (синхронный, простой, бандлинг SQLite) или sqlx 0.9 (async). Anarlog на `sqlx =0.9.0-alpha.1`, Meetily на sqlx 0.8.
- **FTS5.** Встроенный porter-стеммер только для английского («designed for use with English language terms only»), unicode61 токенизирует без стемминга ([SQLite FTS5](https://www.sqlite.org/fts5.html)). Для русского: либо свой токенайзер (API custom tokenizers в FTS5 есть), либо trigram-токенайзер, либо tantivy.
- **tantivy 0.27.** Встроенный стеммер с `Language::Russian` ([stemmer.rs](https://github.com/quickwit-oss/tantivy/blob/main/src/tokenizer/stemmer.rs)). Anarlog держит отдельный `tauri-plugin-tantivy`.
- **Векторы.** sqlite-vec: «pre-v1, so expect breaking changes» ([README](https://github.com/asg017/sqlite-vec)), стабильная 0.1.9. LanceDB 0.40: отдельный формат и движок, для личного архива встреч тяжеловат (оценка, не из источника). Эмбеддинги можно считать тем же ORT.
- **Рекомендация.** rusqlite + FTS5 (trigram для начала) + sqlite-vec в одной БД; tantivy подключить, когда понадобится нормальная морфология.

### 9. Свой MCP-сервер

- `rmcp` 3.7.0 официальный Rust SDK MCP, tokio; реализует спецификацию `2026-07-28` и совместим с `2025-11-25`; транспорт stdio (`transport::stdio`), streamable HTTP (в т.ч. stateless), макросы `#[tool]`/`tool_router` ([README](https://github.com/modelcontextprotocol/rust-sdk)). Anarlog уже использует rmcp 3.2 (`crates/mcp`, `plugins/mcp`).
- **Рекомендация.** Отдельный бинарник `notetaker-mcp` (stdio) с read-only доступом к той же SQLite: tools `search_meetings`, `get_transcript`, `get_summary`, resources по встречам. Отдельный процесс проще подключать к Claude Desktop/Code и не держит UI.

### 10. Юридическое (без юр.советов, только где регулируется)

- Россия: тайна переписки, телефонных переговоров и иных сообщений, ст. 23 Конституции РФ ([текст](http://www.kremlin.ru/acts/constitution/item)); обработка персональных данных, 152-ФЗ ([consultant.ru](https://www.consultant.ru/document/cons_doc_LAW_61801/)).
- ЕС: основания обработки персональных данных, GDPR ст. 6 ([EUR-Lex](https://eur-lex.europa.eu/eli/reg/2016/679/oj)).
- США, федеральный уровень: запись законна, если записывающий участник разговора или один из участников дал согласие, 18 U.S.C. § 2511(2)(d) ([Cornell LII](https://www.law.cornell.edu/uscode/text/18/2511)); у штатов свои правила (не разбирал).
- Для продукта: явный индикатор записи и шаблон уведомления участников, решение за пользователем.

## Предлагаемая архитектура

```
notetaker/ (cargo workspace)
├─ crates/
│  ├─ audio-capture     трейт AudioSource { mic, system, app(pid) }
│  │    win: wasapi (loopback, process loopback, AEC control)
│  │    mac: cidre (Core Audio taps) | cpal loopback
│  │    linux: pipewire (stream.capture.sink)
│  ├─ audio-dsp         rubato -> 16 kHz mono; AEC (webrtc-audio-processing | aec3 | ONNX);
│  │                    ringbuf; запись дорожек (hound/WAV или FLAC)
│  ├─ detect            сессии мика (wasapi + sysinfo), список процессов, sleep/wake
│  ├─ calendar          Graph calendarView, Google events.list (reqwest + oauth2)
│  ├─ asr               трейт Transcriber { stream(), batch() }
│  │    local: sherpa-onnx (GigaAM v3 punct, T-one, Parakeet v3, Silero VAD)
│  │    alt:   whisper-rs (vulkan|cuda)
│  │    cloud: salute, yandex, deepgram, openai (за фичами)
│  ├─ diarize           канал -> спикер; sherpa-onnx diarization на loopback
│  ├─ llm               трейт Summarizer: anthropic (reqwest), openai-compatible (async-openai)
│  ├─ store             rusqlite + FTS5 + sqlite-vec; миграции; (tantivy позже)
│  └─ core              оркестрация сессии встречи: детект -> запись -> live ASR ->
│                       финальный ASR -> диаризация -> саммари -> индекс
├─ apps/
│  ├─ desktop           Tauri 2: трей, плавающее окно, хоткеи, updater, single-instance
│  └─ mcp               rmcp stdio-сервер, read-only к store
└─ models/              загрузчик моделей при первом запуске, не в инсталлятор
```

Поток данных: mic и system пишутся двумя файлами с общей временной меткой → AEC на mic с system как reference → VAD-сегменты → live ASR (T-one) в UI → после встречи batch ASR (GigaAM v3 punct) по обеим дорожкам → диаризация loopback → слияние в транскрипт с метками «я / спикер N» → LLM-саммари поверх заметок пользователя → FTS + эмбеддинги.

Модели качать при первом запуске (GigaAM, T-one, Silero и пр. в сумме сотни МБ, это из размеров архивов sherpa-onnx не проверял, оценка), GPU-варианты как опциональные пакеты.

## Проверено по источникам / требует прототипа

Проверено по документации и исходникам: наличие per-process loopback в wasapi crate и его ограничения; отсутствие per-process в cpal 0.18.2; минимальные сборки Windows (20348 для process loopback, 22621 для AEC-контроля); набор готовых ONNX-моделей GigaAM v3 / T-one / Parakeet v3 в sherpa-onnx; фичи GPU у whisper-rs и llama-cpp-2; отсутствие официального Rust SDK у Anthropic; стек аналогов (Anarlog, Meetily).

Требует сборки на Windows:
1. Per-process loopback для Zoom (`Zoom.exe`), нового Teams (`ms-teams.exe` + WebView2) и браузера: собрать пример `record_application` из wasapi-rs, запустить звонок, проверить, что звук есть с `include_tree=true` и какой формат реально приходит.
2. Системный loopback при тишине: идут ли пакеты, нужна ли подкладка тишины для синхронизации дорожек (пишем таймстемпы `QueryPerformanceCounter` из capture-клиента и сравниваем).
3. AEC без наушников: тот же звонок на колонках, сравнить WER на mic-дорожке без AEC, с `webrtc-audio-processing` (собирается ли `bundled` под MSVC), с `aec3`, с системным AEC (`is_aec_supported()` на конкретном ноутбуке).
4. sherpa-onnx на Windows: статическая сборка по умолчанию, затем `SHERPA_ONNX_LIB_DIR` на CUDA-сборку; RTF GigaAM v3 transducer на CPU и CUDA для часа аудио.
5. Качество ASR на своём домене: 3-5 записанных встреч с ручной разметкой 10-15 минут, WER для GigaAM v3, T-one, Parakeet v3, Whisper large-v3-turbo; отдельно доля правильно написанных терминов (Kubernetes, Helm, werf, namespace) и эффект hotwords / initial_prompt.
6. whisper-rs с `vulkan` на AMD/Intel iGPU: собирается ли, скорость против CPU.
7. Детект встречи: даёт ли перечисление аудиосессий PID Teams/браузера стабильно и без ложных срабатываний (голосовые ассистенты, диктовка).
8. Токены: `count_tokens` на расшифровке часовой русской встречи, чтобы понять стоимость саммари.

## Источники

- https://learn.microsoft.com/en-us/windows/win32/coreaudio/loopback-recording - loopback только shared, событийный режим с 1703, не нужен Stereo Mix
- https://learn.microsoft.com/en-us/windows/win32/api/audioclientactivationparams/ns-audioclientactivationparams-audioclient_process_loopback_params - поля, минимум Windows 10 Build 20348
- https://learn.microsoft.com/en-us/samples/microsoft/windows-classic-samples/applicationloopbackaudio-sample/ - include/exclude tree, тишина без рендера, требование 20348
- https://learn.microsoft.com/en-us/windows/win32/api/audioclient/nn-audioclient-iacousticechocancellationcontrol - AEC reference endpoint, Build 22621, E_NOINTERFACE
- https://github.com/RustAudio/cpal (README, CHANGELOG, исходники тега v0.18.2) - WASAPI loopback через output-как-input, macOS loopback 14.6+, бэкенды PipeWire/Pulse
- https://github.com/HEnquist/wasapi-rs - loopback, new_application_loopback_client и его ограничения, AEC, примеры
- https://github.com/HEnquist/rubato - асинхронный/синхронный ресемплинг, real-time safety
- https://github.com/tonarino/webrtc-audio-processing - APM-обёртка, фича bundled, зависимости сборки
- https://github.com/RubyBit/aec3-rs - порт AEC3 на Rust, статус WIP
- https://developer.apple.com/documentation/coreaudio/capturing-system-audio-with-core-audio-taps - taps, aggregate device, NSAudioCaptureUsageDescription, macOS 14.2
- https://developer.apple.com/documentation/coreaudio/audiohardwarecreateprocesstap(_:_:) - доступность macOS 14.2
- https://developer.apple.com/documentation/screencapturekit/scstreamconfiguration/capturesaudio - capturesAudio macOS 13.0, captureMicrophone 15.0
- https://github.com/insidegui/AudioCap - нет публичного API проверки разрешения, таргет 14.4
- https://gitlab.freedesktop.org/pipewire/pipewire/-/blob/master/src/pipewire/keys.h - stream.capture.sink
- https://docs.pipewire.org/page_man_pipewire-props_7.html - target.object
- https://gitlab.freedesktop.org/pipewire/pipewire-rs - статус WIP, требования
- https://github.com/fastrepl/anarlog - стек аналога: Tauri, wasapi/cidre/pipewire, ONNX AEC, детект мика, tantivy, rmcp, sqlx
- https://github.com/Zackriya-Solutions/meetily - стек аналога: Tauri, cpal, whisper-rs, ort (Parakeet), silero, rubato, sqlx
- https://learn.microsoft.com/en-us/graph/api/user-list-calendarview - calendarView, Calendars.ReadBasic
- https://developers.google.com/workspace/calendar/api/v3/reference/events/list - events.list, scopes, singleEvents
- https://github.com/salute-developers/GigaAM и evaluation.md - лицензия MIT, ONNX, лимит 25 с, таблицы WER
- https://github.com/voicekit-team/T-one - потоковая модель 71M, Apache 2.0, таблица WER
- https://huggingface.co/nvidia/parakeet-tdt-0.6b-v3 - русский, CC BY 4.0, WER ru
- https://github.com/openai/whisper - turbo 809M
- https://docs.rs/sherpa-onnx - Rust API, статическая линковка, примеры
- https://github.com/k2-fsa/sherpa-onnx/releases/tag/asr-models - готовые GigaAM v3, T-one, Parakeet v3
- https://github.com/k2-fsa/sherpa-onnx/releases/tag/speaker-segmentation-models - pyannote segmentation 3.0 ONNX
- https://github.com/k2-fsa/sherpa-onnx/blob/master/sherpa-onnx/csrc/provider.cc и CMakeLists.txt - провайдеры, DirectML выключен по умолчанию
- https://docs.rs/crate/whisper-rs/0.16.0 - фичи GPU, initial_prompt
- https://ort.pyke.io/perf/execution-providers - EP, тихий fallback на CPU
- https://onnxruntime.ai/docs/execution-providers/DirectML-ExecutionProvider.html - DirectML в sustained engineering, ограничения
- https://github.com/snakers4/silero-vad - MIT, 8/16 кГц, скорость
- https://developers.deepgram.com/docs/models-languages-overview - русский в Nova-3/Nova-2/Flux multi
- https://www.assemblyai.com/docs/getting-started/supported-languages и FAQ по streaming - русский только prerecorded
- https://developers.openai.com/api/docs/guides/speech-to-text - модели, deprecation 26.02.2027, 25 МБ
- https://aistudio.yandex.ru/docs/ru/speechkit/stt/models и /stt/streaming - языки, лимиты потока
- https://developers.sber.ru/docs/ru/salutespeech/api/grpc/recognition-stream-2 - gRPC v2, каналы, speaker separation, hints
- https://platform.claude.com/docs/en/api/client-sdks - список официальных SDK (без Rust)
- https://platform.claude.com/docs/en/about-claude/models/overview - ID моделей, контекст 1M, 555k слов
- https://github.com/utilityai/llama-cpp-rs - фичи llama-cpp-2
- https://v2.tauri.app/learn/system-tray/ - трей, ограничения Linux
- https://v2.tauri.app/plugin/updater/ - обязательная подпись, NSIS/MSI, режимы
- https://v2.tauri.app/distribute/sign/windows/ - OV/EV, SmartScreen, Artifact Signing
- https://v2.tauri.app/distribute/sign/macos/ - Developer ID, нотаризация
- https://github.com/slint-ui/slint/blob/master/LICENSE.md - три лицензии Slint
- https://www.sqlite.org/fts5.html - porter только для английского, custom tokenizers
- https://github.com/quickwit-oss/tantivy/blob/main/src/tokenizer/stemmer.rs - русский стеммер
- https://github.com/asg017/sqlite-vec - pre-v1
- https://github.com/modelcontextprotocol/rust-sdk - rmcp, спецификация 2026-07-28, stdio
- http://www.kremlin.ru/acts/constitution/item, https://www.consultant.ru/document/cons_doc_LAW_61801/, https://eur-lex.europa.eu/eli/reg/2016/679/oj, https://www.law.cornell.edu/uscode/text/18/2511 - где регулируется запись
- https://crates.io/api/v1/crates/<name>/versions - версии и даты релизов крейтов
