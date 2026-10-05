// Tauri command handlers must accept owned String for IPC deserialization,
// so clippy's needless_pass_by_value suggestion does not apply here.
#![allow(clippy::needless_pass_by_value)]

mod analysis;
mod parser;

use analysis::{compare_plans, insights, Insight, PlanComparison};
use parser::parse_plan;
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, path::PathBuf, sync::Mutex, time::Duration};
use tauri::{Manager, State};
use tokio::runtime::{Builder as RuntimeBuilder, Runtime};
use turso::{Builder, Connection};
use uuid::Uuid;

type AppResult<T> = Result<T, String>;

struct Backend {
    // A single current-thread executor behind a mutex serializes all database and PG work.
    runtime: Runtime,
    db: Connection,
    db_path: PathBuf,
    // Passwords are process-local only: they are never written to Turso or returned to the UI.
    passwords: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ConnectionInfo {
    host: String,
    port: u16,
    username: String,
    database: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Workflow {
    id: String,
    name: String,
    parent_id: Option<String>,
    host: String,
    port: u16,
    username: String,
    database: String,
    connected: bool,
    windows: Vec<QueryWindow>,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct QueryWindow {
    id: String,
    workflow_id: String,
    parent_id: Option<String>,
    title: String,
    sql: String,
    explain_options: ExplainOptions,
    steps: Vec<Step>,
    notes: Vec<QuickNote>,
}

#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct ExplainOptions {
    verbose: bool,
    buffers: bool,
    wal: bool,
    timing: bool,
    settings: bool,
    costs: bool,
    summary: bool,
}

impl Default for ExplainOptions {
    fn default() -> Self {
        Self {
            verbose: false,
            buffers: true,
            wal: false,
            timing: true,
            settings: false,
            costs: true,
            summary: true,
        }
    }
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct QuickNote {
    id: String,
    window_id: String,
    step_id: Option<String>,
    body: String,
    created_at: String,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct Step {
    id: String,
    window_id: String,
    sql: String,
    note: String,
    plan: Option<serde_json::Value>,
    insights: Vec<Insight>,
    created_at: String,
}

fn id() -> String {
    Uuid::new_v4().to_string()
}
fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

// Earlier versions stored raw PostgreSQL plans. Convert those once on startup
// using the native Rust parser so existing workspaces still open.
async fn migrate_legacy_plans(db: &Connection) -> AppResult<()> {
    let mut rows = db
        .query("SELECT id, sql, plan FROM steps WHERE plan IS NOT NULL", ())
        .await
        .map_err(err)?;
    let mut updates = Vec::new();
    while let Some(row) = rows.next().await.map_err(err)? {
        let id: String = row.get(0).map_err(err)?;
        let sql: String = row.get(1).map_err(err)?;
        let raw: String = row.get(2).map_err(err)?;
        let processed = serde_json::from_str::<serde_json::Value>(&raw)
            .ok()
            .is_some_and(|value| value.get("content").and_then(|v| v.get("Plan")).is_some());
        if !processed {
            match parse_plan(&raw, &sql) {
                Ok(plan) => updates.push((id, plan)),
                Err(error) => eprintln!("Could not migrate saved plan {id}: {error}"),
            }
        }
    }
    drop(rows);
    for (id, plan) in updates {
        db.execute(
            "UPDATE steps SET plan = ?1 WHERE id = ?2",
            [plan.as_str(), id.as_str()],
        )
        .await
        .map_err(err)?;
    }
    Ok(())
}

async fn init_db(db: &Connection) -> AppResult<()> {
    for sql in [
        "PRAGMA foreign_keys = ON",
        "CREATE TABLE IF NOT EXISTS preferences (key TEXT PRIMARY KEY, value TEXT NOT NULL)",
        "CREATE TABLE IF NOT EXISTS workflows (id TEXT PRIMARY KEY, name TEXT NOT NULL, parent_id TEXT, host TEXT NOT NULL DEFAULT '', port INTEGER NOT NULL DEFAULT 5432, username TEXT NOT NULL DEFAULT '', database_name TEXT NOT NULL DEFAULT '', created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP)",
        "CREATE TABLE IF NOT EXISTS query_windows (id TEXT PRIMARY KEY, workflow_id TEXT NOT NULL REFERENCES workflows(id) ON DELETE CASCADE, parent_id TEXT, title TEXT NOT NULL, sql TEXT NOT NULL DEFAULT '', created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP)",
        "CREATE TABLE IF NOT EXISTS steps (id TEXT PRIMARY KEY, window_id TEXT NOT NULL REFERENCES query_windows(id) ON DELETE CASCADE, sql TEXT NOT NULL, note TEXT NOT NULL DEFAULT '', plan TEXT, created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP)",
        "CREATE TABLE IF NOT EXISTS explain_options (window_id TEXT PRIMARY KEY REFERENCES query_windows(id) ON DELETE CASCADE, options TEXT NOT NULL)",
        "CREATE TABLE IF NOT EXISTS quick_notes (id TEXT PRIMARY KEY, window_id TEXT NOT NULL REFERENCES query_windows(id) ON DELETE CASCADE, step_id TEXT REFERENCES steps(id) ON DELETE CASCADE, body TEXT NOT NULL, created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP)",
        "CREATE INDEX IF NOT EXISTS quick_notes_window ON quick_notes(window_id, created_at, id)",
    ] {
        db.execute(sql, ()).await.map_err(err)?;
    }
    Ok(())
}

async fn workspace(
    db: &Connection,
    passwords: &HashMap<String, String>,
) -> AppResult<Vec<Workflow>> {
    let mut workflows = Vec::new();
    let mut rows = db.query("SELECT id, name, parent_id, host, port, username, database_name FROM workflows ORDER BY created_at, rowid", ()).await.map_err(err)?;
    while let Some(row) = rows.next().await.map_err(err)? {
        let id: String = row.get(0).map_err(err)?;
        workflows.push(Workflow {
            connected: passwords.contains_key(&id),
            id,
            name: row.get(1).map_err(err)?,
            parent_id: row.get(2).map_err(err)?,
            host: row.get(3).map_err(err)?,
            port: u16::try_from(row.get::<i64>(4).map_err(err)?).unwrap_or(5432),
            username: row.get(5).map_err(err)?,
            database: row.get(6).map_err(err)?,
            windows: vec![],
        });
    }
    for workflow in &mut workflows {
        let mut rows = db.query("SELECT id, parent_id, title, sql FROM query_windows WHERE workflow_id = ?1 ORDER BY created_at, rowid", [workflow.id.as_str()]).await.map_err(err)?;
        while let Some(row) = rows.next().await.map_err(err)? {
            let window_id: String = row.get(0).map_err(err)?;
            workflow.windows.push(QueryWindow {
                id: window_id,
                workflow_id: workflow.id.clone(),
                parent_id: row.get(1).map_err(err)?,
                title: row.get(2).map_err(err)?,
                sql: row.get(3).map_err(err)?,
                explain_options: ExplainOptions::default(),
                steps: vec![],
                notes: vec![],
            });
        }
        for window in &mut workflow.windows {
            let mut option_rows = db
                .query(
                    "SELECT options FROM explain_options WHERE window_id = ?1",
                    [window.id.as_str()],
                )
                .await
                .map_err(err)?;
            if let Some(row) = option_rows.next().await.map_err(err)? {
                let raw: String = row.get(0).map_err(err)?;
                window.explain_options = serde_json::from_str(&raw).unwrap_or_default();
            }
            let mut rows = db.query("SELECT id, sql, note, plan, created_at FROM steps WHERE window_id = ?1 ORDER BY created_at, rowid", [window.id.as_str()]).await.map_err(err)?;
            while let Some(row) = rows.next().await.map_err(err)? {
                let plan = row.get::<Option<String>>(3).map_err(err)?.and_then(|text| {
                    serde_json::from_str::<serde_json::Value>(&text)
                        .ok()
                        .filter(|value| value.get("content").and_then(|v| v.get("Plan")).is_some())
                });
                let found = plan.as_ref().map(insights).unwrap_or_default();
                window.steps.push(Step {
                    id: row.get(0).map_err(err)?,
                    window_id: window.id.clone(),
                    sql: row.get(1).map_err(err)?,
                    note: row.get(2).map_err(err)?,
                    plan,
                    insights: found,
                    created_at: row.get(4).map_err(err)?,
                });
            }
            let mut note_rows = db.query("SELECT id, step_id, body, created_at FROM quick_notes WHERE window_id = ?1 ORDER BY created_at, id", [window.id.as_str()]).await.map_err(err)?;
            while let Some(row) = note_rows.next().await.map_err(err)? {
                window.notes.push(QuickNote {
                    id: row.get(0).map_err(err)?,
                    window_id: window.id.clone(),
                    step_id: row.get(1).map_err(err)?,
                    body: row.get(2).map_err(err)?,
                    created_at: row.get(3).map_err(err)?,
                });
            }
        }
    }
    Ok(workflows)
}

fn locked<T>(
    state: State<'_, Mutex<Backend>>,
    f: impl FnOnce(&mut Backend) -> AppResult<T>,
) -> AppResult<T> {
    let mut backend = state
        .lock()
        .map_err(|_| "Backend lock poisoned".to_string())?;
    f(&mut backend)
}

#[tauri::command]
fn workspace_database_path(state: State<'_, Mutex<Backend>>) -> AppResult<String> {
    locked(state, |b| Ok(b.db_path.display().to_string()))
}

#[tauri::command]
fn open_workspace_database(state: State<'_, Mutex<Backend>>, path: String) -> AppResult<()> {
    let path = std::fs::canonicalize(path).map_err(err)?;
    if !path.is_file() {
        return Err("Select an existing Pow database file".into());
    }
    locked(state, |b| {
        if b.db_path == path {
            return Ok(());
        }
        let new_connection = b.runtime.block_on(async {
            let database = Builder::new_local(path.to_string_lossy().as_ref())
                .build()
                .await
                .map_err(err)?;
            let connection = database.connect().map_err(err)?;
            drop(database);
            // Reject unrelated files instead of silently creating an empty workspace in them.
            let mut check = connection
                .query("SELECT id FROM workflows LIMIT 1", ())
                .await
                .map_err(|_| "This is not a Pow workspace database".to_string())?;
            check
                .next()
                .await
                .map_err(|_| "This is not a Pow workspace database".to_string())?;
            drop(check);
            init_db(&connection).await?;
            migrate_legacy_plans(&connection).await?;
            Ok::<Connection, String>(connection)
        })?;
        b.db = new_connection;
        b.db_path = path;
        b.passwords.clear();
        Ok(())
    })
}

#[tauri::command]
fn load_workspace(state: State<'_, Mutex<Backend>>) -> AppResult<Vec<Workflow>> {
    locked(state, |b| {
        b.runtime.block_on(workspace(&b.db, &b.passwords))
    })
}

async fn saved_plan(db: &Connection, id: &str) -> AppResult<(String, serde_json::Value)> {
    let mut rows = db
        .query("SELECT window_id, plan FROM steps WHERE id = ?1", [id])
        .await
        .map_err(err)?;
    let row = rows.next().await.map_err(err)?.ok_or("Step not found")?;
    let window_id: String = row.get(0).map_err(err)?;
    let raw: Option<String> = row.get(1).map_err(err)?;
    let plan: serde_json::Value =
        serde_json::from_str(&raw.ok_or("This step has no plan")?).map_err(err)?;
    if plan.get("content").and_then(|v| v.get("Plan")).is_none() {
        return Err("Saved plan has not been processed".into());
    }
    Ok((window_id, plan))
}

async fn compare_saved_steps(
    db: &Connection,
    baseline_id: &str,
    current_id: &str,
) -> AppResult<PlanComparison> {
    if baseline_id == current_id {
        return Err("Select two different steps".into());
    }
    let (old_window, old_plan) = saved_plan(db, baseline_id).await?;
    let (new_window, new_plan) = saved_plan(db, current_id).await?;
    if old_window != new_window {
        return Err("Compare steps from the same query tab".into());
    }
    Ok(compare_plans(&old_plan, &new_plan))
}

#[tauri::command]
fn compare_steps(
    state: State<'_, Mutex<Backend>>,
    baseline_id: String,
    current_id: String,
) -> AppResult<PlanComparison> {
    locked(state, |b| {
        b.runtime
            .block_on(compare_saved_steps(&b.db, &baseline_id, &current_id))
    })
}

#[tauri::command]
fn load_theme(state: State<'_, Mutex<Backend>>) -> AppResult<String> {
    locked(state, |b| {
        b.runtime.block_on(async {
            let mut rows =
                b.db.query("SELECT value FROM preferences WHERE key = 'theme'", ())
                    .await
                    .map_err(err)?;
            rows.next().await.map_err(err)?.map_or_else(|| Ok("nord".to_owned()), |row| row.get(0).map_err(err))
        })
    })
}

const THEMES: &[&str] = &[
    "gruvbox",
    "nord",
    "modus-operandi",
    "modus-vivendi",
    "one-dark",
    "kanagawa",
    "kanso",
    "rose-pine",
    "solarized-dark",
    "solarized-light",
    "github-dark",
    "github-light",
];

#[tauri::command]
fn save_theme(state: State<'_, Mutex<Backend>>, theme: String) -> AppResult<()> {
    if !THEMES.contains(&theme.as_str()) {
        return Err("Unknown theme".into());
    }
    locked(state, |b| {
        b.runtime.block_on(async {
            b.db.execute("INSERT INTO preferences (key, value) VALUES ('theme', ?1) ON CONFLICT(key) DO UPDATE SET value = excluded.value", [theme.as_str()]).await.map_err(err)?;
            Ok(())
        })
    })
}

#[tauri::command]
fn create_workflow(state: State<'_, Mutex<Backend>>, name: String) -> AppResult<String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("Give the workflow a name".into());
    }
    locked(state, |b| {
        let new_id = id();
        b.runtime.block_on(async {
            b.db.execute(
                "INSERT INTO workflows (id, name) VALUES (?1, ?2)",
                [new_id.as_str(), name],
            )
            .await
            .map_err(err)
        })?;
        Ok(new_id)
    })
}

#[tauri::command]
fn create_window(
    state: State<'_, Mutex<Backend>>,
    workflow_id: String,
    title: String,
) -> AppResult<String> {
    locked(state, |b| {
        let new_id = id();
        b.runtime.block_on(async {
            b.db.execute(
                "INSERT INTO query_windows (id, workflow_id, title) VALUES (?1, ?2, ?3)",
                [new_id.as_str(), workflow_id.as_str(), title.as_str()],
            )
            .await
            .map_err(err)
        })?;
        Ok(new_id)
    })
}

#[tauri::command]
fn save_draft(
    state: State<'_, Mutex<Backend>>,
    window_id: String,
    sql: String,
    title: String,
) -> AppResult<()> {
    locked(state, |b| {
        let changed = b.runtime.block_on(async {
            b.db.execute(
                "UPDATE query_windows SET sql = ?1, title = ?2 WHERE id = ?3",
                [sql.as_str(), title.as_str(), window_id.as_str()],
            )
            .await
            .map_err(err)
        })?;
        if changed == 0 {
            return Err("Query window not found".into());
        }
        Ok(())
    })
}

#[tauri::command]
fn save_explain_options(
    state: State<'_, Mutex<Backend>>,
    window_id: String,
    options: ExplainOptions,
) -> AppResult<()> {
    locked(state, |b| {
        b.runtime.block_on(async {
        let json = serde_json::to_string(&options).map_err(err)?;
        b.db.execute("INSERT INTO explain_options (window_id, options) VALUES (?1, ?2) ON CONFLICT(window_id) DO UPDATE SET options = excluded.options", [window_id.as_str(), json.as_str()]).await.map_err(err)?;
        Ok(())
    })
    })
}

#[tauri::command]
fn add_quick_note(
    state: State<'_, Mutex<Backend>>,
    window_id: String,
    step_id: Option<String>,
    body: String,
) -> AppResult<String> {
    let body = body.trim();
    if body.is_empty() || body.len() > 20_000 {
        return Err("Note must have 1 to 20,000 characters".into());
    }
    locked(state, |b| {
        b.runtime.block_on(async {
            if let Some(ref step_id) = step_id {
                let mut rows =
                    b.db.query(
                        "SELECT id FROM steps WHERE id = ?1 AND window_id = ?2",
                        [step_id.as_str(), window_id.as_str()],
                    )
                    .await
                    .map_err(err)?;
                if rows.next().await.map_err(err)?.is_none() {
                    return Err("Step does not belong to this query tab".into());
                }
            }
            let note_id = id();
            b.db.execute(
                "INSERT INTO quick_notes (id, window_id, step_id, body) VALUES (?1, ?2, ?3, ?4)",
                turso::params![note_id.clone(), window_id, step_id, body.to_owned()],
            )
            .await
            .map_err(err)?;
            Ok(note_id)
        })
    })
}

#[tauri::command]
fn save_step(
    state: State<'_, Mutex<Backend>>,
    window_id: String,
    sql: String,
    note: String,
) -> AppResult<String> {
    if sql.trim().is_empty() {
        return Err("Write a query first".into());
    }
    locked(state, |b| {
        let new_id = id();
        b.runtime.block_on(async {
            let tx = b.db.transaction().await.map_err(err)?;
            tx.execute(
                "UPDATE query_windows SET sql = ?1 WHERE id = ?2",
                [sql.as_str(), window_id.as_str()],
            )
            .await
            .map_err(err)?;
            tx.execute(
                "INSERT INTO steps (id, window_id, sql, note) VALUES (?1, ?2, ?3, ?4)",
                [
                    new_id.as_str(),
                    window_id.as_str(),
                    sql.as_str(),
                    note.as_str(),
                ],
            )
            .await
            .map_err(err)?;
            tx.commit().await.map_err(err)
        })?;
        Ok(new_id)
    })
}

async fn copy_window(
    db: &Connection,
    old: &QueryWindow,
    target_workflow: &str,
) -> AppResult<String> {
    let new_id = id();
    db.execute("INSERT INTO query_windows (id, workflow_id, parent_id, title, sql) VALUES (?1, ?2, ?3, ?4, ?5)", [new_id.as_str(), target_workflow, old.id.as_str(), format!("{} · fork", old.title).as_str(), old.sql.as_str()]).await.map_err(err)?;
    let options = serde_json::to_string(&old.explain_options).map_err(err)?;
    db.execute(
        "INSERT INTO explain_options (window_id, options) VALUES (?1, ?2)",
        [new_id.as_str(), options.as_str()],
    )
    .await
    .map_err(err)?;
    let mut copied_steps = HashMap::new();
    for step in &old.steps {
        let copied_id = id();
        db.execute(
            "INSERT INTO steps (id, window_id, sql, note, plan) VALUES (?1, ?2, ?3, ?4, ?5)",
            turso::params![
                copied_id.clone(),
                new_id.clone(),
                step.sql.clone(),
                step.note.clone(),
                step.plan.as_ref().map(serde_json::Value::to_string)
            ],
        )
        .await
        .map_err(err)?;
        copied_steps.insert(step.id.clone(), copied_id);
    }
    for note in &old.notes {
        let step = note
            .step_id
            .as_ref()
            .and_then(|original| copied_steps.get(original))
            .cloned();
        db.execute(
            "INSERT INTO quick_notes (id, window_id, step_id, body) VALUES (?1, ?2, ?3, ?4)",
            turso::params![id(), new_id.clone(), step, note.body.clone()],
        )
        .await
        .map_err(err)?;
    }
    Ok(new_id)
}

#[tauri::command]
fn fork_window(state: State<'_, Mutex<Backend>>, window_id: String) -> AppResult<String> {
    locked(state, |b| {
        b.runtime.block_on(async {
            let all = workspace(&b.db, &b.passwords).await?;
            let (workflow, old) = all
                .iter()
                .find_map(|w| w.windows.iter().find(|q| q.id == window_id).map(|q| (w, q)))
                .ok_or("Query window not found")?;
            b.db.execute("BEGIN", ()).await.map_err(err)?;
            let result = copy_window(&b.db, old, &workflow.id).await;
            match result {
                Ok(new_id) => {
                    b.db.execute("COMMIT", ()).await.map_err(err)?;
                    Ok(new_id)
                }
                Err(e) => {
                    let _ = b.db.execute("ROLLBACK", ()).await;
                    Err(e)
                }
            }
        })
    })
}

#[tauri::command]
fn fork_workflow(state: State<'_, Mutex<Backend>>, workflow_id: String) -> AppResult<String> {
    locked(state, |b| {
        b.runtime.block_on(async {
        let all = workspace(&b.db, &b.passwords).await?;
        let old = all.iter().find(|w| w.id == workflow_id).ok_or("Workflow not found")?;
        let new_id = id();
        b.db.execute("BEGIN", ()).await.map_err(err)?;
        let result = async {
            b.db.execute("INSERT INTO workflows (id, name, parent_id, host, port, username, database_name) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)", turso::params![new_id.clone(), format!("{} · fork", old.name), old.id.clone(), old.host.clone(), i64::from(old.port), old.username.clone(), old.database.clone()]).await.map_err(err)?;
            for window in &old.windows { copy_window(&b.db, window, &new_id).await?; }
            Ok::<(), String>(())
        }.await;
        match result {
            Ok(()) => {
                b.db.execute("COMMIT", ()).await.map_err(err)?;
                if let Some(password) = b.passwords.get(&workflow_id).cloned() { b.passwords.insert(new_id.clone(), password); }
                Ok(new_id)
            }
            Err(e) => { let _ = b.db.execute("ROLLBACK", ()).await; Err(e) }
        }
    })
    })
}

#[tauri::command]
fn close_window(state: State<'_, Mutex<Backend>>, window_id: String) -> AppResult<()> {
    locked(state, |b| {
        b.runtime.block_on(async {
            b.db.execute(
                "DELETE FROM query_windows WHERE id = ?1",
                [window_id.as_str()],
            )
            .await
            .map_err(err)
        })?;
        Ok(())
    })
}

#[tauri::command]
fn delete_workflow(state: State<'_, Mutex<Backend>>, workflow_id: String) -> AppResult<()> {
    locked(state, |b| {
        b.runtime.block_on(async {
            b.db.execute(
                "DELETE FROM workflows WHERE id = ?1",
                [workflow_id.as_str()],
            )
            .await
            .map_err(err)
        })?;
        b.passwords.remove(&workflow_id);
        Ok(())
    })
}

async fn postgres(
    info: &ConnectionInfo,
    password: &str,
) -> AppResult<(tokio_postgres::Client, tokio::task::JoinHandle<()>)> {
    let mut config = tokio_postgres::Config::new();
    config
        .host(&info.host)
        .port(info.port)
        .user(&info.username)
        .dbname(&info.database)
        .password(password)
        .connect_timeout(Duration::from_secs(8));
    let (client, connection) = config.connect(tokio_postgres::NoTls).await.map_err(err)?;
    let task = tokio::spawn(async move {
        if let Err(e) = connection.await {
            eprintln!("PostgreSQL connection: {e}");
        }
    });
    Ok((client, task))
}

fn validate_connection(info: &ConnectionInfo) -> AppResult<()> {
    if info.host.trim().is_empty()
        || info.username.trim().is_empty()
        || info.database.trim().is_empty()
        || info.port == 0
    {
        return Err("Host, port, username and database are required".into());
    }
    Ok(())
}

#[tauri::command]
fn connect_workflow(
    state: State<'_, Mutex<Backend>>,
    workflow_id: String,
    info: ConnectionInfo,
    password: String,
) -> AppResult<()> {
    validate_connection(&info)?;
    locked(state, |b| {
        b.runtime.block_on(async {
            let (client, task) = postgres(&info, &password).await?;
            let check = client.simple_query("SELECT current_database()").await.map_err(err);
            task.abort();
            check?;
            let changed = b.db.execute("UPDATE workflows SET host = ?1, port = ?2, username = ?3, database_name = ?4 WHERE id = ?5", turso::params![info.host, i64::from(info.port), info.username, info.database, workflow_id.clone()]).await.map_err(err)?;
            if changed == 0 { return Err::<(), String>("Workflow not found".into()); }
            Ok(())
        })?;
        b.passwords.insert(workflow_id, password);
        Ok(())
    })
}

fn explain_statement(query: &str, analyze: bool, options: ExplainOptions) -> String {
    let mut flags = vec!["FORMAT JSON"];
    if analyze {
        flags.push("ANALYZE");
    }
    if options.verbose {
        flags.push("VERBOSE");
    }
    if options.settings {
        flags.push("SETTINGS");
    }
    if !options.costs {
        flags.push("COSTS OFF");
    }
    if analyze {
        if options.buffers {
            flags.push("BUFFERS");
        }
        if options.wal {
            flags.push("WAL");
        }
        if !options.timing {
            flags.push("TIMING OFF");
        }
        if !options.summary {
            flags.push("SUMMARY OFF");
        }
    }
    format!("EXPLAIN ({}) {query}", flags.join(", "))
}

#[tauri::command]
fn run_explain(
    state: State<'_, Mutex<Backend>>,
    window_id: String,
    sql: String,
    note: String,
    analyze: bool,
    options: ExplainOptions,
) -> AppResult<String> {
    let query = sql.trim();
    if query.is_empty() {
        return Err("Write a query first".into());
    }
    // ANALYZE actually executes SQL. Only read-only statements in a read-only transaction are allowed.
    if analyze
        && !query.to_ascii_lowercase().starts_with("select ")
        && !query.to_ascii_lowercase().starts_with("with ")
    {
        return Err("ANALYZE is restricted to SELECT/WITH queries".into());
    }
    locked(state, |b| {
        b.runtime.block_on(async {
            let all = workspace(&b.db, &b.passwords).await?;
            let workflow = all
                .iter()
                .find(|w| w.windows.iter().any(|q| q.id == window_id))
                .ok_or("Query window not found")?;
            let password = b
                .passwords
                .get(&workflow.id)
                .ok_or("Connect to PostgreSQL first (passwords are not saved to disk)")?;
            let info = ConnectionInfo {
                host: workflow.host.clone(),
                port: workflow.port,
                username: workflow.username.clone(),
                database: workflow.database.clone(),
            };
            let (client, task) = postgres(&info, password).await?;
            let result = async {
                client
                    .batch_execute("BEGIN READ ONLY; SET LOCAL statement_timeout = '15s'")
                    .await
                    .map_err(err)?;
                // query_one uses the extended protocol, disallowing stacked statements.
                let explain = explain_statement(query, analyze, options);
                let row = client.query_one(&explain, &[]).await.map_err(err)?;
                let value: serde_json::Value = row.try_get(0).map_err(err)?;
                Ok::<String, String>(value.to_string())
            }
            .await;
            let _ = client.batch_execute("ROLLBACK").await;
            task.abort();
            let raw_plan = result?;
            let plan = parse_plan(&raw_plan, &sql)?;
            let step_id = id();
            let tx = b.db.transaction().await.map_err(err)?;
            tx.execute(
                "UPDATE query_windows SET sql = ?1 WHERE id = ?2",
                [sql.as_str(), window_id.as_str()],
            )
            .await
            .map_err(err)?;
            let saved_options = serde_json::to_string(&options).map_err(err)?;
            tx.execute("INSERT INTO explain_options (window_id, options) VALUES (?1, ?2) ON CONFLICT(window_id) DO UPDATE SET options = excluded.options", [window_id.as_str(), saved_options.as_str()]).await.map_err(err)?;
            tx.execute(
                "INSERT INTO steps (id, window_id, sql, note, plan) VALUES (?1, ?2, ?3, ?4, ?5)",
                [
                    step_id.as_str(),
                    window_id.as_str(),
                    sql.as_str(),
                    note.as_str(),
                    plan.as_str(),
                ],
            )
            .await
            .map_err(err)?;
            tx.commit().await.map_err(err)?;
            Ok(step_id)
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_parser_runs_in_backend_for_json_and_text() {
        let json = parse_plan(r#"[{"Plan":{"Node Type":"Seq Scan","Relation Name":"items","Plan Rows":12,"Total Cost":8}}]"#, "SELECT * FROM items").unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["content"]["Plan"]["Relation Name"], "items");
        assert_eq!(parsed["content"]["maxTotalCost"], 8);
        let text = parse_plan(
            "Seq Scan on items  (cost=0.00..8.00 rows=12 width=4)",
            "SELECT * FROM items",
        )
        .unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed["content"]["Plan"]["Node Type"], "Seq Scan");
        assert!(parse_plan("this is not a plan", "SELECT 1").is_err());
    }

    #[test]
    fn comparison_reads_saved_steps_and_rejects_other_tabs() {
        let runtime = RuntimeBuilder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let database = Builder::new_local(":memory:").build().await.unwrap();
            let db = database.connect().unwrap();
            init_db(&db).await.unwrap();
            db.execute("INSERT INTO workflows (id, name) VALUES ('w', 'Test')", ()).await.unwrap();
            db.execute("INSERT INTO query_windows (id, workflow_id, title) VALUES ('tab1', 'w', 'First')", ()).await.unwrap();
            db.execute("INSERT INTO query_windows (id, workflow_id, title) VALUES ('tab2', 'w', 'Other')", ()).await.unwrap();
            let earlier = r#"{"content":{"Execution Time":100,"maxTotalCost":20,"Plan":{"Node Type":"Seq Scan"}}}"#;
            let later = r#"{"content":{"Execution Time":50,"maxTotalCost":10,"Plan":{"Node Type":"Index Scan"}}}"#;
            for (id, window, plan) in [("a", "tab1", earlier), ("b", "tab1", later), ("c", "tab2", later)] {
                db.execute("INSERT INTO steps (id, window_id, sql, plan) VALUES (?1, ?2, 'SELECT 1', ?3)", [id, window, plan]).await.unwrap();
            }
            let diff = compare_saved_steps(&db, "a", "b").await.unwrap();
            assert_eq!(diff.metrics[0].direction, "improved");
            assert_eq!(diff.node_changes.len(), 2);
            assert!(compare_saved_steps(&db, "a", "c").await.is_err());
            assert!(compare_saved_steps(&db, "a", "a").await.is_err());
        });
    }

    #[test]
    fn local_database_survives_reopening() {
        let runtime = RuntimeBuilder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let path = std::env::temp_dir().join(format!("pow-test-{}.db", id()));
        runtime.block_on(async {
            {
                let database = Builder::new_local(path.to_str().unwrap())
                    .build()
                    .await
                    .unwrap();
                let db = database.connect().unwrap();
                init_db(&db).await.unwrap();
                db.execute("INSERT INTO preferences (key, value) VALUES ('theme', ?1) ON CONFLICT(key) DO UPDATE SET value = excluded.value", ["github-light"]).await.unwrap();
                db.execute(
                    "INSERT INTO workflows (id, name) VALUES (?1, ?2)",
                    [id().as_str(), "Saved"],
                )
                .await
                .unwrap();
            }
            let database = Builder::new_local(path.to_str().unwrap())
                .build()
                .await
                .unwrap();
            let db = database.connect().unwrap();
            init_db(&db).await.unwrap();
            let saved = workspace(&db, &HashMap::new()).await.unwrap();
            assert_eq!(saved[0].name, "Saved");
            assert!(!saved[0].connected);
            let mut rows = db.query("SELECT value FROM preferences WHERE key = 'theme'", ()).await.unwrap();
            assert_eq!(rows.next().await.unwrap().unwrap().get::<String>(0).unwrap(), "github-light");
        });
        // Keep the temporary fixture: validation must not delete user files.
    }

    #[test]
    fn explain_options_are_valid_and_analyze_only_flags_stay_guarded() {
        let opts = ExplainOptions {
            verbose: true,
            wal: true,
            timing: false,
            costs: false,
            summary: false,
            settings: true,
            ..ExplainOptions::default()
        };
        let plain = explain_statement("SELECT 1", false, opts);
        assert!(plain.contains("VERBOSE"));
        assert!(plain.contains("SETTINGS"));
        assert!(plain.contains("COSTS OFF"));
        assert!(!plain.contains("WAL"));
        assert!(!plain.contains("BUFFERS"));
        assert!(!plain.contains("TIMING"));
        let analyzed = explain_statement("SELECT 1", true, opts);
        assert!(analyzed.contains("ANALYZE"));
        assert!(analyzed.contains("BUFFERS"));
        assert!(analyzed.contains("WAL"));
        assert!(analyzed.contains("TIMING OFF"));
        assert!(analyzed.contains("SUMMARY OFF"));
    }

    #[test]
    fn persists_workflows_and_forks_independent_history() {
        let runtime = RuntimeBuilder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let database = Builder::new_local(":memory:").build().await.unwrap();
            let db = database.connect().unwrap();
            init_db(&db).await.unwrap();
            let workflow_id = id();
            db.execute(
                "INSERT INTO workflows (id, name) VALUES (?1, ?2)",
                [workflow_id.as_str(), "Test"],
            )
            .await
            .unwrap();
            let window_id = id();
            db.execute(
                "INSERT INTO query_windows (id, workflow_id, title, sql) VALUES (?1, ?2, ?3, ?4)",
                [
                    window_id.as_str(),
                    workflow_id.as_str(),
                    "Query",
                    "SELECT 1",
                ],
            )
            .await
            .unwrap();
            let first_step = id();
            db.execute(
                "INSERT INTO steps (id, window_id, sql, note, plan) VALUES (?1, ?2, ?3, ?4, ?5)",
                [
                    first_step.as_str(),
                    window_id.as_str(),
                    "SELECT 1",
                    "initial",
                    "[{\"Plan\":{\"Node Type\":\"Result\"}}]",
                ],
            )
            .await
            .unwrap();
            db.execute("INSERT INTO explain_options (window_id, options) VALUES (?1, ?2)", [window_id.as_str(), r#"{"verbose":true,"buffers":true,"wal":false,"timing":true,"settings":false,"costs":true,"summary":true}"#]).await.unwrap();
            db.execute("INSERT INTO quick_notes (id, window_id, step_id, body) VALUES (?1, ?2, ?3, ?4)", [id().as_str(), window_id.as_str(), first_step.as_str(), "check index selectivity"]).await.unwrap();
            migrate_legacy_plans(&db).await.unwrap();
            let original = workspace(&db, &HashMap::new()).await.unwrap();
            assert!(original[0].windows[0].explain_options.verbose);
            assert_eq!(original[0].windows[0].notes[0].body, "check index selectivity");
            assert_eq!(original[0].windows[0].steps.len(), 1);
            assert_eq!(
                original[0].windows[0].steps[0].plan.as_ref().unwrap()["content"]["Plan"]
                    ["Node Type"],
                "Result"
            );
            db.execute("BEGIN", ()).await.unwrap();
            let fork_id = copy_window(&db, &original[0].windows[0], &workflow_id)
                .await
                .unwrap();
            db.execute("COMMIT", ()).await.unwrap();
            let copied = workspace(&db, &HashMap::new()).await.unwrap();
            let fork = copied[0].windows.iter().find(|w| w.id == fork_id).unwrap();
            assert_eq!(fork.parent_id.as_deref(), Some(window_id.as_str()));
            assert_eq!(fork.steps[0].plan, original[0].windows[0].steps[0].plan);
            assert!(fork.explain_options.verbose);
            assert_eq!(fork.notes[0].body, "check index selectivity");
            assert_eq!(fork.notes[0].step_id.as_deref(), Some(fork.steps[0].id.as_str()));
            db.execute(
                "DELETE FROM query_windows WHERE id = ?1",
                [window_id.as_str()],
            )
            .await
            .unwrap();
            let remaining = workspace(&db, &HashMap::new()).await.unwrap();
            assert_eq!(remaining[0].windows.len(), 1);
            assert_eq!(remaining[0].windows[0].steps.len(), 1);
            assert_eq!(remaining[0].windows[0].notes.len(), 1);
        });
    }
}

#[allow(clippy::missing_panics_doc)]
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            let data_dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&data_dir)?;
            let path = data_dir.join("pow.turso");
            let runtime = RuntimeBuilder::new_current_thread().enable_all().build()?;
            let db = runtime
                .block_on(async {
                    let db = Builder::new_local(path.to_string_lossy().as_ref())
                        .build()
                        .await
                        .map_err(err)?;
                    let connection = db.connect().map_err(err)?;
                    drop(db);
                    init_db(&connection).await?;
                    migrate_legacy_plans(&connection).await?;
                    Ok::<Connection, String>(connection)
                })
                .map_err(std::io::Error::other)?;
            app.manage(Mutex::new(Backend {
                runtime,
                db,
                db_path: path,
                passwords: HashMap::new(),
            }));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            workspace_database_path,
            open_workspace_database,
            load_workspace,
            compare_steps,
            load_theme,
            save_theme,
            create_workflow,
            create_window,
            save_draft,
            save_explain_options,
            add_quick_note,
            save_step,
            fork_window,
            fork_workflow,
            close_window,
            delete_workflow,
            connect_workflow,
            run_explain
        ])
        .run(tauri::generate_context!())
        .expect("failed to run Pow");
}
