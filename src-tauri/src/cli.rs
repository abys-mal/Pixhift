use anyhow::{Context, Result, ensure};
use clap::{Args, Parser, Subcommand, ValueEnum};
use pixhift_core::{
    codec, engine,
    model::{Conflict, Destination, Edits, Format, MetadataMode, Request, Settings, Status},
};
use serde::Serialize;
use std::{
    io::{self, IsTerminal, Write},
    path::{Path, PathBuf},
    process::ExitCode,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

#[derive(Parser)]
#[command(
    name = "pixhift",
    version,
    about = "Convert images and edit metadata locally in batches.",
    after_help = "Run pixhift without arguments to open the desktop app."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Convert a folder of images, optionally applying metadata changes.
    Convert {
        #[arg(long, value_enum, default_value = "jpeg")]
        format: OutputFormat,
        #[command(flatten)]
        batch: Batch,
    },
    /// Preserve, remove or edit metadata, keeping original formats by default.
    Metadata {
        #[arg(long, value_enum, default_value = "keep")]
        format: OutputFormat,
        #[command(flatten)]
        batch: Batch,
    },
    /// List readable and writable image formats.
    Formats {
        #[arg(long)]
        json: bool,
    },
    /// Read one image's dimensions and available metadata without decoding pixels.
    Info {
        file: PathBuf,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum OutputFormat {
    #[value(alias = "jpg")]
    Jpeg,
    Png,
    Webp,
    Avif,
    #[value(alias = "tif")]
    Tiff,
    Bmp,
    Gif,
    Keep,
}

impl From<OutputFormat> for Option<Format> {
    fn from(format: OutputFormat) -> Self {
        Some(match format {
            OutputFormat::Jpeg => Format::Jpeg,
            OutputFormat::Png => Format::Png,
            OutputFormat::Webp => Format::Webp,
            OutputFormat::Avif => Format::Avif,
            OutputFormat::Tiff => Format::Tiff,
            OutputFormat::Bmp => Format::Bmp,
            OutputFormat::Gif => Format::Gif,
            OutputFormat::Keep => return None,
        })
    }
}

#[derive(Clone, Copy, ValueEnum)]
enum ConflictOption {
    Rename,
    Skip,
    Replace,
}

impl From<ConflictOption> for Conflict {
    fn from(option: ConflictOption) -> Self {
        match option {
            ConflictOption::Rename => Self::Rename,
            ConflictOption::Skip => Self::Skip,
            ConflictOption::Replace => Self::Replace,
        }
    }
}

#[derive(Args)]
struct Batch {
    /// Source folder.
    source: PathBuf,
    /// Existing output folder. By default, save alongside originals.
    #[arg(long, conflicts_with = "overwrite")]
    output: Option<PathBuf>,
    /// Include subfolders. Enabled by default.
    #[arg(long, conflicts_with = "no_recursive")]
    recursive: bool,
    /// Scan only the source folder.
    #[arg(long)]
    no_recursive: bool,
    /// Save output directly in the destination folder.
    #[arg(long, requires = "output")]
    flat: bool,
    /// Conversion quality for JPEG and AVIF.
    #[arg(long, default_value_t = 90, value_parser = clap::value_parser!(u8).range(1..=100))]
    quality: u8,
    /// Replace original files for same-format metadata changes. Converted files are siblings.
    #[arg(long)]
    overwrite: bool,
    /// What to do when the output filename exists.
    #[arg(long, value_enum, default_value = "rename")]
    conflict: ConflictOption,
    /// Print a read-only preview.
    #[arg(long)]
    dry_run: bool,
    /// Number of workers. Use zero for automatic.
    #[arg(long, default_value_t = 0, value_parser = clap::value_parser!(u8).range(0..=16))]
    jobs: u8,
    /// Shared estimated memory budget in MiB.
    #[arg(long, default_value_t = 768, value_parser = clap::value_parser!(u32).range(128..=8192))]
    memory_mib: u32,
    /// Allow JPEG conversion to flatten transparency onto white.
    #[arg(long)]
    flatten_alpha: bool,
    /// Write structured JSON to stdout.
    #[arg(long)]
    json: bool,
    /// Remove standard GPS tags from EXIF and XMP.
    #[arg(long, conflicts_with = "strip")]
    remove_gps: bool,
    /// Remove embedded descriptive metadata and ICC profiles.
    #[arg(long)]
    strip: bool,
    #[command(flatten)]
    edit: EditArgs,
}

#[derive(Args)]
struct EditArgs {
    #[arg(long, default_value = "")]
    set_author: String,
    #[arg(long, default_value = "")]
    set_copyright: String,
    #[arg(long, default_value = "")]
    set_description: String,
    #[arg(long, default_value = "")]
    set_title: String,
    /// Local capture date and time in YYYY-MM-DDTHH:MM[:SS] format.
    #[arg(long, default_value = "")]
    set_captured: String,
    #[arg(
        long,
        default_value = "",
        allow_negative_numbers = true,
        requires = "set_longitude"
    )]
    set_latitude: String,
    #[arg(
        long,
        default_value = "",
        allow_negative_numbers = true,
        requires = "set_latitude"
    )]
    set_longitude: String,
    #[arg(long, default_value = "")]
    set_make: String,
    #[arg(long, default_value = "")]
    set_model: String,
    #[arg(long, default_value = "")]
    set_lens: String,
}

