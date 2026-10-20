//! What DigUp is doing, in the words the tray, the window and the panel use: the index helper's events and the model's
//! download turned into one line, a progress and a caveat for results (the Mac app's AppModel and EngineStatus).

use serde_json::{json, Value};
use std::path::Path;

#[derive(Clone, Debug, Default, PartialEq)]
pub enum Activity {
    /// The index helper hasn't reported yet.
    #[default]
    Starting,
    Idle,
    /// A pass over new or changed files: `done` of `total`.
    Indexing { done: u64, total: u64 },
    /// The rest of long PDFs and documents: `done` of the `total` pages and chunks this pass reads, `files` unfinished.
    Finishing { done: u64, total: u64, files: u64 },
    Failed(String),
}

#[derive(Clone, Debug, Default)]
pub struct Status {
    pub activity: Activity,
    /// Files in the index, in any state.
    pub files: u64,
    /// Done or partly read: what search finds by what's in it.
    pub searchable: u64,
    /// Waiting to be read.
    pub pending: u64,
    /// Long files with pages or chunks still to read.
    pub unfinished: u64,
    /// Chosen folders that aren't there now (an unplugged drive), and ones DigUp can't read: skipped, files kept.
    pub missing: Vec<String>,
    pub unreadable: Vec<String>,
    /// The speed check before the first index is running.
    pub measuring: bool,
    /// Indexing is paused (Pause Indexing): nothing is read until it's resumed; search goes on.
    pub paused: bool,
    /// Outside the set hours (Settings → Folders): when they begin ("01:00").
    pub held_until: Option<String>,
    /// The long file currently being read in the continuation pass.
    pub long_file: Option<LongFileProgress>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LongFileProgress {
    pub file_id: i64,
    pub path: String,
    pub kind: String,
    pub done: u64,
    pub total: u64,
}

impl Status {
    /// An index-service event (`Sources/digup/IndexService.swift`); events about the code index don't count here.
    pub fn apply(&mut self, event: &Value) {
        if event["code"] == true {
            return;
        }
        let count = |key: &str| event[key].as_u64().unwrap_or(0);
        if event["files"].is_u64() {
            self.files = count("files");
            self.searchable = count("searchable");
            self.pending = count("pending");
            self.unfinished = count("unfinished");
        }
        match event["event"].as_str().unwrap_or_default() {
            "sync" if count("new") > 0 => {
                self.activity = Activity::Indexing { done: 0, total: count("new") };
                self.long_file = None;
            }
            // A sync after a failure: the helper works again.
            "sync" if matches!(self.activity, Activity::Failed(_)) => {
                self.activity = Activity::Idle;
                self.long_file = None;
            }
            "file" => {
                self.activity = Activity::Indexing { done: count("done"), total: count("total") };
                self.long_file = None;
                // The counts come with a sync; each file read since leaves "waiting".
                self.pending = self.pending.saturating_sub(1);
                if matches!(event["state"].as_str(), Some("done" | "partial")) {
                    self.searchable += 1;
                }
            }
            "rest" => {
                self.activity = Activity::Finishing { done: count("done"), total: count("total"), files: count("files") };
                let state = event["state"].as_str().unwrap_or_default();
                let clears = matches!(state, "done" | "skipped");
                self.long_file = if clears {
                    None
                } else {
                    event["path"].as_str().map(|path| LongFileProgress {
                        file_id: event["file"].as_i64().unwrap_or(0),
                        path: path.to_string(),
                        kind: event["kind"].as_str().unwrap_or_default().to_string(),
                        done: count("done"),
                        total: count("total"),
                    })
                };
            }
            "idle" | "waiting" => {
                self.activity = Activity::Idle;
                self.long_file = None;
            }
            "missing" => {
                let paths = |key: &str| -> Vec<String> {
                    event[key].as_array().into_iter().flatten().filter_map(Value::as_str).map(String::from).collect()
                };
                self.missing = paths("folders");
                self.unreadable = paths("unreadable");
            }
            "error" if event["fatal"] == true => {
                self.activity = Activity::Failed("Indexing stopped".into());
                self.long_file = None;
            }
            // The helper is gone: whatever it was doing is over.
            "exit" | "ended" => {
                if matches!(self.activity, Activity::Starting | Activity::Indexing { .. } | Activity::Finishing { .. }) {
                    self.activity = Activity::Idle;
                }
                self.long_file = None;
            }
            _ => {}
        }
    }

