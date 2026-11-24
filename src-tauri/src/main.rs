#![windows_subsystem = "windows"]

mod cli;

use pixhift_core::{codec, engine, model};

use anyhow::{Context, Result, ensure};
use model::{Progress, Report, Request, Settings};
use std::{
    fs,
    path::{Path, PathBuf},
    process::ExitCode,
    sync::{
        Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};
use tauri::{Manager, State, ipc::Channel};
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_opener::OpenerExt;

type Reply<T> = std::result::Result<T, String>;

#[derive(Default)]
struct Session {
    preview: Mutex<Option<(u64, engine::Plan)>>,
    report: Mutex<Option<Report>>,
    generation: AtomicU64,
    working: AtomicBool,
    cancel: AtomicBool,
}

fn explain(error: impl std::fmt::Display) -> String {
    format!("{error:#}")
}

fn settings_path(app: &tauri::AppHandle) -> Result<PathBuf> {
    Ok(app.path().app_config_dir()?.join("settings.json"))
}

fn validate_settings(mut settings: Settings) -> Result<Settings> {
    ensure!(
        settings.advanced.workers <= 16,
        "Use automatic workers or enter a number from 1 to 16."
    );
    ensure!(
        (128..=8192).contains(&settings.advanced.memory_mib),
        "Memory limit must be between 128 and 8192 MiB."
    );
    if !settings.remember_output {
        settings.last_output = None;
    }
    Ok(settings)
}

#[tauri::command]
fn load_settings(app: tauri::AppHandle) -> Reply<Settings> {
    let path = settings_path(&app).map_err(explain)?;
    match fs::read(path) {
        Ok(bytes) => {
            validate_settings(serde_json::from_slice(&bytes).map_err(explain)?).map_err(explain)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Settings::default()),
        Err(error) => Err(explain(error)),
    }
}

#[tauri::command]
fn save_settings(app: tauri::AppHandle, settings: Settings) -> Reply<()> {
    (|| -> Result<()> {
        let settings = validate_settings(settings)?;
        let path = settings_path(&app)?;
        fs::create_dir_all(path.parent().context("Missing configuration directory")?)?;
        engine::write_atomic(&path, &serde_json::to_vec_pretty(&settings)?)
    })()
    .map_err(explain)
}

#[tauri::command]
async fn pick_folder(app: tauri::AppHandle, output: bool) -> Reply<Option<PathBuf>> {
    tauri::async_runtime::spawn_blocking(move || {
        let title = if output {
            "Choose output folder"
        } else {
            "Choose source folder"
        };
        app.dialog()
            .file()
            .set_title(title)
            .blocking_pick_folder()
            .map(|path| path.into_path().map_err(explain))
            .transpose()
    })
    .await
    .map_err(explain)?
}

#[tauri::command]
async fn dropped_folder(paths: Vec<PathBuf>) -> Reply<PathBuf> {
    if paths.len() != 1 || !paths[0].is_dir() {
        return Err("Drop one folder containing the images to process.".into());
    }
    Ok(paths.into_iter().next().unwrap())
}

#[derive(serde::Serialize)]
struct PreviewReply {
    id: u64,
    preview: model::Preview,
}

#[tauri::command]
async fn analyze(
    app: tauri::AppHandle,
    request: Request,
    settings: Settings,
) -> Reply<PreviewReply> {
    tauri::async_runtime::spawn_blocking(move || {
        let session = app.state::<Session>();
        let settings = validate_settings(settings).map_err(explain)?;
        let id = session.generation.fetch_add(1, Ordering::AcqRel) + 1;
        let plan = engine::plan(request, settings).map_err(explain)?;
        let preview = plan.preview.clone();
        *session.preview.lock().unwrap() = Some((id, plan));
        Ok(PreviewReply { id, preview })
    })
    .await
    .map_err(explain)?
}

struct Busy<'a>(&'a AtomicBool);

impl Drop for Busy<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

#[tauri::command]
async fn process(
    app: tauri::AppHandle,
    id: u64,
    confirmed: bool,
    on_progress: Channel<Progress>,
) -> Reply<Report> {
    let session = app.state::<Session>();
    if session
        .working
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return Err("A batch is already running.".into());
    }
    session.cancel.store(false, Ordering::Release);
    tauri::async_runtime::spawn_blocking(move || {
        let session = app.state::<Session>();
        let _busy = Busy(&session.working);
        let mut pending = session.preview.lock().unwrap();
        if pending
            .as_ref()
            .is_none_or(|(pending_id, _)| *pending_id != id)
        {
            return Err("This preview has expired. Generate a new preview.".into());
        }
        let (_, mut plan) = pending.take().unwrap();
        drop(pending);
        plan.confirm_overwrite(confirmed);
        let report = engine::run(plan, &session.cancel, |progress| {
            let _ = on_progress.send(progress);
        })
        .map_err(explain)?;
        *session.report.lock().unwrap() = Some(report.clone());
        Ok(report)
    })
    .await
    .map_err(explain)?
}

