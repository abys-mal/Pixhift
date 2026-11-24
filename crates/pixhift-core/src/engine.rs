use crate::{
    codec::{self, Kind},
    metadata,
    model::{
        Conflict, Destination, Entry, Format, MetadataMode, Preview, Progress, Report, Request,
        Settings, Status,
    },
};
use anyhow::{Context, Result, bail, ensure};
use std::{
    collections::HashSet,
    ffi::OsString,
    fs::{self, Metadata},
    io,
    path::{Component, Path, PathBuf},
    sync::{
        Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Instant, SystemTime},
};
use tempfile::NamedTempFile;

const MIB: u64 = 1024 * 1024;

#[derive(Debug, PartialEq, Eq, Clone)]
struct Stamp {
    size: u64,
    modified: Option<SystemTime>,
}

impl From<&Metadata> for Stamp {
    fn from(metadata: &Metadata) -> Self {
        Self {
            size: metadata.len(),
            modified: metadata.modified().ok(),
        }
    }
}

struct Input {
    path: PathBuf,
    format: Format,
    stamp: Stamp,
    dimensions: (u32, u32),
}

pub struct Task {
    input: Input,
    output: PathBuf,
    format: Format,
    replace: Option<Stamp>,
    estimate: u64,
}

pub struct Plan {
    pub preview: Preview,
    tasks: Vec<Task>,
    entries: Vec<Entry>,
    request: Request,
    settings: Settings,
}

fn link(metadata: &Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}

fn hidden(path: &Path, metadata: &Metadata) -> bool {
    if path
        .file_name()
        .is_some_and(|name| name.to_string_lossy().starts_with('.'))
    {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 2 != 0
    }
    #[cfg(not(windows))]
    {
        let _ = metadata;
        false
    }
}

fn key(path: &Path) -> OsString {
    if cfg!(windows) || cfg!(target_os = "macos") {
        path.to_string_lossy().to_lowercase().into()
    } else {
        path.as_os_str().to_owned()
    }
}

fn inside(path: &Path, root: &Path) -> bool {
    Path::new(&key(path)).starts_with(Path::new(&key(root)))
}

fn directory(path: &Path) -> Result<PathBuf> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("Cannot open folder {}", path.display()))?;
    ensure!(
        metadata.is_dir() && !link(&metadata),
        "Select a directory directly. Links and junctions are skipped."
    );
    Ok(fs::canonicalize(path)?)
}

fn entry(path: &Path, status: Status, message: impl Into<String>) -> Entry {
    Entry {
        source: path.to_path_buf(),
        output: None,
        status,
        message: message.into(),
    }
}

struct Scan {
    found: Vec<Input>,
    source_paths: HashSet<OsString>,
    entries: Vec<Entry>,
    preview: Preview,
}

