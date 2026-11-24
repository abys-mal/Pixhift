use crate::{codec, engine, model::*};
use image::{DynamicImage, ImageEncoder, ImageFormat, Rgb, RgbImage, Rgba, RgbaImage};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};
use tempfile::{TempDir, tempdir};

struct Folders {
    _root: TempDir,
    source: PathBuf,
    output: PathBuf,
}

impl Folders {
    fn new() -> Self {
        let root = tempdir().unwrap();
        let source = root.path().join("source");
        let output = root.path().join("output");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&output).unwrap();
        Self {
            _root: root,
            source,
            output,
        }
    }

    fn request(&self, format: Option<Format>, metadata: MetadataMode) -> Request {
        Request {
            source: self.source.clone(),
            output: Some(self.output.clone()),
            format,
            quality: 88,
            metadata,
            edits: Edits::default(),
            destination: Destination::Folder,
            conflict: Conflict::Rename,
            confirmed_overwrite: false,
        }
    }
}

fn rgb() -> DynamicImage {
    RgbImage::from_fn(24, 16, |x, y| Rgb([(x * 7) as u8, (y * 11) as u8, 114])).into()
}

fn save(path: &Path, format: ImageFormat) {
    rgb().save_with_format(path, format).unwrap();
}

fn run(request: Request) -> Report {
    let plan = engine::plan(request, Settings::default()).unwrap();
    engine::run(plan, &AtomicBool::new(false), |_| {}).unwrap()
}

fn assert_saved(report: &Report, count: usize) {
    let errors: Vec<_> = report
        .entries
        .iter()
        .filter(|entry| entry.status == Status::Failed)
        .map(|entry| (&entry.source, &entry.message))
        .collect();
    assert_eq!(report.success, count, "{errors:?}");
    assert_eq!(report.failed, 0, "{errors:?}");
}

#[test]
fn common_encoders_and_avif_roundtrip() {
    for format in [
        Format::Jpeg,
        Format::Webp,
        Format::Avif,
        Format::Tiff,
        Format::Bmp,
        Format::Gif,
    ] {
        let dirs = Folders::new();
        save(&dirs.source.join("photo.png"), ImageFormat::Png);
        let report = run(dirs.request(Some(format), MetadataMode::Preserve));
        assert_saved(&report, 1);
        let written = report.entries[0].output.as_ref().unwrap();
        assert_eq!(
            codec::dimensions(written, format, 32 * 1024 * 1024).unwrap(),
            (24, 16)
        );
        if format == Format::Avif {
            fs::remove_file(dirs.source.join("photo.png")).unwrap();
            fs::copy(written, dirs.source.join("photo.avif")).unwrap();
            let decoded = run(dirs.request(Some(Format::Png), MetadataMode::Preserve));
            assert_saved(&decoded, 1);
            assert_eq!(
                image::open(decoded.entries[0].output.as_ref().unwrap())
                    .unwrap()
                    .width(),
                24
            );
        }
    }
}

#[test]
fn heic_grid_and_camera_metadata_decode_to_jpeg_and_png() {
    let dirs = Folders::new();
    fs::write(
        dirs.source.join("checker.heic"),
        include_bytes!("../fixtures/checker-1024.heic"),
    )
    .unwrap();
    fs::write(
        dirs.source.join("camera.heif"),
        include_bytes!("../fixtures/with-exif.heic"),
    )
    .unwrap();
    for output in [Format::Png, Format::Jpeg] {
        let report = run(dirs.request(Some(output), MetadataMode::Preserve));
        assert_saved(&report, 2);
        let camera = codec::info(report.entries[0].output.as_ref().unwrap()).unwrap();
        assert_eq!((camera.width, camera.height), (2048, 1536));
        assert!(camera.exif);
        assert!(camera.xmp);
        assert!(camera.fields.contains_key("author"));
        let checker = image::open(report.entries[1].output.as_ref().unwrap())
            .unwrap()
            .to_rgb8();
        assert_eq!(checker.dimensions(), (1024, 1024));
        assert!(checker.get_pixel(16, 16).0[0] > 230);
        assert!(checker.get_pixel(80, 16).0[0] < 25);
    }
}

