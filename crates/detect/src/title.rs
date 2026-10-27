//! Window titles are hints: background tabs and multiple call windows are ambiguous.

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallIdentity {
    pub service: String,
    pub meeting_title: Option<String>,
}

/// Parse without Win32 so service names and separator handling are testable anywhere.
pub fn parse_call_title(title: &str) -> Option<CallIdentity> {
    let mut parts: Vec<_> = title
        .split(" - ")
        .flat_map(|part| part.split(" | "))
        .flat_map(|part| part.split('\u{2014}'))
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect();
    while parts.last().is_some_and(|part| is_browser_suffix(part)) {
        parts.pop();
    }
    let (index, service) = parts.iter().enumerate().find_map(|(index, part)| {
        let lower = part.to_lowercase();
        [
            ("Контур.Толк", &["контур.толк", "kontur talk", "ktalk"][..]),
            ("Телемост", &["телемост", "telemost"][..]),
            ("Google Meet", &["google meet", "meet"][..]),
            ("Zoom", &["zoom"][..]),
        ]
        .into_iter()
        .find(|(_, patterns)| {
            patterns.iter().any(|pattern| {
                // Bare Meet is only the conventional "Meet - title" prefix.
                if *pattern == "meet" {
                    return index == 0 && lower == "meet" && parts.len() > 1;
                }
                lower.match_indices(pattern).any(|(at, matched)| {
                    let before = lower[..at].chars().next_back();
                    let after = lower[at + matched.len()..].chars().next();
                    before.is_none_or(|c| !c.is_alphanumeric())
                        && after.is_none_or(|c| !c.is_alphanumeric())
                })
            })
        })
        .map(|(service, _)| (index, service))
    })?;
    parts.remove(index);
    let meeting_title = (!parts.is_empty()).then(|| parts.join(" - "));
    Some(CallIdentity {
        service: service.into(),
        meeting_title,
    })
}

fn is_browser_suffix(part: &str) -> bool {
    matches!(
        part.to_lowercase().as_str(),
        "google chrome"
            | "chrome"
            | "microsoft edge"
            | "edge"
            | "mozilla firefox"
            | "firefox"
            | "yandex browser"
            | "yandex"
            | "яндекс браузер"
            | "яндекс"
    )
}

/// Several visible call windows cannot identify which one owns the microphone.
/// Keep the service when unanimous, but never guess a meeting title.
pub fn identify_call<'a>(titles: impl IntoIterator<Item = &'a str>) -> Option<CallIdentity> {
    let mut calls = titles.into_iter().filter_map(parse_call_title);
    let mut call = calls.next()?;
    for other in calls {
        if other.service != call.service {
            return None;
        }
        if other.meeting_title != call.meeting_title {
            call.meeting_title = None;
        }
    }
    Some(call)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_services_browser_suffixes_and_supported_separators() {
        for (title, service, meeting) in [
            (
                "Обсудить концепцию AI-Driven | Контур.Толк - Google Chrome",
                "Контур.Толк",
                "Обсудить концепцию AI-Driven",
            ),
            (
                "Planning - Kontur Talk - Microsoft Edge",
                "Контур.Толк",
                "Planning",
            ),
            ("Planning | ktalk", "Контур.Толк", "Planning"),
            ("План \u{2014} Телемост \u{2014} Яндекс", "Телемост", "План"),
            (
                "Telemost - Planning - Mozilla Firefox",
                "Телемост",
                "Planning",
            ),
            (
                "Meet - abc-defg-hij - Google Chrome",
                "Google Meet",
                "abc-defg-hij",
            ),
            ("Sprint | Google Meet", "Google Meet", "Sprint"),
            ("ZOOM - Planning", "Zoom", "Planning"),
            ("One - Two | Телемост", "Телемост", "One - Two"),
        ] {
            let parsed = parse_call_title(title).unwrap();
            assert_eq!(parsed.service, service, "{title}");
            assert_eq!(parsed.meeting_title.as_deref(), Some(meeting), "{title}");
        }
    }

    #[test]
    fn service_only_has_no_meeting_title_and_unrelated_titles_are_ignored() {
        for title in ["Телемост", "Zoom Meeting - Google Chrome", "Контур.Толк"] {
            assert!(parse_call_title(title).unwrap().meeting_title.is_none());
        }
        for title in [
            "",
            "Mail - Google Chrome",
            "Meetup",
            "Zooming",
            "Meet the team",
        ] {
            assert_eq!(parse_call_title(title), None);
        }
    }

    #[test]
    fn ambiguous_windows_do_not_guess_a_title_or_service() {
        assert_eq!(
            identify_call(["A | Telemost", "B | Telemost"])
                .unwrap()
                .meeting_title,
            None
        );
        assert_eq!(identify_call(["A | Telemost", "B | Zoom"]), None);
        assert_eq!(
            identify_call(["Mail", "A | Telemost"]),
            parse_call_title("A | Telemost")
        );
    }
}