fn scan(source: &Path, settings: &Settings) -> Scan {
    let mut scan = Scan {
        found: Vec::new(),
        source_paths: HashSet::new(),
        entries: Vec::new(),
        preview: Preview::default(),
    };
    let mut pending = vec![source.to_path_buf()];
    let max_file = u64::from(settings.advanced.memory_mib) * MIB / 3;
    while let Some(folder) = pending.pop() {
        let list = match fs::read_dir(&folder) {
            Ok(list) => list,
            Err(error) => {
                scan.preview.unreadable += 1;
                scan.entries.push(entry(
                    &folder,
                    Status::Failed,
                    format!("Cannot read folder: {error}"),
                ));
                continue;
            }
        };
        for item in list {
            let path = match item {
                Ok(item) => item.path(),
                Err(error) => {
                    scan.preview.unreadable += 1;
                    scan.entries.push(entry(
                        &folder,
                        Status::Failed,
                        format!("Cannot read directory entry: {error}"),
                    ));
                    continue;
                }
            };
            let info = match fs::symlink_metadata(&path) {
                Ok(info) => info,
                Err(error) => {
                    scan.preview.unreadable += 1;
                    scan.entries.push(entry(
                        &path,
                        Status::Failed,
                        format!("Cannot access file: {error}"),
                    ));
                    continue;
                }
            };
            scan.source_paths.insert(key(&path));
            if link(&info) || (settings.advanced.ignore_hidden && hidden(&path, &info)) {
                scan.preview.ignored += 1;
                scan.entries.push(entry(
                    &path,
                    Status::Skipped,
                    "Link, junction or ignored hidden entry.",
                ));
            } else if info.is_dir() {
                if settings.recursive {
                    pending.push(path);
                }
            } else if info.is_file() {
                match codec::sniff(&path) {
                    Ok(Kind::Supported(format)) => {
                        scan.preview.supported += 1;
                        *scan.preview.inputs.entry(format).or_default() += 1;
                        let result = if info.len() > max_file {
                            Err(anyhow::anyhow!(
                                "Encoded file exceeds the current memory profile."
                            ))
                        } else {
                            std::panic::catch_unwind(|| codec::dimensions(&path, format, max_file))
                                .unwrap_or_else(|_| {
                                    Err(anyhow::anyhow!(
                                        "The image header reader rejected this file."
                                    ))
                                })
                        };
                        match result {
                            Ok(dimensions) => scan.found.push(Input {
                                path,
                                format,
                                stamp: Stamp::from(&info),
                                dimensions,
                            }),
                            Err(error) => {
                                scan.preview.unreadable += 1;
                                scan.entries.push(entry(
                                    &path,
                                    Status::Failed,
                                    format!("Cannot read image header: {error:#}"),
                                ));
                            }
                        }
                    }
                    Ok(Kind::Other) => {
                        scan.preview.non_images += 1;
                        scan.entries
                            .push(entry(&path, Status::Skipped, "Non-image file."));
                    }
                    Ok(Kind::Unsupported) => {
                        scan.preview.unsupported += 1;
                        scan.entries.push(entry(
                            &path,
                            Status::Skipped,
                            "Unsupported image format.",
                        ));
                    }
                    Ok(Kind::Invalid) => {
                        scan.preview.unreadable += 1;
                        scan.entries.push(entry(
                            &path,
                            Status::Failed,
                            "The file does not contain a recognized image header.",
                        ));
                    }
                    Err(error) => {
                        scan.preview.unreadable += 1;
                        scan.entries.push(entry(
                            &path,
                            Status::Failed,
                            format!("Cannot read file: {error:#}"),
                        ));
                    }
                }
            }
        }
    }
    scan.found.sort_by(|a, b| a.path.cmp(&b.path));
    scan
}

fn choose_target(
    mut target: PathBuf,
    reserved: &HashSet<OsString>,
    source_paths: &HashSet<OsString>,
    conflict: Conflict,
) -> Result<Option<(PathBuf, Option<Stamp>)>> {
    let stem = target
        .file_stem()
        .context("Output has no filename")?
        .to_owned();
    let extension = target.extension().map(|ext| ext.to_owned());
    for number in 1..=100_000 {
        let path_key = key(&target);
        let blocked = reserved.contains(&path_key) || source_paths.contains(&path_key);
        let exists = match fs::symlink_metadata(&target) {
            Ok(metadata) => Some(metadata),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        if !blocked && exists.is_none() {
            return Ok(Some((target, None)));
        }
        match conflict {
            Conflict::Replace if !blocked => {
                let info = exists.as_ref().unwrap();
                ensure!(
                    info.is_file() && !link(info),
                    "The existing destination is a link or directory."
                );
                return Ok(Some((target, Some(Stamp::from(info)))));
            }
            Conflict::Replace | Conflict::Skip => return Ok(None),
            Conflict::Rename => {
                let mut name = stem.clone();
                name.push(format!(" ({})", number + 1));
                if let Some(ext) = &extension {
                    name.push(".");
                    name.push(ext);
                }
                target.set_file_name(name);
            }
        }
    }
    bail!("No available filename was found.")
}

fn path_through(root: &Path, parent: &Path, create: bool) -> Result<()> {
    let relative = parent
        .strip_prefix(root)
        .context("The destination escaped its selected folder")?;
    let mut current = root.to_path_buf();
    ensure!(
        directory(root)? == root,
        "The selected output folder has moved."
    );
    for component in relative.components() {
        ensure!(
            matches!(component, Component::Normal(_)),
            "Invalid output folder."
        );
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(metadata) => ensure!(
                metadata.is_dir() && !link(&metadata),
                "An output folder is a link, junction or file."
            ),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                if !create {
                    return Ok(());
                }
                if let Err(error) = fs::create_dir(&current)
                    && error.kind() != io::ErrorKind::AlreadyExists
                {
                    return Err(error.into());
                }
                let info = fs::symlink_metadata(&current)?;
                ensure!(
                    info.is_dir() && !link(&info),
                    "An output folder changed while processing."
                );
            }
            Err(error) => return Err(error.into()),
        }
    }
    ensure!(
        inside(&fs::canonicalize(parent)?, root),
        "The destination escaped its selected folder."
    );
    Ok(())
}

