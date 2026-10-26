# Какой полный функционал у Granola (granola.ai) на октябрь 2026, фича за фичей

**Версии:** Granola Help Center и API docs (docs.granola.ai, выгружены целиком по llms.txt 2026-10-26), публичный API v1.6.0 (changelog за октябрь 2026), страница цен granola.ai/pricing, changelog granola.ai/updates (последняя запись 02 Sep 2026). Живого приложения не ставили.
**Дата:** 2026-10-26

## Короткий ответ

Granola - десктопный (macOS 14+, Windows) AI-блокнот без бота: пишет микрофон и системный звук локально, стримит в облачный ASR (Deepgram, AssemblyAI) и показывает транскрипт в реальном времени, делит его на Me/Them, а имена берёт из UI Zoom/Meet/Teams через Accessibility или расширение Chrome [1][5][10].
После встречи LLM (в основном OpenAI и Anthropic) склеивает твои черновые заметки, транскрипт и событие календаря в "enhanced notes" по шаблону [13]. Сверху лежат агентный чат по одной, нескольким или всем встречам с рецептами (сохранёнными промптами), папки и spaces, People/Companies, шаринг ссылками, интеграции (Slack, Notion, HubSpot, Attio, Affinity, Zapier), MCP-сервер и read-only REST API с вебхуками [18][21][26][30][37][39].
Аудио не хранится, данные лежат в AWS US [45]. Тарифы: Basic $0 (история 30 дней), Business $14, Enterprise от $35 за пользователя в месяц [50][51].
Мобильные приложения (iOS, Android, Apple Watch) пишут только живой звук с микрофона и исходящие звонки на iOS. Звонки из Zoom/Meet на телефоне не захватываются [53].

## Разбор

### 1. Захват звука

**Модель захвата**
- Бота нет. Приложение работает на компьютере пользователя и берёт системный звук плюс микрофон [1][45].
- Приложение не умеет изолировать звук одного приложения: пишет общий системный поток, поэтому музыка или YouTube тоже попадут в транскрипт [1].
- Противоречие: в troubleshooting для Windows сказано, что для "неподдерживаемых" meeting-приложений звук собеседников может не попадать в транскрипт [9]. Это намекает на захват по процессам на Windows, но прямо нигде не сказано. Требует проверки руками.
- Микрофон в транскрипт-панели выбирается вручную или стоит "Auto": тогда он следует за микрофоном meeting-приложения, а без него берётся системный по умолчанию [9].
- Импорт готовых аудиофайлов (MP3 и т.п.) не поддерживается, только живая транскрипция [1].
- Разрешения: на macOS микрофон и "Screen & System Audio recording". На Windows разрешения выдаются автоматически, нужен только доступ к микрофону для desktop-приложений [2][9].
- Bluetooth-гарнитура при захвате её микрофона уходит в низкокачественный режим. Совет: взять встроенный микрофон ноутбука, Granola запомнит выбор [9].
- VDI (Citrix, Azure Virtual Desktop) официально не поддерживается: Teams desktop там не отдаёт звук собеседников [9].
- Kaspersky на Windows может блокировать аудиоустройства. Приложение показывает отдельное сообщение об антивирусе [9].

**Платформы**
- macOS (минимум macOS 14), Windows, iOS, Android, Apple Watch (watchOS 11+). Веб notes.granola.ai только для просмотра и редактирования, транскрибировать не умеет [1][2][55].
- Windows-установщик ставится per-user в `AppData\Local\Programs` и обновляется сам без админа. Установка "для всех пользователей" не поддерживается. Тихая установка: `Granola-<version>-win-x64.exe /S`. Есть гайды под MDM: Jamf, Kandji/Iru, Intune, JumpCloud, Rippling [66].
- Даты выхода по changelog: Windows 11 Jun 2025, iOS 30 Apr 2025, Android 01 Jul 2026, Apple Watch 28 Jul 2026 [68].
- Есть бета-канал (Settings > Preferences > Labs) [23].

**Когда начинается запись**
- Автостарта для всех встреч нет. Запись идёт, только если пользователь открыл заметку [1][4]:
  - кликнул уведомление;
  - открыл встречу после её начала;
  - нажал "New Note";
  - заранее открыл заметку будущей встречи, тогда запись стартует в запланированное время.
- Автостарт для всех встреч "under consideration". Сначала хотят сделать удаление и правку транскриптов [52].
- Календарное уведомление приходит за 1 минуту до встречи, только для событий с 2+ участниками. Клик открывает ссылку на видеозвонок и заметку и запускает транскрипцию [3].
- Уведомления - собственные pop-up окна Granola в правом верхнем углу, а не нативные уведомления macOS. Отключаются целиком или по приложениям в Settings > Notifications [3].
- Ad-hoc детект: Granola видит, что микрофон занят, и предлагает "Take Notes". Заголовок зависит от источника: "Huddle detected" (Slack), "Call detected" (FaceTime, WhatsApp), "Meeting detected" (остальные). В теле показано имя приложения [3].
- Если звонок начался в течение 15 минут после запланированной встречи, детект привязывает его к этому событию [3].
- При старте встречи окно Granola по умолчанию переставляется рядом со звонком (тоггл "Reposition Granola for meetings") [3].

**Когда запись останавливается**
- Кнопкой Stop. Есть Stop/Resume без завершения встречи [1].
- Авто-стоп срабатывает в трёх случаях [1]:
  - эвристика конца звонка: длина транскрипта, используется ли ещё приложение звонка, плановое время окончания события;
  - 15 минут без нового звука;
  - сон компьютера.