    /// One line for the tray and the window: "Indexing 42% · 120 of 306", "306 files indexed".
    pub fn line(&self, has_folders: bool, model: &Value) -> String {
        if !has_folders {
            return "No folders chosen yet".into();
        }
        if let Activity::Failed(problem) = &self.activity {
            return format!("⚠︎ {problem}");
        }
        // Nothing is read before the model is in (file names are searchable once indexing starts).
        if let Some(line) = download_line(model) {
            return line;
        }
        if self.measuring {
            return "Checking this computer's speed…".into();
        }
        match self.activity {
            // Held or paused, no index helper runs to say it has started: what's indexed stands.
            Activity::Starting if !self.paused && self.held_until.is_none() => "Starting…".into(),
            Activity::Indexing { done, total } => format!("Indexing {}% · {} of {}", percent(done, total), number(done), number(total)),
            Activity::Finishing { done, total, files } => {
                if let Some(long_file) = &self.long_file {
                    let name = Path::new(&long_file.path)
                        .file_name()
                        .map(|part| part.to_string_lossy().into_owned())
                        .filter(|name| !name.is_empty())
                        .unwrap_or_else(|| long_file.path.clone());
                    format!("Reading long file “{name}” · {}%", percent(done, total))
                } else {
                    let which = if files > 0 {
                        format!("{} long {}", number(files), plural(files, "file", "files"))
                    } else {
                        "long files".into()
                    };
                    format!("Reading the rest of {which} · {}%", percent(done, total))
                }
            }
            _ if self.pending > 0 => {
                let waiting = format!("{} {} waiting", number(self.pending), plural(self.pending, "file", "files"));
                match &self.held_until {
                    _ if self.paused => format!("Paused · {waiting}"),
                    Some(start) => format!("{waiting} · indexing from {start}"),
                    None => format!("{waiting} to be indexed"),
                }
            }
            _ => {
                let indexed = format!("{} {} indexed", number(self.searchable), plural(self.searchable, "file", "files"));
                match &self.held_until {
                    _ if self.paused => format!("{indexed} · paused"),
                    Some(start) => format!("{indexed} · indexing from {start}"),
                    None => indexed,
                }
            }
        }
    }

    /// A few words beside the panel's field while results may be incomplete: "Indexing 42%", "Downloading model · 42%".
    pub fn badge(&self, model: &Value) -> Option<String> {
        if model["state"] == "downloading" {
            return Some(format!("Downloading model · {}%", (fraction(model) * 100.0) as u64));
        }
        if self.paused && self.pending > 0 {
            return Some("Indexing paused".into());
        }
        match self.activity {
            Activity::Indexing { done, total } if total > 0 => Some(format!("Indexing {}%", percent(done, total))),
            _ => None,
        }
    }

    /// Something is under way or can't start: Index Now has nothing to add.
    pub fn busy(&self, model: &Value) -> bool {
        model["state"] != "ready" || self.measuring || self.paused || self.held_until.is_some()
            || matches!(self.activity, Activity::Indexing { .. } | Activity::Finishing { .. })
    }

    /// 0…1 while something is under way (the download, then an indexing pass or the rest of long files).
    pub fn progress(&self, model: &Value) -> Option<f64> {
        if model["state"] == "downloading" {
            return Some(fraction(model));
        }
        match self.activity {
            Activity::Indexing { done, total } | Activity::Finishing { done, total, .. } if total > 0 => {
                Some(done as f64 / total as f64)
            }
            _ => None,
        }
    }

    /// Why results may be incomplete right now: the model isn't in yet, or files aren't read yet.
    pub fn caveat(&self, model: &Value) -> Option<String> {
        match model["state"].as_str() {
            Some("downloading") => {
                return Some(format!("Matching words only until the model has downloaded ({}%)", (fraction(model) * 100.0) as u64))
            }
            Some("waiting" | "failed") => return Some("Matching words only: the model isn't downloaded".into()),
            _ => {}
        }
        if let Activity::Indexing { done, total } = self.activity {
            if total > 0 {
                return Some(format!("Still indexing ({}%): some files aren't in yet", percent(done, total)));
            }
        }
        (self.pending > 0).then(|| {
            format!("{} {} indexed yet", number(self.pending), plural(self.pending, "file isn't", "files aren't"))
        })
    }

