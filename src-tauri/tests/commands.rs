use serde_json::Value;
use std::{
    fs,
    path::Path,
    process::{Command, Output},
};
use tempfile::tempdir;

fn pixhift(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_pixhift"))
        .args(args)
        .output()
        .unwrap()
}

fn json(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|_| {
        panic!(
            "{} / {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

fn path(path: &Path) -> &str {
    path.to_str().unwrap()
}

fn image(path: &Path) {
    image::RgbImage::from_pixel(16, 10, image::Rgb([110, 170, 80]))
        .save(path)
        .unwrap();
}

#[test]
fn argument_parsing_and_formats_json() {
    let version = pixhift(&["--version"]);
    assert!(version.status.success());
    assert_eq!(
        String::from_utf8_lossy(&version.stdout).trim(),
        concat!("pixhift ", env!("CARGO_PKG_VERSION"))
    );

    let help = pixhift(&["--help"]);
    assert!(help.status.success());
    assert!(String::from_utf8_lossy(&help.stdout).contains("metadata"));
    let invalid = pixhift(&["convert", "whatever", "--quality", "101"]);
    assert_eq!(invalid.status.code(), Some(2));
    assert!(invalid.stdout.is_empty());
    assert!(String::from_utf8_lossy(&invalid.stderr).contains("--quality"));
    let unknown = pixhift(&["convert", "whatever", "--format", "heic"]);
    assert_eq!(unknown.status.code(), Some(2));
    let formats = pixhift(&["formats", "--json"]);
    assert_eq!(json(&formats).as_array().unwrap().len(), 9);
    assert!(formats.stderr.is_empty());
    assert!(
        !json(&formats)
            .as_array()
            .unwrap()
            .iter()
            .find(|support| support["format"] == "heic")
            .unwrap()["export"]
            .as_bool()
            .unwrap()
    );
}

#[test]
fn dry_run_is_read_only_and_matches_the_shared_plan() {
    let root = tempdir().unwrap();
    let source = root.path().join("source");
    let output = root.path().join("output");
    fs::create_dir_all(source.join("nested")).unwrap();
    fs::create_dir(&output).unwrap();
    image(&source.join("nested/photo.png"));
    fs::write(source.join("notes.txt"), "keep").unwrap();
    let args = [
        "convert",
        path(&source),
        "--output",
        path(&output),
        "--format",
        "jpeg",
        "--set-author",
        "Pixhift test",
        "--json",
    ];
    let preview = pixhift(&[args.as_slice(), &["--dry-run"]].concat());
    assert!(preview.status.success());
    let value = json(&preview);
    assert_eq!(value["scheduled"], 1);
    assert_eq!(value["nonImages"], 1);
    assert_eq!(fs::read_dir(&output).unwrap().count(), 0);
    let request = pixhift_core::model::Request {
        source: source.clone(),
        output: Some(output.clone()),
        format: Some(pixhift_core::model::Format::Jpeg),
        quality: 90,
        metadata: pixhift_core::model::MetadataMode::Edit,
        edits: pixhift_core::model::Edits {
            author: "Pixhift test".into(),
            ..Default::default()
        },
        destination: pixhift_core::model::Destination::Folder,
        conflict: pixhift_core::model::Conflict::Rename,
        confirmed_overwrite: false,
    };
    let gui_plan = pixhift_core::engine::plan(request, Default::default()).unwrap();
    assert_eq!(value, serde_json::to_value(gui_plan.preview).unwrap());

    let finished = pixhift(&args);
    assert!(finished.status.success());
    assert!(finished.stderr.is_empty());
    assert_eq!(json(&finished)["success"], 1);
    assert_eq!(json(&finished)["nonImages"], 1);
    let photo = output.join("nested/photo.jpg");
    assert!(photo.exists());
    let info = pixhift(&["info", path(&photo), "--json"]);
    assert_eq!(json(&info)["fields"]["author"], "Pixhift test");
}

#[test]
fn non_destructive_default_and_explicit_overwrite() {
    let root = tempdir().unwrap();
    let photo = root.path().join("photo.png");
    image(&photo);
    let original = fs::read(&photo).unwrap();
    let args = [
        "metadata",
        path(root.path()),
        "--set-author",
        "Test",
        "--no-recursive",
        "--json",
    ];
    assert_eq!(pixhift(&args).status.code(), Some(0));
    assert_eq!(fs::read(&photo).unwrap(), original);
    assert!(root.path().join("photo (2).png").exists());
    let overwrite = pixhift(&[args.as_slice(), &["--overwrite"]].concat());
    assert_eq!(json(&overwrite)["success"], 2);
    assert_ne!(fs::read(&photo).unwrap(), original);
    assert_eq!(
        json(&pixhift(&["info", path(&photo), "--json"]))["fields"]["author"],
        "Test"
    );
}

#[test]
fn partial_failure_has_a_nonzero_exit_and_a_valid_json_report() {
    let root = tempdir().unwrap();
    image(&root.path().join("photo.png"));
    fs::write(root.path().join("damaged.jpg"), "bad header").unwrap();
    let report = pixhift(&["convert", path(root.path()), "--json"]);
    assert_eq!(report.status.code(), Some(1));
    assert_eq!(json(&report)["success"], 1);
    assert_eq!(json(&report)["failed"], 1);
}