impl From<EditArgs> for Edits {
    fn from(args: EditArgs) -> Self {
        Self {
            captured: args.set_captured,
            author: args.set_author,
            copyright: args.set_copyright,
            title: args.set_title,
            description: args.set_description,
            latitude: args.set_latitude,
            longitude: args.set_longitude,
            make: args.set_make,
            model: args.set_model,
            lens: args.set_lens,
        }
    }
}

fn print_json(value: &impl Serialize) -> Result<()> {
    let mut out = io::stdout().lock();
    serde_json::to_writer_pretty(&mut out, value)?;
    writeln!(out)?;
    Ok(())
}

fn display(path: &Path) -> String {
    let path = path.to_string_lossy();
    if let Some(unc) = path.strip_prefix(r"\\?\UNC\") {
        return format!(r"\\{unc}");
    }
    path.strip_prefix(r"\\?\").unwrap_or(&path).into()
}

fn batch(args: Batch, format: OutputFormat) -> Result<u8> {
    let edit: Edits = args.edit.into();
    ensure!(
        edit.is_empty() || !(args.remove_gps || args.strip),
        "Use one metadata operation. Combine it with conversion using --format."
    );
    let metadata = if args.remove_gps {
        MetadataMode::RemoveGps
    } else if args.strip {
        MetadataMode::RemoveAll
    } else if edit.is_empty() {
        MetadataMode::Preserve
    } else {
        MetadataMode::Edit
    };
    let destination = if args.overwrite {
        Destination::Overwrite
    } else if args.output.is_some() {
        Destination::Folder
    } else {
        Destination::Alongside
    };
    let request = Request {
        source: args.source,
        output: args.output,
        format: format.into(),
        quality: args.quality,
        metadata,
        edits: edit,
        destination,
        conflict: args.conflict.into(),
        confirmed_overwrite: args.overwrite,
    };
    let mut settings = Settings {
        recursive: args.recursive || !args.no_recursive,
        structure: !args.flat,
        ..Settings::default()
    };
    settings.advanced.workers = args.jobs;
    settings.advanced.memory_mib = args.memory_mib;
    settings.advanced.flatten_alpha = args.flatten_alpha;
    let plan = engine::plan(request, settings)?;
    if args.dry_run {
        if args.json {
            print_json(&plan.preview)?;
        } else {
            println!(
                "{} supported, {} scheduled, {} non-image, {} unsupported, {} unreadable, {} conflicts",
                plan.preview.supported,
                plan.preview.scheduled,
                plan.preview.non_images,
                plan.preview.unsupported,
                plan.preview.unreadable,
                plan.preview.conflicts
            );
            println!("Destination  {}", display(&plan.preview.destination));
            println!("Inputs       {:?}", plan.preview.inputs);
            println!("Outputs      {:?}", plan.preview.outputs);
            println!("Metadata     {metadata:?}");
            for warning in &plan.preview.warnings {
                eprintln!("{warning}");
            }
            for item in &plan.preview.samples {
                eprintln!("{}: {}", display(&item.source), item.message);
            }
        }
        return Ok(0);
    }

    let non_images = plan.preview.non_images;
    let cancel = Arc::new(AtomicBool::new(false));
    let signal = cancel.clone();
    ctrlc::set_handler(move || signal.store(true, Ordering::Release))
        .context("Cannot install the cancellation handler")?;
    let interactive = io::stderr().is_terminal();
    let report = engine::run(plan, &cancel, |progress| {
        if interactive {
            eprint!(
                "\r\x1b[2K{}/{}  {}",
                progress.completed,
                progress.total,
                progress
                    .file
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
            );
            let _ = io::stderr().flush();
        }
    })?;
    if interactive {
        eprintln!();
    }
    if args.json {
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Output<'a> {
            #[serde(flatten)]
            report: &'a pixhift_core::model::Report,
            non_images: usize,
        }
        print_json(&Output {
            report: &report,
            non_images,
        })?;
    } else {
        println!(
            "{} successful, {} failed, {} skipped ({} non-image) in {:.2}s",
            report.success,
            report.failed,
            report.skipped,
            non_images,
            report.duration_ms as f64 / 1000.0
        );
        for record in &report.entries {
            if !record.message.is_empty()
                && (record.status != Status::Skipped || record.message != "Non-image file.")
            {
                eprintln!(
                    "{}  {}: {}",
                    record.status.label(),
                    display(&record.source),
                    record.message
                );
            }
        }
    }
    Ok(if report.cancelled {
        130
    } else {
        u8::from(report.failed > 0)
    })
}