- Для ad-hoc встреч авто-стоп опирается на то, что приложение звонка освободило микрофон. На macOS ниже 14.2 у не-админов этого сигнала нет [1].
- Известная проблема: при звонках подряд Zoom может держать микрофон, и две встречи сливаются в один транскрипт [1].
- Заметку можно возобновить после завершения (Resume): новый звук дописывается в транскрипт, заметки можно перегенерировать [1].

**Индикация и UX во время записи**
- Зелёные "dancing bars" внизу заметки [1].
- Плавающий "live meeting indicator" справа на экране, когда Granola не в фокусе. Его можно перетаскивать, клик возвращает в заметку [1].
- В меню-баре показывается, сколько осталось до конца встречи ("10m left") [69].
- Во время записи можно открывать другие заметки, транскрипция продолжается (06 Mar 2025) [68].
- Escape возвращает на главный экран даже во время транскрипции [70].

**Календарь**
- Google Calendar и Microsoft 365/Outlook (только облачный Exchange Online, on-prem не поддерживается). Apple Calendar нет [2][4].
- Основной календарь синхронизируется по умолчанию, дополнительные включаются в Settings > Calendar. Календари других аккаунтов нужно расшарить на аккаунт входа с правом "See all event details" [4].
- Изменения подтягиваются за несколько минут. Отклонённые события, OOO, Focus Time и Working Location не показываются [4].
- "Coming up": по умолчанию 5 встреч с листанием. На десктопе 14 дней вперёд, на мобильных короткий список [4][53].
- Связанные встречи: заметки прошлых экземпляров показываются по recurring event ID ИЛИ по точному совпадению названия [4].
- Заметку можно перепривязать к другому событию. Granola предлагает события за последние 24 часа [70].
- Microsoft-скоупы только на чтение (Calendars.Read.Shared, openid, email, profile, offline_access) [72]. Для экшенов в чате, которые создают события, нужен отдельный коннектор Google Calendar [19].

**Офлайн и встречи вживую**
- На десктопе можно транскрибировать встречу вживую, голосовые заметки себе и любой звук компьютера [2].
- Транскрипция требует живого соединения с провайдерами: WebSocket по TCP 443 [9][10]. Офлайн-транскрипции на десктопе в источниках нет.
- Офлайн доступны чтение, редактирование и поиск заметок: заметки кэшируются локально (Dec 2024) [45][68].
- Apple Watch пишет офлайн и сохраняет аудио на часах. Синк идёт по цепочке Saved > Syncing to phone > Synced > Uploading > Generating notes > Complete [55].

**Уведомление участников и согласие**
- Ответственность за согласие лежит на пользователе [45].
- Automated chat message: Granola сама постит уведомление в чат встречи, когда транскрипция началась и в звонке есть хотя бы ещё один человек [61].
  - Google Meet: через браузерное расширение.
  - Zoom и Teams: macOS и Windows. На Windows окно Zoom должно быть на переднем плане.
  - Slack huddles: только macOS.
  - На macOS нужно разрешение Accessibility ("control your computer"). Поле ввода чата должно быть пустым.
- Granola Watermark: виртуальная камера "<камера> - Granola Watermark" с надписью "Transcribing", видна только во время записи [62][63].
  - Требования: macOS 14+ (camera extension) и Windows 11 (установка с правами админа).
  - Enterprise может сделать вотермарк обязательным. Тогда Granola может не транскрибировать, пока в meeting-приложении не выбрана эта камера.
- Heads Up (Enterprise, пилот): в календарные события подменяется ссылка на страницу-предупреждение с кнопкой "Join meeting" [64].
  - Каждый клик логируется: meeting ID, время, user agent.
  - Варианты: автоматическая подмена для Google или Outlook либо add-on к календарю.
- Скриншоты расшаренного экрана тоже сопровождаются напоминанием о согласии [6].
- На Apple Watch при старте и стопе играет звук и есть вибрация. Отключить нельзя [55].

### 2. Транскрипция

- Реалтайм. Live-транскрипт открывается в панели (иконка волны слева от "Ask anything") [1].
- В панели [1]:
  - Stop/Resume;
  - "Copy all";
  - копирование и удаление отдельных чанков (удаление частей транскрипта с 28 Jan 2026 [68]);
  - поиск по транскрипту.
- Пузыри: серые слева (системный звук, "Them"), зелёные справа (микрофон, "Me") [1].
- Транскрипт редактировать нельзя, можно только удалять чанки [52][1].
- ASR-провайдеры (в продуктовой документации не названы, раскрыты в allowlist для IT) [10]:
  - Deepgram: `api.deepgram.com` (основной эндпоинт) и dedicated-эндпоинты;
  - AssemblyAI: стриминг US, EU и edge.
- Исторически AssemblyAI использовался в iOS-бете с 27 Nov 2024. Тогда macOS-приложение работало на "другом сервисе транскрипции" [11].
- С 16 Dec 2024 транскрипция iOS ушла на бэкенд [11]. Значит, мобильный звук идёт через сервер, а не прямо в ASR. Это вывод из пост-мортема.
- Языки, docs [7]:
  - десктоп - 31 язык, мобильные - 17;
  - русский есть на обеих платформах;
  - польский и украинский есть только на мобильных.
- Противоречие по числу языков: changelog от 02 Sep 2026 называет 32 языка [68], а таблица в docs даёт 31 на десктопе и 33 в объединении.
- Режим языка на десктопе: "English" (лучшее качество) или "Multi-language" (переключение языков внутри одной встречи) [7].
  - Язык можно поменять прямо во время встречи. Будет пауза на загрузку модели, и настройка сохранится на будущее.
  - Уже готовый транскрипт перетранскрибировать нельзя.
- На мобильных автоматически выбирается один язык на встречу [7].
- Язык саммари: "English" или "Auto" (язык большинства в транскрипте) [7].
- Интерфейс только на английском [7].
- Словарь жаргона [8]:
  - Personal jargon - до 30 терминов, Workspace jargon - до 50 (правит админ).
  - Это буст распознавания, а не замена слов. Каждый термин уходит в модель отдельно.
  - Имена участников и название своей компании добавлять не нужно.
  - По странице цен "workspace-wide dictionary" есть только с Business [51].
