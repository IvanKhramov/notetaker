# Какие есть аналоги Granola (open-source и коммерческие), как они устроены и что взять в свой аналог на Rust

**Версии:** состояние GitHub на 2026-10-26 (звёзды и `pushed_at` через `gh api`). Релизы: Meetily v0.4.1 (2026-09-12), anarlog desktop_v1.4.32 (2026-10-25), Minutes v0.30.10 (2026-10-22), Vibe v3.2.2 (2026-09-05), OpenWhispr v1.10.4 (2026-10-22), Amical v1.12.7 (2026-10-13), Prismical v0.3.23 (2026-09-30), Scriberr v1.2.0 (2025-12-17), Muesli v0.8.5 (2026-10-08), Humla v0.71.0 (2026-10-24), Note67 v0.1.28 (2026-09-19). Исходники читались с ветки по умолчанию (HEAD на 2026-10-26). Крейты: версии с crates.io на ту же дату. Коммерческие: публичные страницы цен и справки на 2026-10-26.
**Дата:** 2026-10-26

## Короткий ответ

- Ближе всего к задаче три Rust/Tauri-проекта под MIT: **Meetily** (Windows+macOS, whisper-rs + Parakeet через ort), **anarlog** (бывший Hyprnote, самый зрелый: AEC, детект встречи, локальная диаризация, MCP) и **Minutes** (CLI+MCP, markdown-файлы, есть Windows-сборки). Их исходники и стоит разбирать в первую очередь.
- Захват на Windows у всех один: WASAPI loopback (крейт `wasapi` или `cpal` с output-устройством). На macOS Core Audio process tap через `cidre`, на Linux PipeWire с откатом на PulseAudio.
- Слабое место у всех - диаризация: системный поток с несколькими удалёнными участниками часто схлопывается в одного спикера. В Rust для этого есть `speakrs`, `pyannote-rs` и `parakeet-rs` (Sortformer). Ловушка: несовместимые версии `ort` между крейтами.
- У anarlog локального STT на Windows нет, только на Apple Silicon. Это ниша для своего продукта.
- Коммерческие продукты отличаются в основном ботом (Otter, Fireflies, Read.ai, Fathom по умолчанию), CRM-интеграциями и ценой. Без бота работают Jamie, Krisp, Notion, Shadow (только Mac), у Fathom botless пока бета для Mac.

## Разбор

### Meetily (Zackriya-Solutions/meetily)

- **Лицензия, активность:** MIT, 32186 звёзд, последний push 2026-10-24, релиз v0.4.1 от 2026-09-12. Репо создан 2024-12-26.
- **Платформы:** готовые сборки для Windows x64 (нужен CPU с AVX2) и macOS arm64. Linux только сборка из исходников (README).
- **Стек:** Tauri 2 + Next.js, бэкенд на Rust (`frontend/src-tauri`). БД SQLite через `sqlx` с миграциями. Отдельный крейт `llama-helper`.
- **Захват звука:**
  - Windows: `cpal` с хостом WASAPI, output-устройства открываются в режиме loopback (`audio/devices/platform/windows.rs`). `cpal` пропатчен на git-ревизию.
  - macOS: Core Audio tap + aggregate device через `cidre` (`audio/capture/core_audio.rs`).
  - Linux: ALSA-хост `cpal`, ищет устройства с `monitor` в имени, то есть мониторы PulseAudio (`platform/linux.rs`).
  - Сведение mic+system с ducking и защитой от клиппинга, `ffmpeg-sidecar` для микса и кодирования, `nnnoiseless` для шумоподавления, `ebur128` для нормализации громкости.
- **ASR:** `whisper-rs` 0.13 (Metal/CoreML на Mac, Vulkan в Windows-инсталляторе, CUDA только при сборке из исходников) и Parakeet через `ort` (ONNX-конвертация istupakov). VAD: `silero_rs`.
- **Диаризация:** в CE нет. README обещает её в PRO. При этом описание репо говорит про "on-device speaker diarization". Это противоречие. В issue #230 (17 комментариев, открыт) мейнтейнер собирается подключить `speakrs`. Миграция `add_speaker_field.sql` уже есть.
- **LLM:** Ollama (рекомендуется), Claude, Groq, OpenRouter, любой OpenAI-совместимый endpoint.
- **Функции:** realtime-транскрипт, импорт аудио и перетранскрибация другой моделью (beta), шаблоны саммари (`summary/templates`), мультиязычные саммари (v0.4.0). Календарь, авто-детект встречи, чат по встречам, расширенный экспорт отнесены к PRO и частично "Coming Soon".
- **Известные проблемы:**
  - #456: на Windows whisper шёл только на CPU, 35 секунд на 4 секунды речи. В v0.4.0 добавили Vulkan.
  - #135: на Win11 "ничего не транскрибирует", на деле транскрибация была очень медленной.
  - #100: GPU не распознаётся на Windows 11.
  - #387: запрос на авто-детект встречи (открыт).

