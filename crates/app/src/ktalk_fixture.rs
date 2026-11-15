//! Neutral synthetic meeting shared by headless tests and screenshot scenes.
pub fn plan() -> (notetaker_core::Transcript, notetaker_llm::ktalk::Plan) {
    use notetaker_core::{Channel, Segment, Source, SpeakerId, Transcript};
    let start = chrono::Utc::now() - chrono::Duration::minutes(6);
    let texts = [
        "Проектирование очереди сообщений требует проверки порядка",
        "Кэширование ответов сервера сокращает задержку обработки",
        "Планируем обновление схемы хранения журнала событий",
        "Измеряем пропускную способность сетевого подключения клиента",
        "Регулярное резервирование защищает данные рабочих проектов",
        "Разделение нагрузки позволяет ускорить расчёт результатов",
        "Документируем ограничения протокола передачи файлов приложения",
    ];
    let mut local = Transcript::default();
    let mut tracks = vec![
        serde_json::json!({"speaker":{"isAnonymous":false,"userInfo":{"key":"neutral","firstname":"Anna","surname":"Petrova"}},"chunks":[]}),
        serde_json::json!({"speaker":{"isAnonymous":true,"anonymousId":"neutral-guest","anonymousName":"Гость"},"chunks":[]}),
    ];
    for (index, t) in [
        120_000_u64,
        127_300,
        139_900,
        156_100,
        178_600,
        211_700,
        250_200,
    ]
    .into_iter()
    .enumerate()
    {
        let text = texts[index];
        let words:Vec<_>=text.split_whitespace().enumerate().map(|(i,text)|serde_json::json!({
            "text":text,"startTimeOffsetInMillis":t+i as u64*500,"endTimeOffsetInMillis":t+(i+1) as u64*500
        })).collect();
        let end = t + words.len() as u64 * 500;
        local.segments.push(Segment {
            id: uuid::Uuid::now_v7(),
            channel: Channel::System,
            start_ms: t,
            end_ms: end,
            text: text.into(),
            speaker: SpeakerId("system-0".into()),
            source: Source::Final,
            edited: false,
            lang: Some("ru".into()),
        });
        tracks[index%2]["chunks"].as_array_mut().unwrap().push(serde_json::json!({"startTimeOffsetInMillis":t,"endTimeOffsetInMillis":end,"words":words}));
    }
    local.segments.push(Segment {
        id: uuid::Uuid::now_v7(),
        channel: Channel::Mic,
        start_ms: 140_000,
        end_ms: 143_000,
        text: "Проверим план перед выпуском обновления".into(),
        speaker: SpeakerId("me".into()),
        source: Source::Final,
        edited: false,
        lang: Some("ru".into()),
    });
    let remote =
        serde_json::from_value(serde_json::json!({"status":"complete","tracks":tracks})).unwrap();
    let recording = notetaker_llm::ktalk::Recording {
        id: "neutral-recording".into(),
        title: "Планирование выпуска".into(),
        created_date: start,
        duration: 300,
        status: "complete".into(),
    };
    let owner = notetaker_llm::ktalk::User {
        key: String::new(),
        firstname: "Олег".into(),
        surname: "Сидоров".into(),
    };
    let plan = notetaker_llm::ktalk::plan(&local, start, recording, &remote, &owner).unwrap();
    (local, plan)
}