- Спикеры:
  - Десктоп по умолчанию: Me/Them по каналу [1].
  - Speaker tags: имена из UI Zoom Workplace desktop, Teams desktop и Google Meet (Chrome через Accessibility или расширение Chrome/Edge/Arc/Brave). Работает на macOS и Windows [5][76].
  - Speaker tags читают display name и active speaker из интерфейса meeting-приложения. На macOS нужен Accessibility, на Windows отдельное разрешение не требуется [5].
  - Не различают нескольких людей за одним переговорным устройством, не работают задним числом и плохо работают при перекрёстной речи [5].
  - Мобильные: диаризация Speaker A/B/... [54].
- Спикер в API: `source` (microphone|speaker), `attribution` (me|them), `diarization_label` (только iOS), `name` (если спикер распознан). Сегмент транскрипта: `speaker`, `text`, `start_time`, `end_time` [40].
- Снимки расшаренного экрана [6]:
  - только macOS 14+, на Windows нет;
  - работает в Zoom desktop, Meet в Chrome, Teams desktop и Teams в Chromium-браузерах;
  - снимается только чужой шаринг, а не свой и не весь рабочий стол;
  - снимки загружаются в облако, лежат в стеке "Images" и используются при генерации заметок;
  - снимки на паузе, когда окно встречи не видно;
  - через MCP доступны как `get_meeting_attachment` [37].

### 3. Заметки

**Модель "твои заметки + транскрипт -> enhanced notes"**
- Вход: транскрипт, сырые заметки пользователя и данные события календаря [13].
- То, что ты написал сам, считается важным и обязательно попадает в результат. Опечатки и сокращения исправляются [12].
- В enhanced notes сырые заметки выделены чёрным [13].
- У каждого пункта есть лупа: показывает, из какого места транскрипта или сырых заметок он взялся [13].
- Генерация запускается автоматически, когда звонок закончился или его остановили вручную [13].
- Модели: "blend", в первую очередь OpenAI и Anthropic. Своих моделей (BYO) нет [13][46].
- Перегенерация [13]:
  - кнопка 🔁 в пикере шаблонов;
  - смена шаблона;
  - правка через чат ("make the next steps section more detailed").
- Есть предупреждения о перегенерации [68].
- Правки одной заметки не влияют на будущие. Стиль задаётся шаблонами и профилем [13].
- Профиль (имя, должность, контекст) влияет на качество заметок [23].
- Сырые заметки остаются доступны (иконка трёх линий). Их можно поправить и перегенерировать [13].

**Редактор**
- Markdown-ввод: `#` заголовки, `-`/`*` списки, `[]` чекбоксы. Жирный, курсив и подчёркивание через Cmd/Ctrl+B/I/U [12].
- Изображения JPEG/PNG вставляются drag-n-drop или из буфера [12].
- @mentions тимейтов с email-уведомлением (Dec 2025) [70].
- Тёмная тема (Nov 2024) [68].

**Шаблоны**
- Встроенные: набор зависит от типа пользователя, выбранного при онбординге. 29 обновлённых шаблонов с Oct 2024 [14][68].
- Кастомные шаблоны содержат свой промпт и секции [14].
- Применение: `/` в заметке во время звонка или дропдаун на табе "Enhanced" после генерации [14].
- Шаблоны создаются и применяются только на десктопе [14].
- Шаринг шаблона на "организацию" работает только при входе с корпоративного домена. Видимость определяется email-доменом, а не воркспейсом [14].
- Шаблон по умолчанию, кроме "auto", задать нельзя [52].

**Удаление**
- Корзина хранит 30 дней. Есть на десктопе и Android, на iOS нет [17].

**Pre-meeting briefs**
- Готовятся ночью для внешних встреч: 2-3 пункта сверху чат-панели [15].
- Источники: прошлые заметки с этими людьми, расшаренные заметки воркспейса, веб (включая LinkedIn), событие календаря, Gmail по opt-in [15].
- Платформы:
  - docs: macOS, Windows и iOS [15];
  - блог от 20 May 2026: только Mac и Windows [68].
- Ограничения: только Business/Enterprise и только Google Calendar. Без внешних участников или без данных brief не создаётся [15].

**Follow-up emails**
- Черновик письма после внешней встречи [16].
- Условия:
  - платный план или триал;
  - вход через Google с подключённым Gmail (read и send);
  - есть хотя бы один внешний гость;
  - всего не больше 10 человек.
- ИИ сам решает не писать, если канал общения Slack, follow-up за коллегой или есть блокер [16].
- Редактор письма: CC/BCC, вложения до 25 MB, подпись Gmail, Undo около 10 секунд [16].
- Черновики хранятся только локально и показываются 3 дня. Outlook не поддерживается [16].

### 4. AI-чат

- Где работает: десктоп и iPhone. На iPad и Android чата нет [18][53].
- Скоупы [18]:
  - домашний экран - все заметки;
  - одна встреча до, во время и после;
  - папка или space;
  - выбранные встречи (галочки);
  - запись People/Company (только десктоп).
- "Ask Granola" можно задавать во время встречи (Jan 2025) [68].
- Агентный чат с 21 Apr 2026 [71]:
  - инлайн-цитаты;
  - показывает, какие заметки использовал;
  - можно перенаправить на нужную заметку;
  - покрывает Team Space и "shared with me".