### anarlog, бывший Hyprnote (fastrepl/anarlog)

- **Лицензия, активность:** MIT для community-приложения, `enterprise/` под коммерческой лицензией. 9603 звезды, push 2026-10-26, релиз desktop_v1.4.32 от 2026-10-25. Issues на GitHub отключены (`has_issues: false`), поддержка идёт через Discord и Reddit.
- **Платформы:** macOS, Windows, Linux (README). Команда параллельно делает новый продукт char.com, anarlog остаётся open-source.
- **Стек:** Tauri v2, React+TypeScript, около 150 Rust-крейтов в `crates/` и около 50 Tauri-плагинов в `plugins/`. SQLite (`sqlx` 0.9-alpha) плюс файлы на диске. Полнотекстовый поиск через плагин `tantivy`. Есть CLI и MCP-сервер (`apps/cli`, `crates/mcp`), мобильное приложение в бете.
- **Захват звука** (`crates/audio-actual/src/speaker/`):
  - Windows: крейт `wasapi` 0.23, loopback на render-endpoint. Следит за сменой устройства по умолчанию и переоткрывает поток (`EndpointFollower`).
  - macOS: `cidre`, `TapDesc::with_mono_global_tap_excluding_processes` + aggregate device.
  - Linux: PipeWire, при ошибке откат на PulseAudio (`libpulse-binding`).
- **AEC:** крейт `aec` на двух ONNX-моделях (`model_*_1.onnx`, `model_*_2.onnx`, константы `BLOCK_SIZE`/`BLOCK_SHIFT`). По структуре похоже на DTLN-aec. Это моё предположение, в коде название модели не встретилось.
- **VAD:** Silero v6 через ONNX и `earshot` (чистый Rust).
- **ASR:** локально только на Apple Silicon: Parakeet Streaming/Batch (Soniqo, 25 европейских языков), Apple Speech на macOS 26, whisper.cpp для файлов. Для Windows и Linux встроенных локальных моделей в документации нет. Облако: Soniox 5. Расхождение: в репо есть крейты `whisper-local` и `transcribe-whisper-local`, но документация не даёт их на Windows.
- **Диаризация:** `crates/pyannote-local` (ONNX-сегментация + свои эмбеддинги + кластеризация, бинарь `diarization-eval` для метрик) и `crates/voiceprint` для узнавания голосов между встречами. Есть и облачный pyannote (`pyannote-cloud`). Уточнение спикеров на устройстве после встречи работает только в связке Parakeet Streaming + Batch.
- **LLM:** свой ключ, OpenAI-совместимые локальные серверы (Ollama, LM Studio, Unsloth), Apple Intelligence (эксперимент). Облачный Auto идёт в Claude Sonnet через OpenRouter.
- **Детект встречи** (`crates/detect`):
  - Windows: раз в секунду опрос активных аудиосессий на capture-устройстве (`IAudioSessionManager` через `wasapi`), PID сопоставляется с установленными приложениями из `Uninstall` в реестре.
  - Linux: libpulse + AT-SPI.
  - macOS: AX API и NSWorkspace.
  - Отдельно разбор accessibility-дерева (`meeting_ax`) для браузерных встреч и Slack huddle.
- **Прочее:** шаблоны в БД (миграции templates), календари Google, Outlook, Apple, экспорт Markdown, опциональный E2EE CloudSync.

### Minutes (silverstein/minutes)

