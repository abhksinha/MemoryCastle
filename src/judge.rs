//! An opt-in judge: TypeSafe AI's Jev scores how much each note matters for a
//! task (`links <symbol> --for "<task>"`), and in the edit gate, with the
//! session's last prompt as the task, also whether the edit brings back what
//! a note rejected. The only network calls at query time; they send the task,
//! the edit and the notes' full text to api.typesafe.ai, key from
//! `TYPESAFE_API_KEY`. Any error, a missing key or a timeout returns `Err`
//! and the caller prints what it prints without a judge. The gate uses it
//! only after `memcastle project judge on`. Questions and rubric are those
//! measured in `examples/eval_links_judge.py` (`request`), word for word.

use crate::error::Result;
use rusqlite::Connection;

#[cfg(feature = "judge")]
const API: &str = "https://api.typesafe.ai/v1/systemone";

/// Five levels, read as score / 4 (0 to 1).
const RUBRIC: [&str; 5] = [
    "Unrelated to the task.",
    "About this code, but nothing in it bears on the task.",
    "Background: useful context that would not change how the task is done.",
    "Relevant: a decision, limit or measurement the change should respect.",
    "Must read: the task changes or reverses what this note decided, or retries what it rejected.",
];

/// The judge's scores, one per note in order; per note the probability that
/// the task changes or retries what it decided or rejected; with an edit, the
/// probability that the edit brings back what it rejected; the model that answered.
pub struct Verdict {
    pub scores: Vec<f32>,
    pub reverses: Vec<f32>,
    pub rejected: Vec<f32>,
    pub model: String,
}

/// The edit the gate is about to let through, as the judge sees it.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct EditText {
    pub file: String,
    pub line: Option<u32>,
    pub old: String,
    pub new: String,
}

pub fn key() -> Option<String> {
    std::env::var("TYPESAFE_API_KEY").ok().filter(|k| !k.trim().is_empty())
}

/// The `meta` key holding a project's judge setting.
pub fn mode_key(project_id: i64) -> String {
    format!("judge_mode:{project_id}")
}

/// `on` or `off`: `MEMORYCASTLE_JUDGE` when set, else `memcastle project
/// judge`, else `off`. Off by default: it sends notes to a vendor.
pub fn mode(conn: &Connection, project_id: i64) -> Result<String> {
    if let Ok(v) = std::env::var("MEMORYCASTLE_JUDGE") {
        return Ok(v);
    }
    Ok(crate::db::meta_get(conn, &mode_key(project_id))?.unwrap_or_else(|| "off".into()))
}

/// The gate judges: the project turned it on and a key is set.
pub fn enabled(conn: &Connection, project_id: i64) -> Result<bool> {
    Ok(cfg!(feature = "judge") && mode(conn, project_id)? == "on" && key().is_some())
}

fn prompt_key(session: &str) -> String {
    format!("prompt:{session}")
}

/// Keeps the prompt's prose (`search::recall_query`) as the session's task.
pub fn remember_prompt(conn: &Connection, session: &str, prose: &str) -> Result<()> {
    let t = prose.trim();
    if t.is_empty() {
        return Ok(());
    }
    let cut: String = t.chars().take(crate::limits::JUDGE_TASK_CHARS).collect();
    crate::db::meta_set(conn, &prompt_key(session), &cut)
}

pub fn last_prompt(conn: &Connection, session: &str) -> Result<Option<String>> {
    crate::db::meta_get(conn, &prompt_key(session))
}

/// The edit the gate hands its `links` lookup (`MEMORYCASTLE_JUDGE_EDIT`, JSON).
pub fn edit_from_env() -> Option<EditText> {
    std::env::var("MEMORYCASTLE_JUDGE_EDIT").ok().and_then(|v| serde_json::from_str(&v).ok())
}

fn clip(s: &str) -> String {
    s.chars().take(crate::limits::JUDGE_EDIT_CHARS).collect()
}

/// What the master question weighs for Note `k`, each a precise yes/no: the
/// first ones raise its score, the last two lower it. With an edit, also
/// whether the edit brings back what the note rejected.
fn considerations(k: usize, edit: bool) -> (Vec<String>, Vec<String>) {
    let mut raises = vec![
        format!("Note {k} decides, rejects or measures something about the exact code this task changes: the same function, constant or behaviour, not only the same file."),
        format!("Note {k} states a decision, limit or measured number that the change must stay within."),
        reverses(k),
    ];
    if edit {
        raises.push(brings_back(k));
    }
    let lowers = vec![
        format!("Note {k} shares only names, files or words with the task; what it decides is about something else."),
        format!("Note {k} is history or context only: the change would be written the same way without reading it."),
    ];
    (raises, lowers)
}

fn reverses(k: usize) -> String {
    format!("The task changes or reverses what Note {k} decided, or retries something Note {k} rejected.")
}

fn brings_back(k: usize) -> String {
    format!("The edit brings back an approach that Note {k}'s Rejected section ruled out.")
}