fn seed_metadata(dirs: &Folders, format: ImageFormat, path: &str) -> PathBuf {
    save(&dirs.source.join(path), format);
    let mut request = dirs.request(None, MetadataMode::Edit);
    request.edits = Edits {
        captured: "2025-05-12T13:42:19".into(),
        author: "First Author".into(),
        title: "Original Title".into(),
        description: "Keep this caption".into(),
        copyright: "Copyright holder".into(),
        latitude: "51.5".into(),
        longitude: "-0.1276".into(),
        make: "Camera Make".into(),
        ..Edits::default()
    };
    let report = run(request);
    assert_saved(&report, 1);
    report.entries[0].output.clone().unwrap()
}

#[test]
fn metadata_preservation_edit_gps_and_strip_without_reencoding() {
    for (extension, format) in [
        ("jpeg", ImageFormat::Jpeg),
        ("png", ImageFormat::Png),
        ("webp", ImageFormat::WebP),
    ] {
        let dirs = Folders::new();
        let name = format!("photo.{extension}");
        let edited = seed_metadata(&dirs, format, &name);
        let before = codec::info(&edited).unwrap();
        assert!(before.exif && before.xmp);
        assert_eq!(before.iptc, extension != "webp");
        assert_eq!(
            before.fields.get("gps").map(String::as_str),
            Some("present")
        );
        assert_eq!(
            before.fields.get("author").map(String::as_str),
            Some("First Author")
        );
        fs::copy(&edited, dirs.source.join(&name)).unwrap();

        let preserved = run(dirs.request(None, MetadataMode::Preserve));
        assert_saved(&preserved, 1);
        assert_eq!(
            fs::read(preserved.entries[0].output.as_ref().unwrap()).unwrap(),
            fs::read(&edited).unwrap()
        );

        let mut request = dirs.request(None, MetadataMode::Edit);
        request.edits.author = "New author".into();
        let changed = run(request);
        assert_saved(&changed, 1);
        let changed = codec::info(changed.entries[0].output.as_ref().unwrap()).unwrap();
        assert_eq!(
            changed.fields.get("author").map(String::as_str),
            Some("New author")
        );
        assert_eq!(
            changed.fields.get("description").map(String::as_str),
            Some("Keep this caption")
        );

        let gps = run(dirs.request(None, MetadataMode::RemoveGps));
        assert_saved(&gps, 1);
        let gps_path = gps.entries[0].output.as_ref().unwrap();
        let info = codec::info(gps_path).unwrap();
        assert!(!info.fields.contains_key("gps"));
        assert!(info.xmp && info.exif);
        let data = fs::read(gps_path).unwrap();
        assert!(!data.windows(11).any(|part| part == b"GPSLatitude"));
        assert!(data.windows(9).any(|part| part == b"rdf:about"));
        assert!(data.windows(8).any(|part| part == b"xml:lang"));

        let removed = run(dirs.request(None, MetadataMode::RemoveAll));
        assert_saved(&removed, 1);
        let removed = removed.entries[0].output.as_ref().unwrap();
        let info = codec::info(removed).unwrap();
        assert!(!info.exif && !info.xmp && !info.iptc && !info.icc);
        assert_eq!(
            image::open(gps_path).unwrap().to_rgba8(),
            image::open(&edited).unwrap().to_rgba8()
        );
        assert_eq!(
            image::open(removed).unwrap().to_rgba8(),
            image::open(&edited).unwrap().to_rgba8()
        );
    }
}