fn run(command: Command) -> Result<u8> {
    match command {
        Command::Convert {
            format,
            batch: args,
        }
        | Command::Metadata {
            format,
            batch: args,
        } => batch(args, format),
        Command::Formats { json } => {
            #[derive(Serialize)]
            struct Support {
                format: Format,
                import: bool,
                export: bool,
            }
            let formats: Vec<_> = Format::ALL
                .into_iter()
                .map(|format| Support {
                    format,
                    import: true,
                    export: format.writable(),
                })
                .collect();
            if json {
                print_json(&formats)?;
            } else {
                println!("{:<12} {:<8} Export", "Format", "Import");
                for support in formats {
                    println!(
                        "{:<12} {:<8} {}",
                        support.format.label(),
                        "Yes",
                        if support.export { "Yes" } else { "—" }
                    );
                }
            }
            Ok(0)
        }
        Command::Info { file, json } => {
            let info = codec::info(&file)?;
            if json {
                print_json(&info)?;
            } else {
                println!(
                    "{}  {} × {}  {} bytes",
                    info.format.label(),
                    info.width,
                    info.height,
                    info.bytes
                );
                println!(
                    "EXIF {}  XMP {}  IPTC {}  ICC {}",
                    info.exif, info.xmp, info.iptc, info.icc
                );
                for (tag, value) in info.fields {
                    println!("{tag}  {value}");
                }
                for note in info.notes {
                    eprintln!("{note}");
                }
            }
            Ok(0)
        }
    }
}

pub(crate) fn run_from_args() -> ExitCode {
    match run(Cli::parse().command) {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            eprintln!("pixhift: {error:#}");
            ExitCode::from(2)
        }
    }
}