/// The request body, one call: per note a master score whose structured
/// instructions carry the considerations to weigh (Jev judges every question
/// in isolation, so the master cannot read other questions' answers; the
/// considerations travel inside it), `v` the reverses-it noul printed as the
/// reason, and with an edit `r` the brings-back-a-rejected-approach noul.
pub fn body(model: &str, symbol: &str, task: &str, notes: &[(String, String)], edit: Option<&EditText>) -> serde_json::Value {
    use serde_json::{json, Map};
    let head = match edit {
        Some(e) => format!(
            "Task: {task}\nThe edit, to {} (line {}):\n--- old\n{}\n+++ new\n{}",
            e.file,
            e.line.map(|l| l.to_string()).unwrap_or_else(|| "?".into()),
            clip(&e.old),
            clip(&e.new)
        ),
        None => format!("Task on `{symbol}`: {task}"),
    };
    let mut state = vec![head];
    let mut questions = Map::new();
    for (i, (path, text)) in notes.iter().enumerate() {
        let k = i + 1;
        state.push(format!("Note {k} ({path}):\n{}", text.trim()));
        let (raises, lowers) = considerations(k, edit.is_some());
        let with_edit = if edit.is_some() { " with this edit" } else { "" };
        questions.insert(
            format!("m{k}"),
            json!({"type": "score", "criteria": RUBRIC, "instructions": {
                "question": format!("How much does Note {k} matter for doing this task{with_edit}?"),
                "weigh": {"raises": raises, "lowers": lowers},
                "reply": format!("Weigh each of these about Note {k}, then answer with the one level that fits the balance."),
            }}),
        );
        questions.insert(format!("v{k}"), json!({"type": "noul", "instructions": reverses(k)}));
        if edit.is_some() {
            questions.insert(format!("r{k}"), json!({"type": "noul", "instructions": brings_back(k)}));
        }
    }
    json!({"model": model, "state": state.join("\n\n"), "questions": questions})
}

/// Scores `notes` (path, full text) for a task on `symbol`, or for `edit`.
#[cfg(feature = "judge")]
pub fn score(symbol: &str, task: &str, notes: &[(String, String)], edit: Option<&EditText>) -> Result<Verdict> {
    use serde_json::Value;
    let key = key().ok_or_else(|| crate::err!("TYPESAFE_API_KEY is not set"))?;
    let model = std::env::var("MEMORYCASTLE_JUDGE_MODEL").unwrap_or_else(|_| "jev-latest".into());
    let request = body(&model, symbol, task, notes, edit).to_string();
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_millis(crate::limits::JUDGE_TIMEOUT_MS)))
        .http_status_as_error(false)
        .build()
        .into();
    let mut resp = agent
        .post(API)
        .header("Authorization", &format!("Bearer {key}"))
        .header("Content-Type", "application/json")
        .send(request)
        .map_err(|e| crate::err!("judge: {e}"))?;
    let status = resp.status().as_u16();
    let text = resp.body_mut().read_to_string().map_err(|e| crate::err!("judge: {e}"))?;
    if status != 200 {
        return Err(crate::err!("judge: HTTP {status}"));
    }
    let v: Value = serde_json::from_str(&text)?;
    let answers = v.get("answers").ok_or_else(|| crate::err!("judge: no answers"))?;
    let get = |q: String, field: &str| answers.get(q).and_then(|a| a.get(field)).and_then(Value::as_f64).map(|s| s as f32);
    let scores = (1..=notes.len())
        .map(|k| get(format!("m{k}"), "score").map(|s| s / 4.0))
        .collect::<Option<Vec<f32>>>()
        .ok_or_else(|| crate::err!("judge: a note was not scored"))?;
    let reverses = (1..=notes.len()).map(|k| get(format!("v{k}"), "noul").unwrap_or(0.0)).collect();
    let rejected = if edit.is_some() { (1..=notes.len()).map(|k| get(format!("r{k}"), "noul").unwrap_or(0.0)).collect() } else { Vec::new() };
    Ok(Verdict { scores, reverses, rejected, model: v.get("model").and_then(Value::as_str).unwrap_or("").to_string() })
}

#[cfg(not(feature = "judge"))]
pub fn score(_symbol: &str, _task: &str, _notes: &[(String, String)], _edit: Option<&EditText>) -> Result<Verdict> {
    Err(crate::err!("judge: this build has no judge (cargo feature `judge`)"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn body_is_the_measured_request() {
        let notes = vec![("memorycastle/journal/a.md".to_string(), "# A\n\n## Decision\nX.\n".to_string())];
        let b = body("jev-latest", "search::merge_queries", "order by RRF", &notes, None);
        assert_eq!(b["state"], "Task on `search::merge_queries`: order by RRF\n\nNote 1 (memorycastle/journal/a.md):\n# A\n\n## Decision\nX.");
        assert_eq!(b["questions"]["m1"]["criteria"][4], RUBRIC[4]);
        assert_eq!(b["questions"]["m1"]["instructions"]["question"], "How much does Note 1 matter for doing this task?");
        assert_eq!(b["questions"]["m1"]["instructions"]["weigh"]["raises"].as_array().unwrap().len(), 3);
        assert_eq!(b["questions"]["v1"]["instructions"], "The task changes or reverses what Note 1 decided, or retries something Note 1 rejected.");
        assert!(b["questions"].get("r1").is_none());
        let e = EditText { file: "src/limits.rs".into(), line: Some(55), old: "0.36".into(), new: "0.40".into() };
        let b = body("jev-latest", "x", "raise the floor", &notes, Some(&e));
        assert!(b["state"].as_str().unwrap().starts_with("Task: raise the floor\nThe edit, to src/limits.rs (line 55):\n--- old\n0.36\n+++ new\n0.40\n\nNote 1"));
        assert_eq!(b["questions"]["r1"]["instructions"], "The edit brings back an approach that Note 1's Rejected section ruled out.");
        assert_eq!(b["questions"]["m1"]["instructions"]["weigh"]["raises"][3], b["questions"]["r1"]["instructions"]);
        assert_eq!(b["questions"]["m1"]["instructions"]["question"], "How much does Note 1 matter for doing this task with this edit?");
    }
}