pub fn plan(request: Request, settings: Settings) -> Result<Plan> {
    ensure!(
        request.format.is_none_or(Format::writable),
        "Select a supported output format."
    );
    if request.metadata == MetadataMode::Edit {
        metadata::validate_edits(&request.edits)?;
    }
    let source = directory(&request.source)?;
    let destination = if request.destination == Destination::Folder {
        let destination = directory(
            request
                .output
                .as_deref()
                .context("Choose an output folder")?,
        )?;
        ensure!(
            !inside(&destination, &source) && !inside(&source, &destination),
            "Choose an output folder outside the source folder tree."
        );
        destination
    } else {
        source.clone()
    };
    let Scan {
        found,
        source_paths,
        mut entries,
        mut preview,
    } = scan(&source, &settings);
    preview.destination = destination.clone();
    preview.metadata = request.metadata;
    let mut tasks = Vec::new();
    let mut reserved = HashSet::new();
    let changes = request.metadata != MetadataMode::Preserve
        && (request.metadata != MetadataMode::Edit || !request.edits.is_empty());
    for input in found {
        let format = request.format.unwrap_or(input.format);
        let same = format == input.format;
        let in_place = request.destination == Destination::Overwrite && same;
        if in_place && !changes {
            entries.push(entry(
                &input.path,
                Status::Skipped,
                "Original already matches the requested format and metadata.",
            ));
            continue;
        }
        let directory = if request.destination != Destination::Folder {
            input.path.parent().unwrap().to_path_buf()
        } else if settings.structure {
            destination.join(input.path.parent().unwrap().strip_prefix(&source)?)
        } else {
            destination.clone()
        };
        let filename = input.path.file_name().context("Image has no filename")?;
        let mut target = directory.join(filename);
        if !same {
            target.set_extension(format.extension());
        }
        if let Err(error) = path_through(&destination, &directory, false) {
            entries.push(entry(
                &input.path,
                Status::Failed,
                format!("Unsafe output path: {error:#}"),
            ));
            continue;
        }
        let replacement = if in_place {
            Some(input.stamp.clone())
        } else {
            if reserved.contains(&key(&target))
                || source_paths.contains(&key(&target))
                || fs::symlink_metadata(&target).is_ok()
            {
                preview.conflicts += 1;
            }
            match choose_target(target, &reserved, &source_paths, request.conflict) {
                Ok(Some((path, replacement))) => {
                    target = path;
                    replacement
                }
                Ok(None) => {
                    entries.push(entry(
                        &input.path,
                        Status::Skipped,
                        "Filename conflict. Source and batch outputs are protected.",
                    ));
                    continue;
                }
                Err(error) => {
                    entries.push(entry(
                        &input.path,
                        Status::Failed,
                        format!("Cannot use output path: {error:#}"),
                    ));
                    continue;
                }
            }
        };
        reserved.insert(key(&target));
        *preview.outputs.entry(format).or_default() += 1;
        let estimate = codec::estimate(
            input.stamp.size,
            input.dimensions,
            input.format,
            format,
            &request,
        );
        tasks.push(Task {
            input,
            output: target,
            format,
            replace: replacement,
            estimate,
        });
    }
    preview.scheduled = tasks.len();
    if preview.non_images != 0 {
        let count = preview.non_images;
        let noun = if count == 1 { "file" } else { "files" };
        preview
            .warnings
            .push(format!("{count} non-image {noun} will be skipped."));
    }
    if request.destination == Destination::Overwrite {
        preview.warnings.push("Same-format metadata edits may replace originals. Conversions create sibling files and retain their sources.".into());
    }
    if request.conflict == Conflict::Replace && preview.conflicts != 0 {
        preview.warnings.push("Existing destination files may be replaced. Source files and other outputs in this batch are protected.".into());
    }
    if preview.inputs.contains_key(&Format::Heic)
        && request.format.is_some_and(|f| f != Format::Heic)
    {
        preview.warnings.push("HEIC export uses the primary still image. HDR and image sequences may require a different workflow.".into());
    }
    if request.format.is_some() {
        preview.warnings.push("Pixhift blocks conversions that would discard animation frames or TIFF pages. Format-specific metadata may be lost.".into());
    }
    if request.format == Some(Format::Jpeg) {
        preview.warnings.push(
            if settings.advanced.flatten_alpha {
                "JPEG transparency will be flattened onto white."
            } else {
                "Images with transparency will fail JPEG conversion. PNG and WebP support alpha."
            }
            .into(),
        );
    }
    if request.format == Some(Format::Gif) {
        preview
            .warnings
            .push("GIF uses palette color and one-bit transparency.".into());
    }
    if request.metadata == MetadataMode::RemoveAll {
        preview.warnings.push("Removing embedded color profiles can change the appearance. Display orientation is preserved where possible.".into());
    }
    if request.metadata == MetadataMode::RemoveGps {
        preview.warnings.push("GPS removal covers standard EXIF and XMP fields. Proprietary blocks and free text can still contain location details.".into());
    }
    if changes
        && request.format.is_none()
        && preview
            .inputs
            .keys()
            .any(|f| !metadata::supports_rewrite(*f))
    {
        preview.warnings.push("Some original formats have limited metadata editing. Convert those images to JPEG or PNG to edit their metadata.".into());
    }
    preview.samples = entries
        .iter()
        .filter(|entry| entry.message != "Non-image file.")
        .take(10)
        .cloned()
        .collect();
    Ok(Plan {
        preview,
        tasks,
        entries,
        request,
        settings,
    })
}