- Переписывание заметок: на триггер "rewrite" и по выделенному тексту. Перевод, правка имён, переназначение задач [18].
- Загрузка файлов как контекста. На iOS нет [18].
- Голосовая диктовка вопроса через иконку микрофона. Это не транскрипция встречи [22].
- Модели [20]:
  - Basic: только Auto.
  - Business: Auto или ручной выбор "standard" и "thinking" моделей. У thinking виден "Thought for Xs".
  - Enterprise: то же, Claude Fable 5 включает админ. Fable 5 не под ZDR: Anthropic хранит логи 30 дней. В HIPAA-воркспейсах Fable 5 недоступен.
- Глубина истории в чате: Basic - 30 дней, Business/Enterprise - вся [18].
- История чата приватна и не шарится вместе с заметкой [18].
- Экшены в чате (десктоп) [19]:
  - письмо через Gmail;
  - сообщение в Slack (каналы, групповые DM, DM; предупреждение про Slack Connect);
  - поиск слота и создание события в Google Calendar (только внутри своего Workspace-домена).
- У экшенов всегда есть шаг ревью: без подтверждения ничего не отправляется. Outlook/Microsoft не поддерживаются [19].
- Recipes [21]:
  - сохранённые промпты, вызываются `/` в чат-баре или из вкладки Recipes;
  - вкладка делится на Discover, My recipes и Workspace recipes;
  - тип рецепта: "Single meetings" или "Multiple meetings";
  - модель можно выбрать на Business+;
  - шаринг: Private, Workspace, Anyone with link (recipes.granola.ai). У чужих рецептов есть кнопка Remix;
  - переменных и плейсхолдеров нет, промпт статический.

### 5. Организация

- Workspaces [59]:
  - личные и командные, один аккаунт может состоять в нескольких;
  - роли Admin и Member;
  - auto-join по домену, invite-ссылки, discoverability;
  - перенос заметок между воркспейсами (по одной или все);
  - удалить воркспейс можно только когда в нём остался один участник.
- Spaces [26]:
  - "My notes" (приватный) и Team space;
  - Enterprise/админы могут создавать кастомные spaces для департаментов или проектов.
- Папки [26]:
  - иконка и описание (описание помогает чату);
  - подпапки только на один уровень;
  - одна заметка может лежать в нескольких папках;
  - мультиселект, Shift-клик и drag-n-drop.
- Авто-добавление recurring-встречи в папку [26].
- Права папок [26]:
  - роли Owner и Member;
  - доступ по ссылке: Invited only / Everyone at workspace / Anyone with link.
- Лимиты файлов папки: до 1 GB на папку, до 30 MB на файл, до 200 файлов [67].
- User Groups (Enterprise): вручную или через SCIM, роли Viewer и Collaborator на space или папку [27].
- People и Companies [28]:
  - автоматически строятся из участников календаря и обогащаются аватарами, должностями, компаниями "из разных источников";
  - вручную добавить нельзя;
  - внутри есть чат по человеку или компании;
  - поиск добавлен в Dec 2025 [70].
- Вид "Shared with me" (Sep 2025) [68].
- Поиск: быстрый и офлайн (Dec 2024) [68]. Детали ранжирования не раскрыты.
- Сайдбар переключается Cmd/Ctrl+S. Чат открывается Cmd/Ctrl+J, настройки Cmd/Ctrl+, [26][13][37].
- Granola Crunched - итоги года по заметкам (Dec 2025) [68].

### 6. Шаринг и экспорт

- Ссылка на заметку. Уровни по умолчанию: Anyone with link / Only my company / Private. Смена на более строгий уровень может применяться к старым заметкам [24][25].
- Enterprise задаёт потолок для всего воркспейса ретроактивно [25]:
  - Allow public links;
  - Only people at your company / Only approved domains;
  - Restricted access.
- Приглашение по email [24]:
  - роль Viewer (читает, видит контекст из транскрипта при наведении, может чатиться) или Collaborator (правит);
  - получатель не становится seat.
- Через ссылку на заметку полный транскрипт получатель не видит. Полный транскрипт видят коллабораторы папки в десктоп-приложении [24][26].
- Есть "Request access" с кнопкой "Grant access" у владельца [24].
- Copy enhanced notes копирует в Markdown (для Obsidian), "Send via email" открывает черновик в Gmail или Outlook [24].
- Экспорт [29]:
  - только CSV (заголовок, саммари, транскрипт, метаданные), только свои заметки, со всей историей;
  - приходит на email за несколько часов, ссылка живёт 24 часа, не чаще 1 раза в сутки;
  - на Enterprise выключен по умолчанию;
  - админ не может выгрузить заметки всех.
- Перенос заметок в другой аккаунт или воркспейс - только с десктопа [57][59].

### 7. Интеграции

- Slack [31]:
  - пост ссылки и саммари в канал вручную или авто-правилом на папку (только для новых заметок в папке);
  - для приватных каналов нужно добавить в канал app;
  - rich unfurl ссылок;
  - нужен Google Workspace или M365, личные Gmail и Outlook.com не подходят.
- Slack есть и на Basic [50].
- Notion: по одной заметке в базу, которую Granola создаёт сама. Ни авто, ни bulk-синка нет [32].
- HubSpot: Meeting или Note на contact, company или deal. Авто-правило на папку с матчингом по email участников. Нужен корпоративный аккаунт [33].
- Attio: note на person или company, авто-правило на папку [34].
- Affinity: персональный API-ключ Affinity. Granola только добавляет заметки, ничего не правит и не удаляет [35].
- Zapier (только платные планы и десктоп) [36]:
  - триггеры "Note Added to Granola Folder" и "Note Shared to Zapier";
  - payload: title, creator, attendees, calendar event, My Notes, Summary (Markdown), Transcript, Link.
- Salesforce:
  - упомянут в списке share-интеграций [24];
  - на странице цен значится как "advanced integration" только для Enterprise [51];
  - отдельной страницы в docs нет;
  - в audit-событиях есть `integration.salesforce_connected` и `integration.pipedrive_connected` [43]. Pipedrive больше нигде не документирован.
