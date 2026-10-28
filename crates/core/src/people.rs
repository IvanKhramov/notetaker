//! Rebuilt observations plus explicit local identity edits. Never guess nicknames.
use crate::{Library, Result, person_matches};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub(crate) fn normalized(value: &str) -> String {
    value
        .trim()
        .trim_start_matches('@')
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
        .replace('ё', "е")
}
fn email(value: &str) -> String {
    value.trim().trim_start_matches("mailto:").to_lowercase()
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Person {
    pub id: String,
    pub name: String,
    pub emails: BTreeSet<String>,
    pub aliases: BTreeSet<String>,
    pub voice_ids: BTreeSet<String>,
    pub hidden: bool,
    pub identities: BTreeSet<String>,
}
impl Person {
    pub fn matches(&self, value: &str) -> bool {
        !value.trim().is_empty()
            && (self.emails.contains(&email(value))
                || person_matches(&self.name, value)
                || self.aliases.iter().any(|n| person_matches(n, value)))
    }
    fn absorb(&mut self, other: Self) {
        if self.name.is_empty() {
            self.name.clone_from(&other.name);
        }
        self.aliases.extend(other.aliases);
        if !other.name.is_empty() {
            self.aliases.insert(other.name);
        }
        self.emails.extend(other.emails);
        self.voice_ids.extend(other.voice_ids);
        self.identities.extend(other.identities);
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PeopleEdits {
    #[serde(skip)]
    pub revision: crate::Revision,
    /// Source identity -> user-selected stable identity.
    pub merges: BTreeMap<String, String>,
    pub names: BTreeMap<String, String>,
    #[serde(default)]
    pub aliases: BTreeMap<String, BTreeSet<String>>,
    pub hidden: BTreeSet<String>,
}
impl PeopleEdits {
    pub fn merge(&mut self, from: &Person, into: &Person) {
        self.names
            .entry(into.id.clone())
            .or_insert_with(|| into.name.clone());
        self.aliases
            .entry(into.id.clone())
            .or_default()
            .extend(from.aliases.iter().cloned().chain([from.name.clone()]));
        for key in from.identities.iter().chain(&into.identities) {
            self.hidden.remove(key);
        }
        self.set_hidden(into, into.hidden);
        for key in &from.identities {
            self.merges.insert(key.clone(), into.id.clone());
        }
        for target in self.merges.values_mut() {
            if *target == from.id {
                target.clone_from(&into.id);
            }
        }
    }
    pub fn set_hidden(&mut self, person: &Person, hidden: bool) {
        self.hidden.remove(&person.id);
        for key in &person.identities {
            self.hidden.remove(key);
        }
        if hidden {
            self.hidden.insert(person.id.clone());
        }
    }
    pub fn rename(&mut self, person: &Person, name: &str) {
        if !name.trim().is_empty() {
            self.aliases
                .entry(person.id.clone())
                .or_default()
                .extend(person.aliases.iter().cloned().chain([person.name.clone()]));
            self.names.insert(person.id.clone(), name.trim().into());
        }
    }
}

/// Ambiguous names never collapse two distinct email identities.
pub fn resolve_people(observations: Vec<Person>, edits: &PeopleEdits) -> Vec<Person> {
    let mut keyed: BTreeMap<String, Person> = BTreeMap::new();
    for person in observations {
        keyed.entry(person.id.clone()).or_default().absorb(person);
    }
    let mut people: Vec<Person> = Vec::new();
    // Emails first, then voices, then bare names.
    for (key, mut person) in keyed {
        person.id.clone_from(&key);
        person.identities.insert(key);
        if let Some(name) = edits.names.get(&person.id) {
            person.aliases.insert(name.clone());
        }
        if let Some(aliases) = edits.aliases.get(&person.id) {
            person.aliases.extend(aliases.iter().cloned());
        }
        people.push(person);
    }
    let mut result: Vec<Person> = people
        .iter()
        .filter(|p| !p.emails.is_empty())
        .cloned()
        .collect();
    let mut unnamed_email: Vec<_> = people.into_iter().filter(|p| p.emails.is_empty()).collect();
    unnamed_email.sort_by_key(|p| {
        (
            !edits.names.contains_key(&p.id),
            p.voice_ids.is_empty(),
            p.id.clone(),
        )
    });
    for person in unnamed_email {
        let candidates: Vec<_> = result
            .iter()
            .enumerate()
            .filter(|(_, existing)| {
                !existing.voice_ids.is_disjoint(&person.voice_ids)
                    || person.aliases.iter().any(|n| existing.matches(n))
            })
            .map(|(i, _)| i)
            .collect();
        if candidates.len() == 1 {
            result[candidates[0]].absorb(person);
        } else {
            result.push(person);
        }
    }
    let canonical: BTreeMap<_, _> = result
        .iter()
        .flat_map(|person| {
            person
                .identities
                .iter()
                .map(|key| (key.clone(), person.id.clone()))
        })
        .collect();
    let mut merged: BTreeMap<String, Person> = BTreeMap::new();
    for person in result {
        let target = person
            .identities
            .iter()
            .find_map(|key| edits.merges.get(key))
            .map(|target| canonical.get(target).unwrap_or(target).clone())
            .unwrap_or_else(|| person.id.clone());
        let entry = merged.entry(target.clone()).or_insert_with(|| Person {
            id: target,
            ..Person::default()
        });
        entry.absorb(person);
    }
    let mut people: Vec<_> = merged.into_values().collect();
    for person in &mut people {
        if let Some(name) = edits.names.get(&person.id).or_else(|| {
            person
                .identities
                .iter()
                .find_map(|key| edits.names.get(key))
        }) {
            person.name.clone_from(name);
            person.aliases.insert(name.clone());
        }
        for key in person.identities.iter().chain(std::iter::once(&person.id)) {
            if let Some(aliases) = edits.aliases.get(key) {
                person.aliases.extend(aliases.iter().cloned());
            }
        }
        person.hidden = edits.hidden.contains(&person.id)
            || person
                .identities
                .iter()
                .any(|key| edits.hidden.contains(key));
        person.aliases.remove(&person.name);
    }
    people.sort_by_key(|p| (normalized(&p.name), p.id.clone()));
    people
}
fn observation(name: &str, address: &str, voice: Option<&str>) -> Option<Person> {
    let address = email(address);
    let name = name.trim();
    let id = if !address.is_empty() {
        format!("email:{address}")
    } else if let Some(voice) = voice {
        format!("voice:{voice}")
    } else if !name.is_empty() {
        format!("name:{}", normalized(name))
    } else {
        return None;
    };
    Some(Person {
        id,
        name: if name.is_empty() {
            address.clone()
        } else {
            name.into()
        },
        emails: (!address.is_empty())
            .then_some(address)
            .into_iter()
            .collect(),
        aliases: (!name.is_empty())
            .then(|| name.into())
            .into_iter()
            .collect(),
        voice_ids: voice.map(String::from).into_iter().collect(),
        ..Person::default()
    })
}
pub(crate) fn meeting_people(
    meta: &crate::MeetingMeta,
    transcript: &crate::Transcript,
) -> Vec<Person> {
    let mut people = Vec::new();
    if let Some(event) = &meta.calendar_event {
        for a in &event.attendees {
            people.extend(observation(&a.name, &a.email, None));
        }
    }
    for (speaker, name) in &transcript.speaker_names {
        people.extend(observation(
            name,
            "",
            transcript
                .speaker_voice_ids
                .get(speaker)
                .map(String::as_str),
        ));
    }
    for (speaker, voice) in &transcript.speaker_voice_ids {
        if !transcript.speaker_names.contains_key(speaker) {
            people.extend(observation("", "", Some(voice)));
        }
    }
    people
}
impl Library {
    pub fn read_people_edits(&self) -> Result<PeopleEdits> {
        let loaded = self.load_shared_text("people.json")?;
        let mut edits = if loaded.value.is_empty() {
            PeopleEdits::default()
        } else {
            serde_json::from_str(&loaded.value)?
        };
        edits.revision = loaded.revision;
        Ok(edits)
    }
    pub fn write_people_edits(&self, edits: &PeopleEdits) -> Result<()> {
        self.save_shared_text(
            "people.json",
            &serde_json::to_string_pretty(edits)?,
            edits.revision,
        )?;
        Ok(())
    }
    /// Local-only voice enrollment and user aliases are omitted for MCP privacy.
    pub fn people(
        &self,
        exclude_local: bool,
        user_name: &str,
        user_aliases: &str,
    ) -> Result<Vec<Person>> {
        self.reindex_incremental()?;
        let mut observations = self.indexed_people(exclude_local)?;
        for task in self.list_tasks(None, None, exclude_local)? {
            for value in [&task.owner, &task.requester] {
                observations.extend(observation(
                    value,
                    if value
                        .split_once('@')
                        .is_some_and(|(name, domain)| !name.is_empty() && !domain.is_empty())
                    {
                        value
                    } else {
                        ""
                    },
                    None,
                ));
            }
        }
        let mut edits = self.read_people_edits()?;
        if exclude_local {
            // An explicit merge can point at an identity seen only in Local.
            // Do not reveal that target key or old source aliases through MCP.
            let keys: BTreeSet<_> = observations.iter().map(|p| p.id.clone()).collect();
            let names: BTreeSet<_> = observations
                .iter()
                .flat_map(|p| p.aliases.iter().chain(std::iter::once(&p.name)))
                .map(|name| normalized(name))
                .collect();
            edits
                .merges
                .retain(|source, target| keys.contains(source) && keys.contains(target));
            edits.names.retain(|key, _| keys.contains(key));
            edits.aliases.retain(|key, aliases| {
                aliases.retain(|name| names.contains(&normalized(name)));
                keys.contains(key)
            });
        }
        if !exclude_local {
            for voice in self.read_voices()?.voices {
                observations.extend(observation(&voice.name, "", voice.id.as_deref()));
            }
            if let Some(user) = observation(user_name, "", None) {
                for alias in user_aliases
                    .split([',', ';', '\n'])
                    .filter(|s| !s.trim().is_empty())
                {
                    if let Some(alias) = observation(alias, "", None) {
                        edits
                            .merges
                            .entry(alias.id.clone())
                            .or_insert_with(|| user.id.clone());
                        observations.push(alias);
                    }
                }
                observations.push(user);
            }
        }
        Ok(resolve_people(observations, &edits))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_names_email_precedence_and_explicit_nickname_merge() {
        let observations = vec![
            observation(" Александр ", "A@EXAMPLE.ORG", None).unwrap(),
            observation("АЛЕКСАНДР", "a@example.org", None).unwrap(),
            observation("александр", "", None).unwrap(),
            observation("Саша", "", Some("v1")).unwrap(),
        ];
        let people = resolve_people(observations.clone(), &PeopleEdits::default());
        assert_eq!(people.len(), 2);
        let mut edits = PeopleEdits::default();
        edits.merge(&people[1], &people[0]);
        let person = resolve_people(observations, &edits).pop().unwrap();
        assert!(person.matches("Саша"));
        assert!(person.matches("a@example.org"));
        assert!(person.voice_ids.contains("v1"));
        let separate = resolve_people(
            vec![
                observation("Иван", "a@x", None).unwrap(),
                observation("Иван", "b@x", None).unwrap(),
                observation("иван", "", None).unwrap(),
            ],
            &PeopleEdits::default(),
        );
        assert_eq!(separate.len(), 3);
    }
}