fn verify(path: &Path, stamp: &Stamp) -> Result<()> {
    let info = fs::symlink_metadata(path)?;
    ensure!(
        info.is_file() && !link(&info) && Stamp::from(&info) == *stamp,
        "The file changed after preview. Run a new preview."
    );
    Ok(())
}

fn process_task(task: &Task, plan: &Plan, cancel: &AtomicBool) -> Result<Vec<String>> {
    let budget = u64::from(plan.settings.advanced.memory_mib) * MIB;
    ensure!(
        task.estimate <= budget,
        "Estimated memory exceeds the selected limit. Raise the memory limit in Advanced Settings."
    );
    ensure!(
        fs::canonicalize(&task.input.path)? == task.input.path,
        "The source path changed after preview."
    );
    verify(&task.input.path, &task.input.stamp)?;
    let parent = task
        .output
        .parent()
        .context("Output has no parent folder")?;
    path_through(&plan.preview.destination, parent, true)?;
    let mut temporary = tempfile::Builder::new()
        .prefix(".pixhift-")
        .suffix(".tmp")
        .tempfile_in(parent)?;
    let warnings = codec::render(
        &task.input.path,
        task.input.format,
        task.format,
        &plan.request,
        &plan.settings.advanced,
        plan.settings.advanced.memory_mib,
        temporary.as_file_mut(),
    )?;
    temporary.as_file().sync_all()?;
    ensure!(
        codec::sniff(temporary.path())? == Kind::Supported(task.format),
        "The temporary output did not validate as the requested format."
    );
    if task.input.format != task.format || plan.request.metadata != MetadataMode::Preserve {
        codec::dimensions(temporary.path(), task.format, budget / 3)
            .context("Cannot read the temporary output header")?;
    }
    verify(&task.input.path, &task.input.stamp)?;
    path_through(&plan.preview.destination, parent, false)?;
    if cancel.load(Ordering::Acquire) {
        bail!("Cancelled before saving.");
    }
    if let Some(stamp) = &task.replace {
        verify(&task.output, stamp)?;
        let permissions = fs::metadata(&task.output)?.permissions();
        temporary.as_file().set_permissions(permissions)?;
        temporary
            .persist(&task.output)
            .map_err(|error| error.error)?;
    } else {
        temporary
            .persist_noclobber(&task.output)
            .map_err(|error| error.error)?;
    }
    Ok(warnings)
}

struct Queue {
    next: usize,
    available: u64,
    completed: usize,
    entries: Vec<Entry>,
}