#[test]
fn recursion_skips_non_images_and_classifies_unsupported_and_bad_files() {
    let dirs = Folders::new();
    fs::create_dir(dirs.source.join("nested")).unwrap();
    save(&dirs.source.join("nested/photo.png"), ImageFormat::Png);
    save(&dirs.source.join("top.png"), ImageFormat::Png);
    fs::write(dirs.source.join("notes.txt"), "ignore").unwrap();
    fs::write(dirs.source.join("drawing.svg"), "<svg/>").unwrap();
    fs::write(dirs.source.join("broken.jpg"), "broken").unwrap();
    let request = dirs.request(Some(Format::Jpeg), MetadataMode::Preserve);
    let shallow = engine::plan(
        request.clone(),
        Settings {
            recursive: false,
            ..Settings::default()
        },
    )
    .unwrap();
    assert_eq!(shallow.preview.scheduled, 1);
    let plan = engine::plan(request, Settings::default()).unwrap();
    assert_eq!(
        (
            plan.preview.scheduled,
            plan.preview.non_images,
            plan.preview.unsupported,
            plan.preview.unreadable
        ),
        (2, 1, 1, 1)
    );
    assert!(!dirs.output.join("nested").exists());
    let report = engine::run(plan, &AtomicBool::new(false), |_| {}).unwrap();
    assert_eq!((report.success, report.failed, report.skipped), (2, 1, 2));
    assert!(dirs.output.join("nested/photo.jpg").exists());
    assert!(!dirs.output.join("notes.txt").exists());
}

#[test]
fn conflicts_rename_skip_and_replace_are_deterministic() {
    let dirs = Folders::new();
    save(&dirs.source.join("photo.png"), ImageFormat::Png);
    let target = dirs.output.join("photo.jpg");
    fs::write(&target, b"previous").unwrap();
    let mut request = dirs.request(Some(Format::Jpeg), MetadataMode::Preserve);
    let renamed = run(request.clone());
    assert_saved(&renamed, 1);
    assert!(dirs.output.join("photo (2).jpg").exists());
    assert_eq!(fs::read(&target).unwrap(), b"previous");
    request.conflict = Conflict::Skip;
    let skipped = run(request.clone());
    assert_eq!((skipped.success, skipped.skipped), (0, 1));
    request.conflict = Conflict::Replace;
    assert_saved(&run(request), 1);
    assert_eq!(image::open(target).unwrap().width(), 24);
}

#[test]
fn overwrite_requires_confirmation_and_conversions_keep_originals() {
    let dirs = Folders::new();
    let original = dirs.source.join("photo.heic");
    fs::write(&original, include_bytes!("../fixtures/checker-1024.heic")).unwrap();
    let existing = dirs.source.join("photo.jpg");
    save(&existing, ImageFormat::Jpeg);
    let before = fs::read(&original).unwrap();
    let other = fs::read(&existing).unwrap();
    let mut request = dirs.request(Some(Format::Jpeg), MetadataMode::Preserve);
    request.destination = Destination::Overwrite;
    let plan = engine::plan(request.clone(), Settings::default()).unwrap();
    assert!(engine::run(plan, &AtomicBool::new(false), |_| {}).is_err());
    request.confirmed_overwrite = true;
    let report = run(request.clone());
    assert_saved(&report, 1);
    assert!(dirs.source.join("photo (2).jpg").is_file());
    assert_eq!(fs::read(original).unwrap(), before);
    assert_eq!(fs::read(&existing).unwrap(), other);
    request.metadata = MetadataMode::Edit;
    request.edits.author = "Updated".into();
    request.format = None;
    let _ = run(request);
    assert_eq!(codec::info(&existing).unwrap().fields["author"], "Updated");
}

#[test]
fn cancellation_and_changed_output_leave_original_bytes_and_no_temp_files() {
    let dirs = Folders::new();
    for name in ["a.png", "b.png", "c.png"] {
        save(&dirs.source.join(name), ImageFormat::Png);
    }
    let request = dirs.request(Some(Format::Jpeg), MetadataMode::Preserve);
    let mut settings = Settings::default();
    settings.advanced.workers = 1;
    let plan = engine::plan(request.clone(), settings).unwrap();
    let cancel = AtomicBool::new(false);
    let report = engine::run(plan, &cancel, |_| cancel.store(true, Ordering::Release)).unwrap();
    assert_eq!(
        (report.success, report.skipped, report.cancelled),
        (1, 2, true)
    );
    assert_eq!(fs::read_dir(&dirs.output).unwrap().count(), 1);

    let plan = engine::plan(request, Settings::default()).unwrap();
    fs::write(dirs.output.join("b.jpg"), b"arrived after preview").unwrap();
    let report = engine::run(plan, &AtomicBool::new(false), |_| {}).unwrap();
    assert_eq!(report.failed, 1);
    assert_eq!(
        fs::read(dirs.output.join("b.jpg")).unwrap(),
        b"arrived after preview"
    );
    assert!(fs::read_dir(&dirs.output).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".pixhift-")
    }));
}