    /// What the pages show: the line, the progress, the caveat, and the counts.
    pub fn snapshot(&self, has_folders: bool, model: &Value) -> Value {
        let indexing = match self.activity {
            Activity::Indexing { done, total } => json!({"done": done, "total": total}),
            _ => Value::Null,
        };
        json!({
            "line": self.line(has_folders, model),
            "progress": self.progress(model),
            "caveat": self.caveat(model),
            "indexing": indexing,
            "files": self.files,
            "searchable": self.searchable,
            "pending": self.pending,
            "unfinished": self.unfinished,
            "measuring": self.measuring,
            "paused": self.paused,
            "heldUntil": self.held_until,
            "longFile": self.long_file.as_ref().map(|long_file| json!({
                "fileId": long_file.file_id,
                "path": long_file.path,
                "kind": long_file.kind,
                "done": long_file.done,
                "total": long_file.total,
            })),
            "badge": self.badge(model),
            "busy": self.busy(model),
            "activity": match self.activity {
                Activity::Starting => "starting",
                Activity::Idle => "idle",
                Activity::Indexing { .. } => "indexing",
                Activity::Finishing { .. } => "finishing",
                Activity::Failed(_) => "failed",
            },
            "model": model,
        })
    }
}

/// While the model isn't there, how its download goes.
fn download_line(model: &Value) -> Option<String> {
    match model["state"].as_str()? {
        "downloading" => Some(format!("Downloading the model · {}%", (fraction(model) * 100.0) as u64)),
        "checking" => Some("Checking the model download…".into()),
        "waiting" => Some("Model download: waiting for the network".into()),
        "failed" => Some("⚠︎ The model download stopped".into()),
        _ => None,
    }
}

fn fraction(model: &Value) -> f64 {
    let (bytes, total) = (model["bytes"].as_f64().unwrap_or(0.0), model["total"].as_f64().unwrap_or(0.0));
    if total > 0.0 { (bytes / total).clamp(0.0, 1.0) } else { 0.0 }
}

/// Rounded down, so 100% means done.
fn percent(done: u64, total: u64) -> u64 {
    if total == 0 { 0 } else { done * 100 / total }
}

fn plural(count: u64, one: &'static str, many: &'static str) -> &'static str {
    if count == 1 { one } else { many }
}

/// "1,204"
pub fn number(value: u64) -> String {
    let digits = value.to_string();
    let mut out = String::new();
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines() {
        let ready = json!({"state": "ready"});
        let mut status = Status::default();
        assert_eq!(status.line(false, &ready), "No folders chosen yet");
        assert_eq!(status.line(true, &ready), "Starting…");
        assert_eq!(status.line(true, &json!({"state": "downloading", "bytes": 432.0, "total": 865.0})),
                   "Downloading the model · 49%");
        status.apply(&json!({"event": "sync", "new": 306, "files": 306, "searchable": 0, "pending": 306}));
        assert_eq!(status.line(true, &ready), "Indexing 0% · 0 of 306");
        status.apply(&json!({"event": "file", "done": 129, "total": 306}));
        assert_eq!(status.line(true, &ready), "Indexing 42% · 129 of 306");
        assert_eq!(status.progress(&ready), Some(129.0 / 306.0));
        assert_eq!(status.caveat(&ready).as_deref(), Some("Still indexing (42%): some files aren't in yet"));
        status.apply(&json!({"event": "file", "done": 1, "total": 1, "code": true}));
        assert_eq!(status.line(true, &ready), "Indexing 42% · 129 of 306");
        status.apply(&json!({"event": "rest", "done": 40, "total": 100, "files": 3, "file": 9, "path": "/docs/Team notes 2026.md",
                             "kind": "doc", "state": "partial"}));
        assert_eq!(status.line(true, &ready), "Reading long file “Team notes 2026.md” · 40%");
        assert_eq!(status.snapshot(true, &ready)["longFile"]["fileId"], 9);
        status.apply(&json!({"event": "rest", "done": 70, "total": 100, "files": 2, "file": 9, "path": "/docs/Team notes 2026.md",
                             "kind": "doc", "state": "skipped"}));
        assert!(status.snapshot(true, &ready)["longFile"].is_null());
        status.apply(&json!({"event": "idle", "files": 1306, "searchable": 1306, "pending": 0, "unfinished": 0}));
        assert_eq!(status.line(true, &ready), "1,306 files indexed");
        assert_eq!(status.progress(&ready), None);
        assert_eq!(status.caveat(&ready), None);
        status.apply(&json!({"event": "error", "message": "worker died", "fatal": true}));
        assert_eq!(status.line(true, &ready), "⚠︎ Indexing stopped");

        let mut held = Status { held_until: Some("01:00".into()), ..Status::default() };
        held.apply(&json!({"event": "counts", "files": 70, "searchable": 66, "pending": 4, "unfinished": 0}));
        assert_eq!(held.line(true, &ready), "4 files waiting · indexing from 01:00");
        assert!(held.busy(&ready));
        held.held_until = None;
        held.paused = true;
        assert_eq!(held.line(true, &ready), "Paused · 4 files waiting");
    }

    #[test]
    fn numbers() {
        assert_eq!(number(0), "0");
        assert_eq!(number(999), "999");
        assert_eq!(number(1000), "1,000");
        assert_eq!(number(1234567), "1,234,567");
    }
}