pub fn run(plan: Plan, cancel: &AtomicBool, progress: impl Fn(Progress) + Sync) -> Result<Report> {
    ensure!(
        plan.request.destination != Destination::Overwrite || plan.request.confirmed_overwrite,
        "Confirm overwrite mode before processing."
    );
    let start = Instant::now();
    let memory = u64::from(plan.settings.advanced.memory_mib) * MIB;
    let cpus = std::thread::available_parallelism().map_or(1, |cpu| cpu.get());
    let configured = usize::from(plan.settings.advanced.workers);
    let automatic = cpus.min(8).min((memory / (96 * MIB)).max(1) as usize);
    let workers = if configured == 0 {
        automatic
    } else {
        configured.min(16)
    };
    let total = plan.tasks.len();
    if plan.settings.advanced.diagnostics {
        eprintln!(
            "Pixhift: {total} scheduled images, up to {workers} workers, {memory} bytes estimated memory budget."
        );
    }
    let queue = Mutex::new(Queue {
        next: 0,
        available: memory,
        completed: 0,
        entries: Vec::new(),
    });
    let changed = Condvar::new();
    std::thread::scope(|scope| {
        for _ in 0..workers.min(total) {
            scope.spawn(|| {
                loop {
                    let (task, reservation) = {
                        let mut state = queue.lock().unwrap();
                        loop {
                            if cancel.load(Ordering::Acquire) || state.next == total {
                                return;
                            }
                            let task = &plan.tasks[state.next];
                            let reservation = task.estimate.min(memory);
                            if reservation <= state.available {
                                state.available -= reservation;
                                state.next += 1;
                                break (task, reservation);
                            }
                            state = changed.wait(state).unwrap();
                        }
                    };
                    let result = std::panic::catch_unwind(|| process_task(task, &plan, cancel))
                        .unwrap_or_else(|_| {
                            Err(anyhow::anyhow!(
                                "The image codec stopped while processing this file."
                            ))
                        });
                    let output = match result {
                        Ok(warnings) => Entry {
                            source: task.input.path.clone(),
                            output: Some(task.output.clone()),
                            status: Status::Success,
                            message: warnings.join(" "),
                        },
                        Err(error)
                            if cancel.load(Ordering::Acquire)
                                && error.to_string() == "Cancelled before saving." =>
                        {
                            entry(
                                &task.input.path,
                                Status::Skipped,
                                "Cancelled before saving.",
                            )
                        }
                        Err(error) => entry(&task.input.path, Status::Failed, format!("{error:#}")),
                    };
                    let mut state = queue.lock().unwrap();
                    if plan.settings.advanced.diagnostics && output.status == Status::Failed {
                        eprintln!("Pixhift: {}: {}", output.source.display(), output.message);
                    }
                    state.entries.push(output);
                    state.available += reservation;
                    state.completed += 1;
                    progress(Progress {
                        completed: state.completed,
                        total,
                        file: task.input.path.clone(),
                    });
                    changed.notify_all();
                }
            });
        }
    });
    let Queue {
        next,
        entries: completed,
        ..
    } = queue.into_inner().unwrap();
    let mut entries = plan.entries;
    entries.extend(completed);
    entries.extend(plan.tasks.iter().skip(next).map(|task| {
        entry(
            &task.input.path,
            Status::Skipped,
            "Cancelled before processing.",
        )
    }));
    entries.sort_by(|a, b| a.source.cmp(&b.source));
    Ok(Report {
        success: entries
            .iter()
            .filter(|record| record.status == Status::Success)
            .count(),
        failed: entries
            .iter()
            .filter(|record| record.status == Status::Failed)
            .count(),
        skipped: entries
            .iter()
            .filter(|record| record.status == Status::Skipped)
            .count(),
        duration_ms: start.elapsed().as_millis() as u64,
        cancelled: cancel.load(Ordering::Acquire),
        destination: plan.preview.destination,
        entries,
    })
}

pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let parent = path.parent().context("File has no parent folder")?;
    let mut temporary = NamedTempFile::new_in(parent)?;
    temporary.write_all(bytes)?;
    temporary.as_file().sync_all()?;
    temporary.persist(path).map_err(|error| error.error)?;
    Ok(())
}

impl Plan {
    pub fn confirm_overwrite(&mut self, confirmed: bool) {
        self.request.confirmed_overwrite = confirmed;
    }
}