- Gmail и Google Calendar коннекторы для briefs, follow-up и экшенов чата [15][16][19].
- MCP (`https://mcp.granola.ai/mcp`) [37]:
  - транспорт Streamable HTTP;
  - авторизация OAuth с DCR, Enterprise-Managed Authorization (проверено с Okta XAA) или CIMD/private_key_jwt;
  - сервисных аккаунтов и API-ключей у MCP нет.
- MCP tools [37]:
  - `query_granola_meetings` - чат;
  - `list_meeting_folders` - платно;
  - `list_meetings`;
  - `get_meetings` - включая private notes и summary;
  - `get_meeting_attachment` - снимки экрана;
  - `get_meeting_transcript` - платно;
  - `get_account_info`;
  - `switch_workspace`;
  - `sign_out`.
- Лимит MCP около 100 запросов в минуту. Basic видит только 30 дней [37].
- Enterprise-тогглы MCP [37]:
  - "Meeting transcripts over MCP";
  - "Make changes over MCP" (добавлять и убирать заметки в папках, слать фидбек).
- Публичный REST API (`https://public-api.granola.ai`, Bearer `grn_...`, Business+) [38][40]:
  - `GET /v1/notes` - фильтры created_after/before, updated_after, folder_id, space_id, cursor, page_size;
  - `GET /v1/notes/{id}` - поля title, owner, attendees, calendar_event, summary_text/markdown, private_notes_* (только владелец), transcript, attachments, folder/space_membership, web_url;
  - `GET /v1/notes/{id}/transcript` - пагинация; при большом транскрипте Get Note отдаёт 413;
  - `GET /v1/folders`, `GET /v1/spaces`.
- Записи заметок через API нет. Возвращаются только заметки, у которых уже есть и саммари, и транскрипт [39].
- Лимиты API: burst 25 запросов за 5 секунд, устойчиво 5 rps. Sandbox нет [38][39].
- Ключи [38]:
  - персональные со скоупами Personal и/или Public;
  - workspace-ключи (без срока, видят public и spaces с API-доступом; на новых spaces доступ включён по умолчанию).
- Webhooks (Business+) [42]:
  - события `note.generated`, `note.edited`, `note.access_granted`;
  - payload без контента, сам контент забирается через API;
  - подпись по Standard Webhooks (HMAC-SHA256);
  - таймаут 15 секунд;
  - ретраи с экспоненциальной паузой 4 дня, затем эндпоинт отключается;
  - фильтр по folder_ids.
- Audit API (Enterprise) [43]:
  - `GET /v1/audit`, события за последний год, до 5 ключей;
  - события: transcription.started/ended, document.viewed, transcript.viewed, auth.*, mcp.tool_used, integration.* и т.д.
- Legal Holds API (Enterprise): CRUD холдов и кастодианов. Пока холд активен, заметки, транскрипты и вложения не удаляются [44].

### 8. Хранение и приватность

- Данные лежат в AWS US и шифруются at rest и in transit. Региональной резидентности (EU, UK и т.д.) нет [45].
- Заметки кэшируются локально [45].
- Аудио [1][45][54][65]:
  - на десктопе не записывается и не сохраняется;
  - во время встречи временно кэшируется для транскрипции, потом удаляется у Granola и третьих сторон;
  - на мобильных кэшируется временно.
- Видео и аудио хранить не планируют [52].
- Транскрипты и заметки хранятся бессрочно, но транскрипты можно авто-удалять [48]:
  - пользователь выбирает 1 день, неделю, месяц, 3, 6 или 12 месяцев с cooldown в неделю;
  - на Enterprise политика задаётся через Granola и применяется сразу;
  - после удаления транскрипта заметку нельзя перегенерировать.
- Обучение моделей [46][45]:
  - Free/Business: по умолчанию анонимизированные данные идут на собственные модели Granola, opt-out в "Use my data to improve models for everyone";
  - Enterprise: opt-out по умолчанию;
  - OpenAI и Anthropic на данных не обучаются (договоры).
- SOC 2 Type II есть, ISO 27001 нет, GDPR и DPA со SCC есть, FERPA нет [45].
- HIPAA с BAA только на Enterprise и только для десктопа [45][47].
- SSO [49][45]:
  - только Enterprise, от 50 активных мест или коммита на 50 мест;
  - брокер WorkOS [10];
  - Enforce SSO по домену.
- SCIM с синком групп, Okta Push Groups [49].
- Свой 2FA и пароли отсутствуют: всё делегировано IdP [45].
- Инциденты, по пост-мортемам самой Granola [11][75]:
  - утечка ключа AssemblyAI в iOS TestFlight, исправлено 11 Mar 2025;
  - auto-join по домену для unmanaged Google-аккаунтов, исправлено 23 May 2025;
  - сессии не разлогинивались после удаления из Google Workspace, исправлено к 2 Jun 2025;
  - навигация desktop через уведомление, июнь 2026.

### 9. Тарифы и лимиты

- Basic ($0) [50][51]:
  - история в приложении 30 дней (старые заметки скрыты, не удалены);
  - чат только Auto;
  - из интеграций только Slack;
  - шаблоны, рецепты и shared folders есть;
  - MCP с ограничениями.
- Business ($14 за пользователя в месяц, только помесячно) [50][51]:
  - безлимитная история;
  - выбор модели;
  - briefs и follow-up;
  - workspace dictionary;
  - интеграции, API, webhooks, MCP.
