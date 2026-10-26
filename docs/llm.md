# Расширение заметок через LLM

```sh
notetaker enhance /path/to/library/2026-10-26_1530_planning
notetaker enhance /path/to/meeting --template client-call --provider cloud
```

Команда читает `meta.yaml`, `notes.md` и `transcript.json`, печатает Markdown
и атомарно заменяет `enhanced.md`. При ошибке прежний файл сохраняется.
Шаблон выбирается из `--template`, затем `meta.template`, затем `general`.
Каталог библиотеки — родитель папки встречи. ASR и аудио не используются.

## Конфигурация

Файл `ProjectDirs::from("", "", "notetaker").config_dir()/config.toml`:

- Linux: `$XDG_CONFIG_HOME/notetaker/config.toml` или `~/.config/notetaker/config.toml`;
- Windows: `%APPDATA%\notetaker\config\config.toml`;
- macOS: `~/Library/Application Support/notetaker/config.toml`.

Создай файл вручную; ключи в него не записываются. `api_key_env` содержит
**имя** переменной окружения с ключом. Для локального сервера без авторизации
поле можно опустить. Указанная переменная должна существовать и быть непустой.

```toml
[defaults]
local = "ollama"
hybrid = "cloud"

[[providers]]
id = "ollama"
kind = "openai"
base_url = "http://localhost:11434/v1"
model = "qwen3:8b"
context_tokens = 8192

[[providers]]
id = "cloud"
kind = "openai"
base_url = "https://api.openai.com/v1"
model = "<OPENAI_MODEL>"
api_key_env = "OPENAI_API_KEY"
context_tokens = 32768

[[providers]]
id = "claude"
kind = "anthropic"
base_url = "https://api.anthropic.com/v1"
model = "<ANTHROPIC_MODEL>"
api_key_env = "ANTHROPIC_API_KEY"
context_tokens = 32768
```

Подставь доступные модели и их фактический размер контекста. Для Ollama также
настрой соответствующий контекст на стороне сервера: `context_tokens` ограничивает
клиентские запросы, но не меняет настройки модели. Обе записи в `[defaults]`
обязательны; они могут ссылаться на один локальный провайдер.
`--provider ID` переопределяет выбор для одной команды, сохраняя проверку режима.

`base_url` — корень API, **без** `/chat/completions` или `/messages`:

- OpenAI: `https://api.openai.com/v1`;
- OpenRouter: `https://openrouter.ai/api/v1`;
- Ollama: `http://localhost:11434/v1`;
- llama-server / vLLM: например `http://127.0.0.1:8080/v1`;
- Anthropic: `https://api.anthropic.com/v1`.

OpenAI-совместимый провайдер отправляет Chat Completions с `stream=true`,
читает SSE и принимает обычный JSON, если сервер игнорирует стриминг.
Anthropic использует Messages API, `anthropic-version: 2023-06-01` и `x-api-key`;
ответ целиком передаётся колбэку одним фрагментом. HTTP синхронный: в UI вызывай
его вне основного потока. Автоматических повторов запросов нет.
Тела HTTP-ошибок и значения ключей не включаются в диагностику.

## Local-only

Для `mode: local` URL проверяется **до** чтения ключа и создания клиента.
Разрешены только `localhost` и loopback IP (`127.0.0.1`, `::1`). Удалённый
провайдер отклоняется, в том числе при явном `--provider`.
У loopback-клиента отключены HTTP-прокси, `localhost` привязан к loopback IP.
Редиректы отключены у всех провайдеров. URL с credentials, query или fragment
не принимаются. Для `hybrid` удалённые провайдеры разрешены.

## Шаблоны

Встроены `general`, `one-on-one`, `client-call`, `standup`. Инструкции на
английском, результат — на языке встречи. Пользовательский шаблон
`<library>/.templates/<name>.md` имеет приоритет над встроенным с тем же именем:

```markdown
---
name: Release review
description: Decisions and follow-ups for a release
---
Preserve the user's agenda. Expand it with evidence from the meeting.
Use sections: Release scope, Decisions, Risks, Action items.
Include owners and deadlines only when stated.
Write the notes in the language of the meeting.
```

Применение: `--template release-review`, если файл называется
`release-review.md`. `name` и `description` — отображаемые метаданные,
тело — инструкция. Имя файла: латинские буквы, цифры, `-`, `_`.

## Промпт и длинные встречи

Заметки пользователя — каркас и приоритет: каждый пункт нужно сохранить,
развернуть по транскрипту, исправить опечатки и сокращения. Промпт запрещает
выдумывать факты, решения, владельцев и сроки; отсутствие пункта в транскрипте
не является поводом его удалить. Добавляются заголовок и дата встречи,
транскрипт с `[mm:ss]` и speaker ID (`me` / `them` в v1), инструкция шаблона.
Входные данные отделены от системных инструкций.

Оценка: около 4 Unicode-символов на токен с запасом (используем 3), плюс
64 токена на служебную обвязку. На ответ резервируется четверть контекста,
не более 2048 токенов. Токенизация приблизительная, особенно для не-латиницы.
Если провайдер отклоняет контекст, уменьши `context_tokens`.

Если транскрипт не помещается, он делится по строкам и границам Unicode;
для длинного сегмента метка времени и спикер повторяются. Каждый кусок
сжимается с исходными заметками, затем конспекты используются для итогового
свода по шаблону. Если конспекты тоже велики, добавляются уровни reduce.
Заметки, метаданные и шаблон передаются на каждом шаге. Финальные фрагменты
вызывают колбэк; промежуточные конспекты остаются внутри крейта.

Не более 8 проходов сжатия; отсутствие сокращения даёт ошибку. Если сами
заметки и шаблон не помещаются, команда просит увеличить контекст или
сократить их, не обрезая молча. Пустые ответы, обрыв SSE и достижение лимита
ответа считаются ошибкой. Map-reduce требует нескольких последовательных
запросов и может терять детали при сжатии.

## API и проверка

Крейт `notetaker-llm`: `Config::{load,from_path,parse,select}`,
`Template::{load,builtin}`, `HttpProvider::new(config, mode)`,
трейт `Provider::{config,complete}` и `CompletionRequest` с ограничением ответа.
`enhance` возвращает Markdown; `enhance_meeting` дополнительно читает файлы
через core и атомарно пишет результат. Оба принимают `&mut dyn FnMut(&str)`.
При ошибке колбэк мог получить часть ответа: сохранять его нужно только
после успешного завершения.

Тесты используют локальный `TcpListener`, фиктивные ключи и провайдеры:
SSE/JSON, Anthropic, HTTP 4xx/5xx, local-only, редиректы, шаблоны,
контекст и map-reduce. Живые API и модели для тестов не требуются.

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -j 2 -- -D warnings
cargo test --workspace -j 2
```
