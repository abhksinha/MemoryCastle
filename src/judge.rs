//! An opt-in judge: TypeSafe AI's Jev scores how much each note matters for a
//! task (`links <symbol> --for "<task>"`). The only network call at query
//! time; it sends the task and the notes' full text to api.typesafe.ai, key
//! from `TYPESAFE_API_KEY`. Any error, a missing key or a timeout returns
//! `Err` and the caller prints what it prints without a judge. Never on a
//! hook. Questions and rubric are those measured in
//! `examples/eval_links_judge.py`, word for word.

use crate::error::Result;

#[cfg(feature = "judge")]
const API: &str = "https://api.typesafe.ai/v1/systemone";

/// Five levels, read as score / 4 (0 to 1).
#[cfg(feature = "judge")]
const RUBRIC: [&str; 5] = [
    "Unrelated to the task.",
    "About this code, but nothing in it bears on the task.",
    "Background: useful context that would not change how the task is done.",
    "Relevant: a decision, limit or measurement the change should respect.",
    "Must read: the task changes or reverses what this note decided, or retries what it rejected.",
];

/// The judge's scores, one per note in order, and the model that answered.
pub struct Verdict {
    pub scores: Vec<f32>,
    pub model: String,
}

pub fn key() -> Option<String> {
    std::env::var("TYPESAFE_API_KEY").ok().filter(|k| !k.trim().is_empty())
}

/// Scores `notes` (path, full text) for a task on `symbol`.
#[cfg(feature = "judge")]
pub fn score(symbol: &str, task: &str, notes: &[(String, String)]) -> Result<Verdict> {
    use serde_json::{json, Map, Value};
    let key = key().ok_or_else(|| crate::err!("TYPESAFE_API_KEY is not set"))?;
    let mut state = vec![format!("Task on `{symbol}`: {task}")];
    let mut questions = Map::new();
    for (i, (path, text)) in notes.iter().enumerate() {
        let k = i + 1;
        state.push(format!("Note {k} ({path}):\n{}", text.trim()));
        questions.insert(
            format!("m{k}"),
            json!({"type": "score", "instructions": format!("How much does Note {k} matter for doing this task?"), "criteria": RUBRIC}),
        );
    }
    let model = std::env::var("MEMORYCASTLE_JUDGE_MODEL").unwrap_or_else(|_| "jev-latest".into());
    let body = json!({"model": model, "state": state.join("\n\n"), "questions": questions}).to_string();
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_millis(crate::limits::JUDGE_TIMEOUT_MS)))
        .http_status_as_error(false)
        .build()
        .into();
    let mut resp = agent
        .post(API)
        .header("Authorization", &format!("Bearer {key}"))
        .header("Content-Type", "application/json")
        .send(body)
        .map_err(|e| crate::err!("judge: {e}"))?;
    let status = resp.status().as_u16();
    let text = resp.body_mut().read_to_string().map_err(|e| crate::err!("judge: {e}"))?;
    if status != 200 {
        return Err(crate::err!("judge: HTTP {status}"));
    }
    let v: Value = serde_json::from_str(&text)?;
    let answers = v.get("answers").ok_or_else(|| crate::err!("judge: no answers"))?;
    let scores = (1..=notes.len())
        .map(|k| answers.get(format!("m{k}")).and_then(|a| a.get("score")).and_then(Value::as_f64).map(|s| s as f32 / 4.0))
        .collect::<Option<Vec<f32>>>()
        .ok_or_else(|| crate::err!("judge: a note was not scored"))?;
    Ok(Verdict { scores, model: v.get("model").and_then(Value::as_str).unwrap_or("").to_string() })
}

#[cfg(not(feature = "judge"))]
pub fn score(_symbol: &str, _task: &str, _notes: &[(String, String)]) -> Result<Verdict> {
    Err(crate::err!("judge: this build has no judge (cargo feature `judge`)"))
}