- Enterprise (от $35, есть годовая оплата) [50][51][63][64]:
  - SSO/SCIM, user groups;
  - админ-политики шаринга, MCP, API и интеграций;
  - workspace-wide opt-out, политика ретеншна;
  - HIPAA;
  - Audit API, Legal Holds;
  - Salesforce;
  - аналитика, обязательный Watermark, Heads Up (пилот);
  - MDM/Intune app wrapping для iOS.
- Мобильные приложения бесплатны. Дневного лимита транскрипции нет ни на одном плане [50].
- Биллинг по воркспейсу через Stripe. Каждый активный участник - платное место. Смешивать тарифы внутри воркспейса нельзя [50].
- Программы: стартапы и студенты - 12 месяцев Business бесплатно, некоммерческие организации - скидка 50% [50].

### 10. Мобильные приложения

- Назначение: встречи вживую и исходящие звонки (только iOS) [53].
- Звук других приложений (Zoom, Meet) на телефоне не захватывается из-за ограничений ОС [53].
- Звонки на iOS [56]:
  - встроенная звонилка;
  - верификация номера звонком на себя;
  - собеседник видит твой номер;
  - входящие не поддерживаются;
  - список стран ограничен (нельзя +91 на +91 и сеть Telstra).
- Индикация [53][54]:
  - iOS: Live Activity и Dynamic Island;
  - Android: постоянное уведомление с pause, resume и end плюс Quick Settings tile.
- Синк на десктоп только после того, как саммари готово. Шаблоны, интеграции, корзина (iOS) и календарь настраиваются на десктопе [57].
- Короткие записи на Android (меньше примерно 5 минут) могут не дать заметок. Записи около 10 часов и длиннее могут упасть [54].

### 11. Хоткеи и UX-детали

- Хоткеи из docs [12][13][26][37][76]:
  - Cmd/Ctrl+J - чат;
  - Cmd/Ctrl+S - сайдбар;
  - Cmd/Ctrl+, - настройки;
  - Cmd/Ctrl+R - обновить;
  - Cmd/Ctrl+B/I/U - форматирование;
  - `/` - шаблоны в заметке и рецепты в чате;
  - `@` - упоминание;
  - Esc - домой [70].
- Удобства, которые описаны в docs [3][1][6][69][14]:
  - уведомление за 1 минуту открывает ссылку звонка и сразу пишет;
  - окно встаёт рядом со звонком;
  - плавающий индикатор с превью снимков;
  - "Xm left" в меню-баре;
  - авто-стоп;
  - Resume после конца встречи;
  - смена шаблона одним кликом.
- Сетевая диагностика встроена (Settings > Troubleshooting > Run check) [9].
- Restart Granola есть в Dock и в трее Windows [9].
- Логи отправляются в AWS CloudWatch [10].

### 12. Слабые места и жалобы (помечено, откуда)

- **Признано в docs:**
  - нельзя править транскрипт [52];
  - нельзя задать шаблон по умолчанию [52];
  - нет Apple Calendar [52];
  - нет авто-экспорта и bulk в Notion [32][52];
  - нет автостарта [52];
  - нет аудио, чтобы перепроверить [52];
  - нет импорта файлов [1];
  - встречи подряд сливаются в одну [1];
  - авто-стоп ad-hoc ненадёжен [1];
  - VDI не поддерживается [9];
  - Bluetooth деградирует [9];
  - на Windows "неподдерживаемые" приложения теряют собеседников [9];
  - у speaker tags есть ограничения [5];
  - нет EU-резидентности [45];
  - SSO только от 50 мест [49];
  - follow-up, briefs и экшены чата работают только с Google [15][16][19].
- **Юридическое (иск, не решение суда):** коллективный иск Chamberlain v. Granola, Inc., N.D. Cal. 3:26-cv-07926, подан 30 Jul 2026 [73].
  - Претензии: запись без ведома участников (CIPA, all-party consent) и обучение на данных по умолчанию без opt-out для не-пользователей [74].
  - Granola на запрос Computerworld не ответила [74].
- **Обзоры и конкуренты (не первоисточник, только для ориентира):** жалобы на Me/Them без имён в многоголосых звонках, пропуск чисел и кодов в транскрипте, менее отполированный Windows. Первоисточников (форумов или issue) с этими жалобами я не нашёл. На Reddit и HN поиск прямых тредов не дал.

### Сводная таблица фич

| Фича | Платформы | План |
| - | - | - |
| Захват mic + system audio, без бота | macOS 14+, Windows | Все |
| Живая встреча (только mic) | iOS, Android, Watch | Все, бесплатно |
| Исходящие звонки | iOS | Все |
| Realtime-транскрипт, Me/Them | Десктоп | Все |
| Speaker tags (Zoom/Meet/Teams) | macOS, Windows | Все (pricing) |
| Диаризация Speaker A/B | Мобильные | Все |
| 31/17 языков, multi-language | Десктоп/мобильные | Все |
| Личный жаргон (30) | Десктоп | Все |
| Жаргон воркспейса (50) | Десктоп | Business+ |
| Снимки шаринга экрана | Только macOS 14+ | Все, Ent - админ |
| Enhanced notes + шаблоны | Десктоп (мобильные - auto) | Все |
| Pre-meeting briefs | macOS, Windows, iOS(?) | Business+ |
| Follow-up emails (Gmail) | Десктоп | Платные/триал |
| Чат: встреча/папка/все | Десктоп, iPhone | Все |
| Выбор модели в чате | Десктоп | Business+ |
| Экшены чата (Gmail/Slack/GCal) | Десктоп | Не указано |
| Recipes | Десктоп, iPhone | Все |
| Папки, spaces, People/Companies | Десктоп (+моб. папки) | Все |
| Кастомные spaces, user groups | Десктоп | Enterprise |
| Шаринг ссылкой, Viewer/Collab | Десктоп, веб | Все |
| CSV-экспорт | Десктоп | Все (Ent - админ) |
| Slack | Десктоп | Все |
| Notion/HubSpot/Attio/Affinity | Десктоп | Business+ |
| Zapier | Десктоп | Business+ |
| Salesforce | Не документировано | Enterprise |
| MCP | Любой MCP-клиент | Все (Basic урезан) |
| REST API + webhooks | Сервер | Business+ |
| Audit API, Legal Holds | Сервер | Enterprise |
| Chat message / Watermark | Десктоп (Watermark - Win 11) | Все, Ent - принудительно |
| Heads Up | Google/Outlook календарь | Enterprise, пилот |
| SSO/SCIM, HIPAA | - | Enterprise |
| Авто-удаление транскриптов | Десктоп | Все (Ent - политика) |