- **Лицензия, активность:** MIT, 1572 звезды, push 2026-10-26, релиз v0.30.10 от 2026-10-22. Создан 2026-03-18, развивается очень быстро.
- **Платформы:** macOS (desktop + CLI), Windows (desktop setup.exe и CLI zip), Linux CLI (нужен glibc 2.39+). Ассеты релиза и `docs/install.md`.
- **Стек:** Rust workspace (`crates/core`, `cli`, `reader`, `whisper-guard` и др.), Tauri-приложение в `tauri/src-tauri`. Хранение: markdown с YAML frontmatter (action_items, decisions), плюс `rusqlite`. MCP-сервер в npm (`minutes-mcp`, 34 инструмента), около 60 CLI-команд, плагин для Claude Code.
- **Захват звука:** `cpal` для микрофона. Системный звук на macOS через `cidre` (core_audio, macOS 14.4) и ScreenCaptureKit. На Windows и Linux крейт `pocketstation` (Apache-2.0, "source-aware desktop audio Session SDK", 2 звезды): захват звука конкретного приложения отдельными стемами.
- **ASR:** `whisper-rs` (фича `whisper`), sherpa-onnx вынесен в отдельный процесс-плагин (`crates/sherpa-plugin`), Parakeet в разработке.
- **Диаризация:** `pyannote-rs` + `knf-rs` + `ort` (фича `diarize`). Для live-атрибуции отдельный sidecar `sidecars/diarize-streaming` на `parakeet-rs` с Sortformer. Sidecar вынесен в отдельный процесс и workspace, потому что `pyannote-rs` тянет `ort` rc.10 / `ndarray` 0.16, а `parakeet-rs` требует rc.12 / 0.17 (комментарий в Cargo.toml).
- **Функции:** запись встреч, звонков, голосовых заметок и диктовка; поиск по истории; авто-запись Teams и Zoom на macOS (opt-in); маркеры согласия на запись; импорт из Granola.
- **Известные проблемы:**
  - #169 (открыт): на многосторонних звонках системный стем даёт 2 спикера на 7 участников, удалённые схлопываются в один кластер.
  - #21: whisper зацикливался на итальянском (greedy, best_of=1), лечили VAD Silero из `ggml-org/whisper-vad`.
  - #531: сборка с CUDA падает на Windows + Pascal.
  - #940: перегрузка IOWorkLoop на macOS рвёт микрофон посреди записи.

### Vibe (thewh1teagle/vibe)