#[test]
fn refuses_nested_destination_and_isolates_transparency_error() {
    let dirs = Folders::new();
    fs::create_dir(dirs.source.join("nested")).unwrap();
    save(&dirs.source.join("solid.png"), ImageFormat::Png);
    let transparent = RgbaImage::from_pixel(10, 10, Rgba([0, 100, 0, 100]));
    transparent
        .save(dirs.source.join("transparent.png"))
        .unwrap();
    let mut request = dirs.request(Some(Format::Jpeg), MetadataMode::Preserve);
    request.output = Some(dirs.source.join("nested"));
    assert!(engine::plan(request.clone(), Settings::default()).is_err());
    request.output = Some(dirs.output.clone());
    let report = run(request);
    assert_eq!((report.success, report.failed), (1, 1));
    assert!(
        report
            .entries
            .iter()
            .any(|entry| entry.message.contains("cannot store transparency"))
    );
}

#[test]
fn orientation_is_baked_and_icc_transferred_when_supported() {
    let dirs = Folders::new();
    let path = dirs.source.join("portrait.jpg");
    let pixels = rgb().to_rgb8();
    let mut encoder = image::codecs::jpeg::JpegEncoder::new(fs::File::create(&path).unwrap());
    let profile = moxcms::ColorProfile::new_display_p3().encode().unwrap();
    encoder.set_icc_profile(profile).unwrap();
    let mut exif = little_exif::metadata::Metadata::new();
    exif.set_tag(little_exif::exif_tag::ExifTag::Orientation(vec![6]));
    encoder.set_exif_metadata(exif.encode().unwrap()).unwrap();
    encoder
        .write_image(pixels.as_raw(), 24, 16, image::ExtendedColorType::Rgb8)
        .unwrap();
    let report = run(dirs.request(Some(Format::Png), MetadataMode::Preserve));
    assert_saved(&report, 1);
    let info = codec::info(report.entries[0].output.as_ref().unwrap()).unwrap();
    assert_eq!((info.width, info.height), (16, 24));
    assert!(info.icc);
    assert!(!info.fields.contains_key("orientation"));
}

#[test]
fn strips_jpeg_comments_and_trailing_data() {
    let dirs = Folders::new();
    let source = dirs.source.join("photo.jpg");
    save(&source, ImageFormat::Jpeg);
    let mut bytes = fs::read(&source).unwrap();
    bytes.splice(2..2, b"\xff\xfe\0\x08hidden".iter().copied());
    bytes.extend(b"trailing metadata");
    fs::write(&source, bytes).unwrap();
    let report = run(dirs.request(None, MetadataMode::RemoveAll));
    assert_saved(&report, 1);
    let bytes = fs::read(report.entries[0].output.as_ref().unwrap()).unwrap();
    assert!(bytes.ends_with(&[0xff, 0xd9]));
    assert!(!bytes.windows(6).any(|part| part == b"hidden"));
}

#[test]
fn jpeg_xl_container_imports_to_png() {
    let dirs = Folders::new();
    fs::write(
        dirs.source.join("square.jxl"),
        include_bytes!("../fixtures/square.jxl"),
    )
    .unwrap();
    let report = run(dirs.request(Some(Format::Png), MetadataMode::Preserve));
    assert_saved(&report, 1);
    let pixels = image::open(report.entries[0].output.as_ref().unwrap()).unwrap();
    assert!(pixels.width() > 0);
    assert_eq!(pixels.width(), pixels.height());
}

#[test]
fn avif_conversion_embeds_readable_exif_edits() {
    let dirs = Folders::new();
    save(&dirs.source.join("photo.png"), ImageFormat::Png);
    let mut request = dirs.request(Some(Format::Avif), MetadataMode::Edit);
    request.edits.author = "AVIF author".into();
    let report = run(request);
    assert_saved(&report, 1);
    let info = codec::info(report.entries[0].output.as_ref().unwrap()).unwrap();
    assert_eq!(
        info.fields.get("author").map(String::as_str),
        Some("AVIF author")
    );
}