## Проверено по источникам / требует проверки руками

**Проверено по официальным docs, API-спеке и странице цен:**
- захват, авто-старт и стоп;
- уведомления;
- языки и жаргон;
- speaker tags;
- шаблоны, чат, рецепты;
- организация и шаринг;
- все интеграции, кроме Salesforce;
- MCP tools, API-эндпоинты и вебхуки;
- приватность и тарифы.

**Только установив приложение (в источниках нет или есть противоречия):**
- Техника захвата системного звука на Windows: общий loopback или захват по процессам. Docs противоречат друг другу [1] и [9]. Есть ли эхоподавление на Windows.
- Как именно детектится занятость микрофона и окончание звонка на Windows. Работает ли ad-hoc детект на Windows так же, как на macOS (docs пишут без разделения).
- Задержка live-транскрипта, размер чанков, что происходит при обрыве сети посреди встречи (буферизация или потеря).
- Формат локального кэша и офлайн-поиска, движок поиска.
- Структура промпта enhanced notes, как шаблон влияет на секции, как устроены ссылки-лупы на транскрипт.
- Полный список встроенных шаблонов и рецептов (Discover).
- Полный список хоткеев: в docs только перечисленные выше. Есть ли глобальные хоткеи старта и стопа записи.
- Плавающий индикатор и "10m left" на Windows (в источниках описан общий вид, источник про меню-бар - macOS).
- Работают ли экшены чата на Basic.
- Salesforce: native или через Zapier, что синкается.
- Pipedrive и `cloud_agent` из audit-событий: что это за интеграции.
- Briefs на iOS: docs и блог расходятся.
- Число языков: 31 (docs) против 32 (changelog).
- Стек десктопа не раскрыт. Установщик `/S` в `AppData\Local\Programs` похож на electron-builder NSIS, но это мой вывод, а не факт из источника.

## Источники