#[tauri::command]
fn cancel_batch(session: State<'_, Session>) {
    session.cancel.store(true, Ordering::Release);
}

#[tauri::command]
fn open_output(app: tauri::AppHandle, session: State<'_, Session>) -> Reply<()> {
    let destination = session
        .report
        .lock()
        .unwrap()
        .as_ref()
        .map(|report| report.destination.clone())
        .ok_or("No report is available.")?;
    let path = destination.to_string_lossy();
    let path = if let Some(unc) = path.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{unc}")
    } else {
        path.strip_prefix(r"\\?\").unwrap_or(&path).to_owned()
    };
    app.opener().open_path(path, None::<&str>).map_err(explain)
}

fn report_bytes(report: &Report, csv: bool) -> Result<Vec<u8>> {
    if csv {
        let mut output = csv::Writer::from_writer(Vec::new());
        output.write_record(["Status", "Source", "Output", "Details"])?;
        for record in &report.entries {
            let path = |path: &Path| path.to_string_lossy().into_owned();
            let escape = |value: &str| {
                if value.starts_with(['=', '+', '-', '@', '\t', '\r', '\n']) {
                    format!("'{value}")
                } else {
                    value.to_owned()
                }
            };
            output.write_record([
                record.status.label().to_owned(),
                escape(&path(&record.source)),
                escape(&record.output.as_deref().map(path).unwrap_or_default()),
                escape(&record.message),
            ])?;
        }
        return Ok(output.into_inner()?);
    }
    use std::fmt::Write;
    let mut text = format!(
        "Pixhift batch report\n\nSuccessful: {}\nFailed: {}\nSkipped: {}\nDuration: {:.2} seconds\nCancelled: {}\nDestination: {}\n\n",
        report.success,
        report.failed,
        report.skipped,
        report.duration_ms as f64 / 1000.0,
        report.cancelled,
        report.destination.display()
    );
    for record in &report.entries {
        writeln!(
            text,
            "{}\t{}",
            record.status.label(),
            record.source.display()
        )?;
        if let Some(path) = &record.output {
            writeln!(text, "  Output: {}", path.display())?;
        }
        if !record.message.is_empty() {
            writeln!(text, "  {}", record.message)?;
        }
    }
    Ok(text.into_bytes())
}

#[tauri::command]
async fn export_report(app: tauri::AppHandle, csv: bool) -> Reply<Option<PathBuf>> {
    tauri::async_runtime::spawn_blocking(move || {
        (|| -> Result<Option<PathBuf>> {
            let report = app
                .state::<Session>()
                .report
                .lock()
                .unwrap()
                .clone()
                .context("No report is available")?;
            let (suffix, filter) = if csv {
                ("csv", "CSV report")
            } else {
                ("txt", "Plain text report")
            };
            let chosen = app
                .dialog()
                .file()
                .set_title("Export Pixhift report")
                .set_file_name(format!("pixhift-report.{suffix}"))
                .add_filter(filter, &[suffix])
                .blocking_save_file();
            let Some(chosen) = chosen else {
                return Ok(None);
            };
            let mut path = chosen.into_path()?;
            if path.extension().is_none() {
                path.set_extension(suffix);
            }
            ensure!(
                path.extension()
                    .and_then(|ext| ext.to_str())
                    .is_some_and(|ext| ext.eq_ignore_ascii_case(suffix)),
                "Choose a filename ending in .{suffix}."
            );
            if path.exists() {
                ensure!(
                    codec::sniff(&path)? == codec::Kind::Other,
                    "A report cannot replace an image."
                );
            }
            engine::write_atomic(&path, &report_bytes(&report, csv)?)?;
            Ok(Some(path))
        })()
        .map_err(explain)
    })
    .await
    .map_err(explain)?
}

fn main() -> ExitCode {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};

        // SAFETY: This uses a reserved process ID and attaches only to an existing console.
        unsafe { AttachConsole(ATTACH_PARENT_PROCESS) };
    }

    if std::env::args_os().nth(1).is_some() {
        return cli::run_from_args();
    }

    run_desktop();
    ExitCode::SUCCESS
}

fn run_desktop() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .manage(Session::default())
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                let session = window.state::<Session>();
                if session.working.load(Ordering::Acquire) {
                    api.prevent_close();
                    session.cancel.store(true, Ordering::Release);
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            load_settings,
            save_settings,
            pick_folder,
            dropped_folder,
            analyze,
            process,
            cancel_batch,
            open_output,
            export_report,
        ])
        .run(tauri::generate_context!())
        .expect("Pixhift could not start");
}
