//! Gry czatu: deterministyczne odpowiedzi, czas i punkty liczone w Rust.
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Question {
    pub text: String,
    pub choices: Vec<String>,
    pub correct: usize,
}
pub fn validate(questions: &[Question]) -> Result<(), String> {
    if questions.is_empty() || questions.len() > 50 {
        return Err("Dodaj od 1 do 50 pytań.".into());
    }
    for q in questions {
        if q.text.trim().is_empty()
            || q.text.chars().count() > 240
            || !(2..=6).contains(&q.choices.len())
            || q.correct >= q.choices.len()
            || q.choices
                .iter()
                .any(|c| c.trim().is_empty() || c.chars().count() > 100)
        {
            return Err(
                "Pytanie: do 240 znaków, 2–6 niepustych odpowiedzi i wskazana poprawna odpowiedź."
                    .into(),
            );
        }
    }
    Ok(())
}
pub fn starter_questions() -> Vec<Question> {
    [
        (
            "Która gra ma creepery?",
            vec!["Minecraft", "Portal", "Terraria", "Doom"],
            0,
        ),
        (
            "Jak nazywa się konsola Valve do grania przenośnego?",
            vec!["Switch", "Steam Deck", "PS Vita", "Wii U"],
            1,
        ),
        (
            "Która postać jest hydraulikiem?",
            vec!["Link", "Kirby", "Mario", "Sonic"],
            2,
        ),
        ("Ile boków ma sześciokąt?", vec!["4", "5", "6", "8"], 2),
        (
            "Która planeta jest najbliżej Słońca?",
            vec!["Wenus", "Mars", "Ziemia", "Merkury"],
            3,
        ),
        (
            "Która gra ma portale w dwóch kolorach?",
            vec!["Portal", "Hades", "Celeste", "Stardew Valley"],
            0,
        ),
    ]
    .into_iter()
    .map(|(text, choices, correct)| Question {
        text: text.into(),
        choices: choices.into_iter().map(str::to_owned).collect(),
        correct,
    })
    .collect()
}
#[derive(Clone, Serialize)]
pub struct Score {
    pub login: String,
    pub name: String,
    pub points: u64,
}
fn ranking(scores: &HashMap<String, Score>) -> Vec<Score> {
    let mut rows: Vec<_> = scores.values().cloned().collect();
    rows.sort_by(|a, b| b.points.cmp(&a.points).then(a.login.cmp(&b.login)));
    rows.truncate(10);
    rows
}
fn answer(text: &str, boss: bool) -> Option<usize> {
    let text = text.trim().to_lowercase();
    let text = if boss {
        text.strip_prefix("!atak ")?.trim()
    } else {
        text.strip_prefix("!odp ").unwrap_or(&text).trim()
    };
    if text.len() != 1 {
        return None;
    }
    let byte = text.as_bytes()[0];
    (b'a'..=b'f')
        .contains(&byte)
        .then(|| (byte - b'a') as usize)
}
pub struct Quiz {
    pub channel: String,
    pub voice: bool,
    questions: Vec<Question>,
    index: usize,
    phase: &'static str,
    started: i64,
    deadline: i64,
    seconds: u32,
    answered: HashSet<String>,
    scores: HashMap<String, Score>,
}
impl Quiz {
    pub fn new(
        channel: String,
        questions: Vec<Question>,
        seconds: u32,
        voice: bool,
        now: i64,
    ) -> Result<Self, String> {
        validate(&questions)?;
        let seconds = seconds.clamp(10, 120);
        Ok(Self {
            channel,
            voice,
            questions,
            index: 0,
            phase: "question",
            started: now,
            deadline: now + seconds as i64 * 1000,
            seconds,
            answered: HashSet::new(),
            scores: HashMap::new(),
        })
    }
    pub fn next(&mut self, now: i64) -> bool {
        if self.phase == "finished" {
            return false;
        }
        if self.phase == "question" {
            self.phase = "reveal";
            self.deadline = now + 6000;
            return true;
        }
        self.index += 1;
        if self.index >= self.questions.len() {
            self.index = self.questions.len() - 1;
            self.phase = "finished";
            return true;
        }
        self.phase = "question";
        self.started = now;
        self.deadline = now + self.seconds as i64 * 1000;
        self.answered.clear();
        true
    }
    pub fn tick(&mut self, now: i64) -> bool {
        if self.phase != "finished" && now >= self.deadline {
            self.next(now)
        } else {
            false
        }
    }
    pub fn message(
        &mut self,
        channel: &str,
        login: &str,
        name: &str,
        text: &str,
        at: i64,
        now: i64,
    ) -> bool {
        if channel != self.channel
            || self.phase != "question"
            || now >= self.deadline
            || at < self.started
        {
            return false;
        }
        let Some(choice) = answer(text, false) else {
            return false;
        };
        let q = &self.questions[self.index];
        if choice >= q.choices.len() {
            return false;
        }
        let key = login.to_lowercase();
        if !self.answered.insert(key.clone()) {
            return false;
        }
        let points = if choice == q.correct {
            100 + ((self.deadline - now).max(0) / 1000) as u64
        } else {
            0
        };
        let row = self.scores.entry(key.clone()).or_insert(Score {
            login: key,
            name: name.into(),
            points: 0,
        });
        row.points += points;
        true
    }
    pub fn public(&self) -> Value {
        let q = &self.questions[self.index];
        json!({"channel":self.channel,"phase":self.phase,"index":self.index+1,"total":self.questions.len(),"question":q.text,"choices":q.choices,"correct":(self.phase!="question").then_some(q.correct),"deadline":self.deadline,"answers":self.answered.len(),"scores":ranking(&self.scores)})
    }
    pub fn narration(&self) -> String {
        let q = &self.questions[self.index];
        if self.phase == "question" {
            format!(
                "Pytanie {}. {}. {}",
                self.index + 1,
                q.text,
                q.choices
                    .iter()
                    .enumerate()
                    .map(|(i, c)| format!("{}. {c}", (b'A' + i as u8) as char))
                    .collect::<Vec<_>>()
                    .join(". ")
            )
        } else if self.phase == "finished" {
            ranking(&self.scores)
                .first()
                .filter(|s| s.points > 0)
                .map(|s| {
                    format!(
                        "Teleturniej zakończony! Wygrywa {} z wynikiem {} punktów.",
                        s.name, s.points
                    )
                })
                .unwrap_or("Teleturniej zakończony. Brak odpowiedzi.".into())
        } else {
            format!("Poprawna odpowiedź: {}.", q.choices[q.correct])
        }
    }
}
pub struct Boss {
    pub channel: String,
    pub voice: bool,
    pub victory_scene: String,
    name: String,
    hp: u32,
    max_hp: u32,
    questions: Vec<Question>,
    index: usize,
    phase: &'static str,
    started: i64,
    deadline: i64,
    attacked: HashSet<String>,
    scores: HashMap<String, Score>,
    pub revision: u64,
}
impl Boss {
    pub fn phase(&self) -> &'static str {
        self.phase
    }
    pub fn new(
        channel: String,
        name: String,
        hp: u32,
        seconds: u32,
        voice: bool,
        victory_scene: String,
        questions: Vec<Question>,
        now: i64,
    ) -> Result<Self, String> {
        validate(&questions)?;
        let hp = hp.clamp(20, 1000);
        Ok(Self {
            channel,
            name: crate::studio::text(&name, 60),
            voice,
            victory_scene,
            hp,
            max_hp: hp,
            questions,
            index: 0,
            phase: "battle",
            started: now,
            deadline: now + seconds.clamp(30, 600) as i64 * 1000,
            attacked: HashSet::new(),
            scores: HashMap::new(),
            revision: 0,
        })
    }
    pub fn tick(&mut self, now: i64) -> bool {
        if self.phase == "battle" && now >= self.deadline {
            self.phase = "defeat";
            self.revision += 1;
            true
        } else {
            false
        }
    }
    pub fn message(
        &mut self,
        channel: &str,
        login: &str,
        name: &str,
        text: &str,
        at: i64,
        now: i64,
    ) -> bool {
        if channel != self.channel
            || self.phase != "battle"
            || now >= self.deadline
            || at < self.started
        {
            return false;
        }
        let Some(choice) = answer(text, true) else {
            return false;
        };
        let q = &self.questions[self.index];
        if choice >= q.choices.len() {
            return false;
        }
        let key = login.to_lowercase();
        if !self.attacked.insert(key.clone()) {
            return false;
        }
        if choice == q.correct {
            self.hp = self.hp.saturating_sub(10);
            let row = self.scores.entry(key.clone()).or_insert(Score {
                login: key,
                name: name.into(),
                points: 0,
            });
            row.points += 10;
            if self.hp == 0 {
                self.phase = "victory";
            } else {
                self.index = (self.index + 1) % self.questions.len();
                self.attacked.clear();
                self.started = now;
            }
        }
        self.revision += 1;
        true
    }
    pub fn public(&self) -> Value {
        let q = &self.questions[self.index];
        json!({"channel":self.channel,"name":self.name,"phase":self.phase,"hp":self.hp,"max_hp":self.max_hp,"question":q.text,"choices":q.choices,"deadline":self.deadline,"scores":ranking(&self.scores),"revision":self.revision,"correct":(self.phase!="battle").then_some(q.correct)})
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn quiz_automatically_reveals_advances_and_finishes() {
        let mut quiz = Quiz::new(
            "a".into(),
            vec![starter_questions()[0].clone()],
            10,
            false,
            0,
        )
        .unwrap();
        assert!(quiz.tick(10000));
        assert_eq!(quiz.public()["phase"], "reveal");
        assert!(!quiz.tick(15000));
        assert!(quiz.tick(16000));
        assert_eq!(quiz.public()["phase"], "finished");
        assert!(!quiz.tick(999999));
    }
    #[test]
    fn quiz_counts_first_answer_only_and_reveals_no_solution_early() {
        let mut game = Quiz::new("a".into(), starter_questions(), 10, false, 1000).unwrap();
        assert!(game.public()["correct"].is_null());
        assert!(!game.message("b", "u", "U", "A", 1001, 1002));
        assert!(game.message("a", "u", "U", "B", 1001, 1002));
        assert!(!game.message("a", "u", "U", "A", 1002, 1003));
        assert_eq!(game.public()["scores"][0]["points"], 0);
        assert!(game.tick(11000));
        assert_eq!(game.public()["correct"], 0);
        assert!(!game.message("a", "x", "X", "A", 11000, 11000));
    }
    #[test]
    fn boss_requires_command_and_cannot_be_hit_after_victory() {
        let mut game = Boss::new(
            "a".into(),
            "Boss".into(),
            20,
            30,
            false,
            "".into(),
            starter_questions(),
            1000,
        )
        .unwrap();
        assert!(!game.message("a", "u", "U", "A", 1001, 1002));
        assert!(game.message("a", "u", "U", "!atak A", 1001, 1002));
        assert!(game.message("a", "u", "U", "!atak B", 1003, 1004));
        assert_eq!(game.public()["phase"], "victory");
        assert!(!game.message("a", "x", "X", "!atak C", 1005, 1006));
    }
    #[test]
    fn late_messages_and_invalid_question_banks_are_rejected() {
        assert!(
            validate(&[Question {
                text: "x".into(),
                choices: vec!["a".into(), "b".into()],
                correct: 2
            }])
            .is_err()
        );
        let mut game = Quiz::new("a".into(), starter_questions(), 10, false, 1000).unwrap();
        assert!(!game.message("a", "u", "U", "A", 999, 1001));
        assert!(!game.message("a", "u", "U", "A", 11001, 11001));
    }
}