1. https://docs.granola.ai/help-center/taking-notes/transcription - старт, стоп и авто-стоп, панель транскрипта, захват звука, импорт файлов, speaker tags
2. https://docs.granola.ai/help-center/getting-started/setting-up-granola-for-the-first-time - платформы, macOS 14, разрешения, Exchange Online, вход
3. https://docs.granola.ai/help-center/taking-notes/notifications - уведомления за 1 минуту, ad-hoc детект, reposition, правило 15 минут
4. https://docs.granola.ai/help-center/getting-started/syncing-your-calendars - календари, Coming up, related meetings, нет автостарта
5. https://docs.granola.ai/help-center/taking-notes/speaker-attribution (+ /speaker-attribution-zoom) - speaker tags, ограничения, Accessibility
6. https://docs.granola.ai/help-center/taking-notes/capture-shared-screens - снимки экрана, macOS 14+, приложения
7. https://docs.granola.ai/help-center/customising-granola/multi-language - таблица языков, multi-language, язык саммари
8. https://docs.granola.ai/help-center/customising-granola/customising-transcription - жаргон 30/50, ограничения
9. https://docs.granola.ai/help-center/troubleshooting/transcription-issues - выбор микрофона, Windows-приложения, Kaspersky, VDI, Bluetooth, Run check
10. https://docs.granola.ai/help-center/troubleshooting/network-troubleshooting - домены: Deepgram, AssemblyAI, WorkOS, Knock, CloudWatch, WebSocket
11. https://docs.granola.ai/help-center/policies/security-reports/post-mortem-assembly-ai-api-key-exposure - AssemblyAI в iOS, другой ASR на macOS, транскрипция на бэкенде
12. https://docs.granola.ai/help-center/taking-notes/taking-notes-in-granola - редактор, markdown, хоткеи форматирования, изображения
13. https://docs.granola.ai/help-center/taking-notes/ai-enhanced-notes - входы генерации, лупа, модели OpenAI/Anthropic, перегенерация, Cmd+J
14. https://docs.granola.ai/help-center/taking-notes/customise-notes-with-templates - шаблоны, `/`, шаринг по домену
15. https://docs.granola.ai/help-center/taking-notes/pre-meeting-briefs - briefs, источники, ограничения
16. https://docs.granola.ai/help-center/taking-notes/follow-up-emails - follow-up, условия, редактор, лимиты
17. https://docs.granola.ai/help-center/taking-notes/deleting-notes - корзина 30 дней
18. https://docs.granola.ai/help-center/getting-more-from-your-notes/chatting-with-your-meetings - скоупы чата, rewrite, файлы, история по планам
19. https://docs.granola.ai/help-center/getting-more-from-your-notes/workflows-in-chat - экшены Gmail/Slack/GCal
20. https://docs.granola.ai/help-center/getting-more-from-your-notes/understanding-model-selection-in-granola-chat - Auto/standard/thinking, Fable 5, ZDR
21. https://docs.granola.ai/help-center/getting-more-from-your-notes/recipes - рецепты
22. https://docs.granola.ai/help-center/getting-more-from-your-notes/granola-chat-dictation-vs-transcription - диктовка в чат
23. https://docs.granola.ai/help-center/customising-granola/profile-and-preferences - профиль, opt-out, бета-канал
24. https://docs.granola.ai/help-center/sharing/sharing-notes - ссылки, роли, Request access, copy markdown, email
25. https://docs.granola.ai/help-center/consent-security-privacy/sharing-controls - уровни шаринга, политика Enterprise
26. https://docs.granola.ai/help-center/sharing/folders/spaces-and-folders - spaces, папки, права, авто-пост, Cmd+S
27. https://docs.granola.ai/help-center/sharing/user-groups - группы
28. https://docs.granola.ai/help-center/people-and-companies - People/Companies
29. https://docs.granola.ai/help-center/sharing/exporting-notes - CSV-экспорт
30. https://docs.granola.ai/help-center/sharing/integrations/integrations-with-granola - обзор интеграций
31. https://docs.granola.ai/help-center/sharing/integrations/slack - Slack
32. https://docs.granola.ai/help-center/sharing/notion - Notion
33. https://docs.granola.ai/help-center/sharing/integrations/hub-spot - HubSpot
34. https://docs.granola.ai/help-center/sharing/integrations/attio - Attio
35. https://docs.granola.ai/help-center/sharing/integrations/affinity - Affinity
36. https://docs.granola.ai/help-center/sharing/integrations/zapier - Zapier, триггеры, payload
37. https://docs.granola.ai/help-center/sharing/integrations/mcp - MCP: URL, auth, tools, лимиты, Enterprise-тогглы, Cmd+,
38. https://docs.granola.ai/help-center/sharing/integrations/granola-api - ключи, скоупы, workspace-ключи, лимиты
39. https://docs.granola.ai/introduction - обзор API, только завершённые заметки, rate limits
40. https://docs.granola.ai/api-reference/openapi.json - эндпоинты, схемы Note/Transcript/Speaker
41. https://docs.granola.ai/api-reference/changelog - версии API v1.0-v1.6
42. https://docs.granola.ai/webhooks - вебхуки
43. https://docs.granola.ai/audit (+ /audit-events) - Audit API и список событий
44. https://docs.granola.ai/legal-holds - Legal Holds API
45. https://docs.granola.ai/help-center/consent-security-privacy/security-privacy-data-faqs - AWS US, аудио, сертификации, SSO, 2FA, обучение
46. https://docs.granola.ai/help-center/consent-security-privacy/model-training - обучение, BYO-моделей нет
47. https://docs.granola.ai/help-center/consent-security-privacy/is-granola-hipaa-compliant - HIPAA
48. https://docs.granola.ai/help-center/consent-security-privacy/transcript-auto-deletion - авто-удаление транскриптов
49. https://docs.granola.ai/help-center/sso-setup-guide - SSO и SCIM
50. https://docs.granola.ai/help-center/managing-your-account/subscriptions-and-billing - планы, цены, биллинг
51. https://www.granola.ai/pricing - матрица фич по планам
52. https://docs.granola.ai/help-center/feature-requests - чего нет и почему
53. https://docs.granola.ai/help-center/ios/getting-started - мобильные: возможности, различия iOS и Android
54. https://docs.granola.ai/help-center/ios/transcription - мобильная транскрипция, диаризация, лимиты длины
55. https://docs.granola.ai/help-center/ios/apple-watch - Apple Watch
56. https://docs.granola.ai/help-center/ios/phone-calls - звонки на iOS
57. https://docs.granola.ai/help-center/ios/syncing-with-desktop - синк и что требует десктопа
58. https://docs.granola.ai/help-center/ios/taking-notes - заметки на мобильных
59. https://docs.granola.ai/help-center/workspaces - воркспейсы, роли, перенос
60. https://docs.granola.ai/help-center/consent-security-privacy/transparency-solutions/introduction - chat message и Watermark
61. https://docs.granola.ai/help-center/consent-security-privacy/transparency-solutions/for-individuals/zoom - механика chat message
62. https://docs.granola.ai/help-center/consent-security-privacy/transparency-solutions/for-individuals/watermark - виртуальная камера
63. https://docs.granola.ai/help-center/consent-security-privacy/transparency-solutions/for-enterprise-admins/watermark - Windows 11+, enforcement
64. https://docs.granola.ai/help-center/consent-security-privacy/heads-up-for-enterprise - Heads Up
65. https://docs.granola.ai/help-center/consent-security-privacy/getting-consent - согласие, аудио на мобильных
66. https://docs.granola.ai/help-center/getting-started/managed-installations - установщик Windows, MDM
67. https://docs.granola.ai/help-center/troubleshooting/increased-file-limits - лимиты файлов
68. https://www.granola.ai/updates - changelog с датами фич
69. https://www.granola.ai/updates/whats-new-2025-12-12 - "10m left", календарные подсказки
70. https://www.granola.ai/updates/whats-new-2025-12-19 - @mentions, корзина, Esc, перепривязка события
71. https://www.granola.ai/blog/granola-chat-just-got-smarter - агентный чат, цитаты
72. https://docs.granola.ai/help-center/sign-in-with-microsoft - Microsoft-скоупы только на чтение
73. https://dockets.justia.com/docket/california/candce/3%3A2026cv07926/475308 - иск Chamberlain v. Granola
74. https://www.computerworld.com/article/4206255/granola-lawsuit-raises-concerns-over-ai-note-taking-app-privacy.html - суть иска, Granola не ответила
75. https://docs.granola.ai/help-center/policies/security-reports/post-mortem-legacy-unmanaged-google-accounts-workspace-auto-join, .../post-mortem-google-workspace-session-logout-vulnerability, .../security-contributions/desktop-app-navigation-vulnerability - инциденты безопасности
76. https://docs.granola.ai/help-center/taking-notes/speaker-attribution-google-meet - Meet speaker tags через Accessibility или расширение, Cmd+R
