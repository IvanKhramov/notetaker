# План notetaker v1 (Windows first)

Формат плана по мотивам [obra/superpowers](https://github.com/obra/superpowers) (MIT).

Ресёрч: `research/01-granola-features.md`, `research/02-alternatives.md`, `research/03-rust-stack.md`.

## Принятые решения

- Open-source, MIT OR Apache-2.0. GitHub, релизы через Actions, без подписи.
- Чистый Rust, UI на egui. Редактор заметок: markdown + просмотр.
- Windows первым, но захват звука за трейтом, macOS и Linux добавляются реализацией.
- Пишем только онлайн-звонки: микрофон + системный звук, отдельными дорожками.
- Захват Windows: `wasapi`, per-process loopback звонилки, откат на endpoint loopback при тишине (Teams).
- ASR: sherpa-onnx, GigaAM v3 (русский, с пунктуацией) и Parakeet v3 (английский), Silero VAD.
  Модели в отдельном процессе-воркере (конфликт версий `ort` между крейтами).
- Транскрипт чанками по VAD во время звонка + чистовой проход после.
- Спикер в транскрипте это id. v1: `me` (микрофон) и `them` (loopback). Диаризация и голоса позже.
- LLM: OpenAI-совместимый API (OpenAI, OpenRouter, Ollama, vLLM) и Anthropic через HTTP.
  Режим `local-only` на встречу запрещает любые сетевые вызовы.
- Хранение: встреча = папка, файлы источник правды, SQLite (FTS5) только индекс, пересобирается.
- Аудио: opus, хранится 7 дней, настраивается (0 / N / навсегда).
- Старт: детект звонка + подсказка, горячая клавиша. Во время записи индикатор.

## Формат данных

```
<library>/
  2026-10-26_1530_<slug>/
    meta.yaml          # id (uuid v7), title, started_at, ended_at, app, mode (local|hybrid), template, audio_expires_at
    notes.md           # заметки пользователя
    enhanced.md        # результат LLM, редактируемый
    transcript.json    # {version, segments:[{id, speaker, channel, start_ms, end_ms, text, source: live|final, edited}]}
    audio/mic.opus
    audio/system.opus
<library>/.notetaker/index.sqlite   # не синхронизируется, пересобирается
```

`transcript.json` версионируется полем `version`, миграции в `core`.

## Workspace

- `crates/core` - модель, чтение/запись папок, индекс SQLite FTS5, ретеншн аудио.
- `crates/capture` - трейт `AudioSource`, реализации `windows` (wasapi), заглушка и файловый источник для тестов.
- `crates/asr-proto` - протокол воркера: JSON lines по stdin/stdout.
- `crates/asr-worker` - бинарник `notetaker-asr`: sherpa-onnx, VAD, модели.
- `crates/llm` - провайдеры, шаблоны, промпт расширения заметок.
- `crates/pipeline` - запись -> чанки -> воркер -> транскрипт -> чистовой проход -> LLM.
- `crates/cli` - `notetaker probe | record | transcribe | enhance | reindex`.
- `crates/app` - egui: список, поиск, встреча, настройки, трей, хоткей, индикатор, детект звонка.

## Вехи

Каждая веха: отдельная ветка `flant-<суть>`, коммиты, проходят `cargo fmt --check`,
`cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`.
Код, который компилируется только под Windows, проверяется `cargo check --target x86_64-pc-windows-msvc`
локально (если собирается без линкера) и обязательно в CI на `windows-latest`.

### M0. Каркас
- Workspace, лицензии, README, `.gitignore`, `rust-toolchain.toml` (stable).
- CI GitHub Actions: матрица windows/linux/macos, fmt, clippy, test.
- Приёмка: `cargo test --workspace` зелёный локально, workflow валиден (`actionlint`, если есть).

### M1. core: формат и индекс (Linux, параллельно с M2)
- Типы meta/transcript/segment, serde, версия схемы.
- Создание/чтение папки встречи, атомарная запись (tmp + rename).
- Индекс: rusqlite bundled, FTS5 по title/notes/enhanced/transcript, `reindex` с нуля, инкрементально по mtime.
- Ретеншн аудио по `audio_expires_at`.
- Приёмка: юнит-тесты на roundtrip, поиск, пересборку индекса после удаления sqlite.

### M2. ASR-воркер (Linux, параллельно с M1)
- Протокол: `load_model`, `transcribe {pcm_f32_16k | wav_path, channel}`, ответы `segments`, `error`.
- sherpa-onnx: GigaAM v3 (punct) и Parakeet v3, Silero VAD, выбор провайдера CPU/CUDA.
- Загрузка моделей в каталог данных по манифесту (url + sha256), без моделей в репе.
- Приёмка: интеграционный тест на коротком wav (тест помечен `#[ignore]`, если модели не скачаны),
  `notetaker transcribe file.wav` печатает сегменты.

### M3. Захват на Windows (параллельно с M1/M2)
- `AudioSource`: микрофон (wasapi capture), system loopback, application loopback по PID с откатом на endpoint при тишине N секунд.
- Ресемплинг в 16 kHz mono f32 (rubato), запись каждой дорожки в opus.
- `notetaker probe`: список устройств и процессов со звуком, 10 секунд записи, уровни по дорожкам, отчёт.
- Приёмка: CI windows собирается; живую проверку делает пользователь по инструкции `docs/probe.md`
  (Zoom, Teams, браузер; с наушниками и без).

### M4. Pipeline и CLI record (после M1-M3)
- `notetaker record`: две дорожки -> VAD-чанки -> воркер -> `transcript.json` (source=live) по ходу.
- По стопу: чистовой проход по полным дорожкам, замена live-сегментов на final.
- Приёмка: на Linux тест с файловым источником вместо захвата; на Windows ручная проверка.

### M5. LLM
- Провайдеры: OpenAI-совместимый (async-openai), Anthropic (reqwest, messages API).
- Шаблоны в `<library>/.templates/*.md` + встроенные (общий, 1:1, клиентский созвон, премит).
- Расширение: notes.md + transcript + meta -> enhanced.md.
- `local-only`: провайдер обязан быть локальным (Ollama/llama-server на localhost), иначе ошибка.
- Приёмка: тест на мок-сервере, проверка запрета сети в local-only.

### M6. UI egui
- Список встреч, поиск (FTS), вид встречи: заметки (markdown + просмотр), enhanced, транскрипт с правкой сегментов.
- Запись: кнопка, хоткей, индикатор поверх окон, трей.
- Настройки: библиотека, модели, LLM, ретеншн, режим по умолчанию.
- Приёмка: собирается во всех ОС в CI, ручная проверка на Windows.

### M7. Детект звонка
- Windows: процесс, держащий микрофон (CapabilityAccessManager ConsentStore), + список известных звонилок.
- Уведомление «Записать?»; автостоп по 15 минутам тишины.
- Приёмка: юнит-тесты на разбор, ручная проверка.

### M8. Релиз
- Пакет для Windows (zip/msi), загрузка моделей при первом старте, release workflow по тегу.

## Позже
- v1.1: чат по встречам (sqlite-vec), MCP (rmcp, read-only stdio).
- v1.2: диаризация дорожки `them` (sherpa-onnx), переименование спикеров.
- v1.3: библиотека голосов (опционально, биометрия, только локально).
- macOS (Core Audio taps), Linux (PipeWire), календарь, синхронизация папки через Диск.

## Откат
Каждая веха в своей ветке, мерж fast-forward в main после проверки. Откат: revert мерж-коммита.
Формат данных версионирован; ломающее изменение схемы только с миграцией в `core`.