- **Лицензия, активность:** MIT, 7912 звёзд, push 2026-10-06, релиз v3.2.2 от 2026-09-05.
- **Платформы:** macOS, Windows, Linux.
- **Назначение:** транскрибатор файлов, микрофона и системного звука. Не блокнот встреч, но автор - тот же человек, что сделал `pyannote-rs` и `sherpa-rs`.
- **Стек:** Tauri 2. Системный звук через `cpal` из форка автора (ветка `feat/macos-system-audio-permission-check`): loopback по output-устройству, на macOS output по умолчанию. Инференс вынесен в отдельный `vibe-server` со своими крейтами на ggml: `whisper-rs`, `parakeet-rs`, `nemotron-rs`, `vad-rs`, `diarize-rs`. Диаризация на NVIDIA Sortformer, перенесена с ONNX Runtime на ggml/GGUF.
- **Функции:** Whisper, Nemotron 3.5, Parakeet TDT v3; экспорт SRT/VTT/TXT/HTML/PDF/JSON/DOCX; саммари через Claude API или Ollama; HTTP API со Swagger; CLI; запись с телефона по QR. Есть крейт `crates/meeting-detect`: на Windows `winreg` + список окон, на Linux X11.
- **Известные проблемы (в основном Windows):** вылеты при загрузке модели (#79, #175, #445): неподдерживаемые инструкции CPU, GPU-бэкенды, старые драйверы NVIDIA (#87 решён обновлением драйвера). SIGILL на CPU без AVX2 (#981). Автор добавил проверку CPU с понятной ошибкой (#79).

### OpenWhispr (OpenWhispr/openwhispr)

- **Лицензия, активность:** MIT, 10036 звёзд, push 2026-10-26, релиз v1.10.4 от 2026-10-22.
- **Платформы:** macOS (arm64, Intel), Windows, Linux (AppImage, deb, rpm).
- **Назначение:** в первую очередь диктовка (аналог Wispr Flow), но есть и заметки встреч.
- **Стек:** Electron 41, React 19, better-sqlite3, whisper.cpp, sherpa-onnx (Parakeet), llama.cpp.
- **Захват на Windows:** отдельный C-хелпер `windows-system-audio-helper.c` на WASAPI process loopback (Windows 10 2004+), исключает дерево своих процессов. Отдаёт 24 кГц mono s16le в stdout и JSON-события в stderr. Детект микрофона: `windows-mic-listener.c` по событиям WASAPI-сессий. Для Linux свой C-хелпер. AEC: нативный хелпер на WebRTC AEC3, по умолчанию выключен.
- **Функции:** авто-детект Zoom, Teams, FaceTime; live-диаризация и голосовые отпечатки; календари Google, Microsoft, Apple; семантический поиск; публичный API и MCP; облачная синхронизация и командные пространства.
- **Известные проблемы:**
  - #1265: на Windows 11 звук Teams-встречи не попадает в process loopback (версии 1.7.4+). В CLAUDE.md репо описан обход: хелпер шлёт `capture_silent`, если его поток тихий при активном render-endpoint, и тогда включается Chromium display-media loopback.
  - Диаризация на Intel Mac недоступна: ONNX Runtime 1.24 перестал выпускать бинарники для macOS x86_64 (README).
  - #102, #126: на Windows не находился whisper.cpp, падение с кодом 3221225501.

### Amical (amicalhq/amical)

- **Лицензия, активность:** MIT, 1571 звезда, push 2026-10-26, релиз v1.12.7 от 2026-10-13.
- **Платформы:** macOS и Windows. Linux нет, не хватает нативного хелпера.
- **Стек:** Electron, TypeScript, Whisper как N-API-аддон (сабмодуль whisper.cpp), Ollama.
- **Назначение:** диктовка с учётом активного приложения. Транскрибация встреч с захватом системного звука в README помечена как "Planned". Встречи вынесены в отдельный продукт той же команды, Prismical.

### Prismical (amicalhq/prismical)

- **Лицензия, активность:** MIT, 124 звезды, push 2026-10-26, релиз v0.3.23 от 2026-09-30.
- **Платформы:** macOS (arm64, Intel) и Windows x64.
- **Стек:** Electron + React + Effect, whisper.cpp через N-API-аддон, SQLite + Drizzle, редактор Tiptap.
- **Захват:**
  - macOS: Swift-хелпер (системный звук других участников с macOS 14.2).
  - Windows: C#-хелпер на .NET 8 + NAudio (`WasapiSource`).
  - AEC: WebRTC AEC3, собирается воспроизводимо (`packages/webrtc-aec3-builder`), при недоступности откат на простой reference reducer.
  - Детект микрофона на Windows: перебор `AudioSessionManager` capture-устройства, активные сессии сопоставляются с PID и процессом.
- **Функции:** realtime-транскрипт, AI-скиллы (Enhance, Cleanup, Ask, свои), детект встречи по микрофону, плавающий виджет, полнотекстовый поиск. Облачные (опционально): синхронизация, календарь, диаризация, MCP.

### Scriberr (rishikanthc/Scriberr)

- **Лицензия, активность:** MIT, 3114 звёзд, последний коммит 2026-09-20, релиз v1.2.0 от 2025-12-17.
- **Назначение:** self-hosted сервер транскрибации файлов, не десктопный блокнот.
- **Стек:** Go-бэкенд, React 19 + Vite, SQLite. Python-адаптеры для моделей: WhisperX, NVIDIA Parakeet/Canary, Voxtral, диаризация pyannote и Sortformer. Docker-образы под CPU, CUDA и отдельно под Blackwell.
- **Функции:** диаризация, чат с транскриптом через Ollama или OpenAI-совместимый API, API, folder watcher.
- **Проблемы:** сборка Docker (#45, 59 комментариев), трейсбеки WhisperX (#161), GTX 10-й серии (#273). Захват системного звука просят в #222 (открыт).

### Небольшие проекты, полезные как референс

- **Muesli** (Muesli-HQ/muesli): MIT, 1475 звёзд, только macOS 14.2+ на Apple Silicon. Swift: FluidAudio (Parakeet и диаризация), WhisperKit, MLX (Gemma), DTLN-aec на CoreML. Шаблоны, календарь, Q&A по встречам со ссылками на источники.
- **Humla** (michaelwilhelmsen/humla): MIT, 327 звёзд, только macOS. Tauri 2 + Rust + Swift, `whisper-rs` 0.16 с Metal, SQLite. Диаризация офлайн: Nemotron 3 (до 8 спикеров) или pyannote community-1. Провайдер STT выбирается по языку встречи. Локальный MCP-бинарь читает SQLite напрямую.
- **Note67** (ZapYap-com/note67): AGPL-3.0, 218 звёзд. Tauri 2, `cpal` + `wasapi` 0.16, `whisper-rs` 0.13, `rusqlite`. Разделяет только "Вы / Остальные" (на macOS), фильтрует галлюцинации whisper на тишине. Системного звука на Linux нет.
- **Clovy** (open-software-network/os-clovy): MIT, 356 звёзд. Tauri v2, macOS. Windows-сборка покрывает только оболочку, вход, запись микрофона и заметки.

### Коммерческие (только отличия от Granola)

- **Otter:** бот заходит в Zoom, Teams, Meet. Free: 300 мин/мес. Pro: $16.99/мес или $8.33 при оплате за год. Business: $30 или $19.99. Языки: английский, испанский, французский, немецкий, японский, китайский. MCP-сервер, Salesforce, HubSpot, Zapier, API.
- **Fireflies:** бот Notetaker; без бота можно через расширение Chrome. Основной язык английский, ещё 4 поддерживаются и 100+ в бете. Цены на странице не показаны.
- **Fathom:** бесплатный тариф с безлимитной записью. Premium $20/$16, Team $19/$15, Business $34/$25. По умолчанию бот; режим без бота пока бета и только для Mac. API, MCP, CRM.
- **tl;dv:** Pro €29/€18, Business €39/€29 за место. Главная обещает "NO BOT REQUIRED", страница цен этого не раскрывает. 30+ языков, CRM.
- **Jamie:** без бота ("No virtual participant"), Windows, macOS, iOS, офлайн-встречи, данные в EEA/CH/UK. Plus €21, Pro €39/мес при оплате за год, MCP на Pro.
- **Krisp:** без бота, работает на уровне аудио устройства; плюс шумоподавление и конвертация акцента. На устройстве транскрибирует только английский, остальные 15 языков на сервере. На той же странице встречается и цифра 17 языков, это противоречие.
- **Notion AI Meeting Notes:** без бота. Системный звук и микрофон только в desktop-приложении, в браузере только микрофон. Тарифы Business и Enterprise. Русский в списке языков есть, метки спикеров только для английского. Интеграция с Notion Calendar.
- **Read.ai:** бот в Zoom, Teams, Meet; desktop- и мобильные приложения пишут с устройства. Pro $15/$19.75. MCP, API.
- **Bluedot:** расширение Chrome, desktop и мобильные приложения. Basic $14/$18, Pro $20/$25. MCP, webhooks. Есть ли бот, страница не говорит.
- **Shadow (shadow.do):** только Mac, без бота. Данные о цене расходятся ($8/мес в одном источнике и $20 в другом), первоисточник не открывался.
- **Российские:** готового десктопного блокнота без бота на базе SaluteSpeech или SpeechKit не нашёл. Расшифровка и итоги встроены в сами платформы ВКС:
  - SaluteJazz: распознавание спикеров, итоги на GigaChat, включается автоматически при втором участнике, есть on-prem от 100 лицензий.
  - Яндекс Телемост: расшифровка и конспектирование для бизнес-пользователей, по новостям 2025 года.
  - Это не конкуренты по модели "без бота на любой платформе".

### Сводка open-source

| Проект | Стек | ОС | Лицензия |
|---|---|---|---|
| Meetily | Rust/Tauri | Win, mac (Linux из исходников) | MIT |
| anarlog | Rust/Tauri | Win, mac, Linux | MIT + ent |
| Minutes | Rust/Tauri+CLI | mac, Win, Linux CLI | MIT |
| Vibe | Rust/Tauri | Win, mac, Linux | MIT |
| OpenWhispr | Electron+C | Win, mac, Linux | MIT |
| Prismical | Electron+Swift/C# | Win, mac | MIT |
| Scriberr | Go+Python (сервер) | Docker | MIT |

Локальная диаризация по проектам:
- Meetily: нет, планируется через `speakrs`.
- anarlog: есть `pyannote-local` и voiceprint, но локальный STT только на Mac.
- Minutes: `pyannote-rs` и Sortformer в отдельном процессе.
- Vibe: Sortformer на ggml.
- OpenWhispr: есть, через sherpa-onnx.
- Prismical: только в облаке.
- Scriberr: pyannote и Sortformer на Python.

## Что взять себе

1. **Захват на Windows:**
   - Брать `wasapi` (HEnquist, 0.26), как anarlog, а не голый `cpal`. Понадобятся process loopback, исключение своего процесса и слежение за сменой устройства (`EndpointFollower` в `crates/audio-actual/src/speaker/windows.rs` у anarlog).
   - Сразу заложить обход проблемы из OpenWhispr #1265: Teams бывает не виден в process loopback. Нужна детекция "мой поток тихий, а endpoint играет" и откат на endpoint loopback (описано в CLAUDE.md OpenWhispr).
2. **Захват на macOS:** `cidre` с `TapDesc::with_mono_global_tap_excluding_processes` + aggregate device (anarlog, Meetily, Minutes). На Linux PipeWire с откатом на PulseAudio (anarlog `speaker/linux.rs`).
3. **Микрофон и системный звук раздельными стемами.** Хранить два отдельных потока, а не микс, как в Minutes и `pocketstation`. Тогда "я / остальные" размечается бесплатно, а диаризация нужна только на системном стеме (проблема Minutes #169).
4. **AEC обязателен для встреч без наушников:**
   - WebRTC AEC3 (Prismical и OpenWhispr собирают сами);
   - ONNX-модели, похожие на DTLN (anarlog `crates/aec`, Muesli `dtln-aec-coreml`).
5. **ASR:** whisper-rs (Vulkan для Windows-сборки, как у Meetily) плюс Parakeet TDT v3 через `parakeet-rs` или `ort`. Parakeet v3 поддерживает русский, лицензия CC-BY-4.0. Опционально sherpa-onnx (`sherpa-onnx` 2.0 - официальная Rust-обёртка). VAD: `earshot` (чистый Rust) или Silero. VAD перед whisper защищает от зацикливания и галлюцинаций (Minutes #21, Note67).
6. **Диаризация:** `speakrs` (Apache-2.0, pyannote community-1 на чистом Rust, CPU/CUDA/CoreML без ORT) или Sortformer через `parakeet-rs`. Плюс голосовые отпечатки между встречами (anarlog `voiceprint`, OpenWhispr).
7. **Конфликты `ort`:** держать одну версию `ort` на весь workspace, тяжёлые модели выносить в sidecar-процесс, как Minutes и Vibe. Заодно падение модели не роняет UI.
8. **Детект встречи на Windows:** опрос аудиосессий capture-устройства, сопоставление PID с приложением (anarlog `crates/detect`, Prismical mic-detector). Для браузерных встреч смотреть дерево UI Automation (anarlog `meeting_ax/windows.rs`).
9. **Хранение и агенты:** SQLite + tantivy для поиска (anarlog), экспорт в markdown с YAML frontmatter (Minutes). Локальный MCP прямо по базе без сети (Humla, Minutes), это дешёвая и сильная фича.
10. **Windows-боль с CPU и GPU:** проверка AVX2 при старте с понятной ошибкой (Vibe #79, Meetily требует AVX2). Vulkan по умолчанию, CUDA отдельной сборкой. Модели и GPU-бэкенд должны сообщать, что они реально используют (Meetily #456).
11. **Позиционирование:** локальный STT с русским на Windows не закрыт ни у anarlog (только Mac), ни у Prismical (диаризация в облаке). У Meetily нет диаризации в CE.

## Проверено по источникам / требует проверки руками

Проверено по исходникам и официальным страницам:
- Крейты захвата по ОС в Meetily, anarlog, Minutes, Vibe, Note67 (Cargo.toml и файлы в `speaker/`, `platform/`).
- Хелперы OpenWhispr и Prismical (CLAUDE.md, .csproj, .cs).
- Звёзды, лицензии, релизы через GitHub API на 2026-10-26.
- Языки Parakeet v3, локальные модели anarlog по платформам (docs.anarlog.so).
- Цены Otter, Fathom, tl;dv, Jamie, Read.ai, Bluedot. Справка Notion.

Требует проверки руками:
- Реально ли anarlog транскрибирует локально на Windows: в коде есть `whisper-local`, документация этого не обещает.
- Тип AEC-модели anarlog (DTLN - моё предположение).
- Как Minutes захватывает системный звук на Windows через `pocketstation` и насколько зрелый этот крейт (2 звезды).
- Качество `speakrs` и Sortformer на русском и на системном стеме с 5+ участниками. Цифры DER у `speakrs` - со слов автора, на VoxConverse.
- Цены Fireflies (на странице нет), Krisp, Shadow (источники расходятся), botless-режим Bluedot и tl;dv.
- Конспекты Телемоста: тарифы и доступность по первоисточнику Яндекса не проверены, только новости.
- Что у Meetily с диаризацией: описание репо и README противоречат друг другу.

## Источники

- https://github.com/Zackriya-Solutions/meetily - README, `frontend/src-tauri/Cargo.toml`, `audio/capture/*.rs`, `audio/devices/platform/*.rs`, релизы v0.3.0-v0.4.1, issues #100, #135, #230, #387, #456
- https://github.com/fastrepl/anarlog - README, корневой Cargo.toml, `crates/audio-actual/src/speaker/{windows,macos,linux}.rs`, `crates/aec`, `crates/vad`, `crates/pyannote-local`, `crates/detect`, `crates/whisper-local`, миграции `crates/db-app`
- https://docs.anarlog.so/models-and-providers - локальные модели только на Apple Silicon, LLM-провайдеры, диаризация
- https://github.com/silverstein/minutes - README, `crates/core/Cargo.toml`, `sidecars/diarize-streaming/Cargo.toml`, `crates/sherpa-plugin/Cargo.toml`, `docs/install.md`, ассеты v0.30.10, issues #21, #169, #531, #940
- https://github.com/pocketstation-io/pocketstation - README, назначение и лицензия
- https://github.com/thewh1teagle/vibe - README, `desktop/src-tauri/Cargo.toml`, `src/cmd/audio.rs`, `server/crates/diarize-rs/Cargo.toml`, `crates/meeting-detect`, issues #79, #87, #175, #981
- https://github.com/OpenWhispr/openwhispr - README, CLAUDE.md (хелперы WASAPI, AEC3, sherpa-onnx), workflow сборки хелпера, issues #102, #126, #1265
- https://github.com/amicalhq/amical - README: стек и статус фич
- https://github.com/amicalhq/prismical - README, `packages/native-helpers/*` (Swift, C#/NAudio, AEC3)
- https://github.com/rishikanthc/Scriberr - README, дерево `internal/transcription/adapters/py`, issues #45, #161, #222, #273
- https://github.com/Muesli-HQ/muesli - README, `native/MuesliNative/Package.swift`
- https://github.com/michaelwilhelmsen/humla - README, `src-tauri/Cargo.toml`
- https://github.com/ZapYap-com/note67 - README, `src-tauri/Cargo.toml`
- https://github.com/open-software-network/os-clovy - README: платформы
- https://github.com/avencera/speakrs - README: пайплайн community-1 на Rust, бэкенды
- https://github.com/altunenes/parakeet-rs - README: TDT 25 языков, Sortformer
- https://crates.io (API) - версии `wasapi`, `cidre`, `screencapturekit`, `whisper-rs`, `sherpa-onnx`, `sherpa-rs`, `pyannote-rs`, `parakeet-rs`, `speakrs`, `earshot`, `cpal`, `transcribe-rs`, `pocketstation`
- https://huggingface.co/nvidia/parakeet-tdt-0.6b-v3 - список языков (есть ru), CC-BY-4.0
- https://otter.ai/pricing - тарифы, бот, языки, интеграции
- https://fireflies.ai/pricing - бот, расширение Chrome, языки
- https://www.fathom.ai/pricing - тарифы, botless-бета для Mac
- https://tldv.io/pricing/ и https://tldv.io/ - цены, "NO BOT REQUIRED", языки
- https://www.meetjamie.ai/ и https://www.meetjamie.ai/pricing - без бота, платформы, EEA, цены
- https://krisp.ai/ai-meeting-assistant/ - без бота, языки (противоречие 15+1 и 17)
- https://www.notion.com/help/ai-meeting-notes - захват, тарифы, языки, хранение
- https://www.read.ai/pricing - тарифы, бот, MCP
- https://www.bluedothq.com/pricing - тарифы, интеграции
- https://theaiagentindex.com/agents/shadow и https://www.stork.ai/de/shadow - Shadow, наводка, цены расходятся
- https://developers.sber.ru/help/jazz/decoding - расшифровка и итоги в SaluteJazz
- https://www.comss.ru/page.php?id=18300 и https://www.comnews.ru/content/242089/2025-10-30/2025-w44/1010/yandeks-prevratit-telemost-universalnyy-instrument-dlya-obscheniya - Телемост, расшифровка и конспекты (новости)
