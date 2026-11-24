use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::PathBuf};

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    Jpeg,
    Png,
    Webp,
    Heic,
    Avif,
    Tiff,
    Bmp,
    Gif,
    Jxl,
}

impl Format {
    pub fn extension(self) -> &'static str {
        match self {
            Self::Jpeg => "jpg",
            Self::Png => "png",
            Self::Webp => "webp",
            Self::Heic => "heic",
            Self::Avif => "avif",
            Self::Tiff => "tif",
            Self::Bmp => "bmp",
            Self::Gif => "gif",
            Self::Jxl => "jxl",
        }
    }

    pub fn writable(self) -> bool {
        !matches!(self, Self::Heic | Self::Jxl)
    }

    pub fn image(self) -> Option<image::ImageFormat> {
        Some(match self {
            Self::Jpeg => image::ImageFormat::Jpeg,
            Self::Png => image::ImageFormat::Png,
            Self::Webp => image::ImageFormat::WebP,
            Self::Tiff => image::ImageFormat::Tiff,
            Self::Bmp => image::ImageFormat::Bmp,
            Self::Gif => image::ImageFormat::Gif,
            Self::Avif | Self::Heic | Self::Jxl => return None,
        })
    }
}

#[derive(Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    #[default]
    System,
    Light,
    Dark,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Advanced {
    pub workers: u8,
    pub memory_mib: u32,
    pub ignore_hidden: bool,
    pub flatten_alpha: bool,
    pub diagnostics: bool,
}

impl Default for Advanced {
    fn default() -> Self {
        Self {
            workers: 0,
            memory_mib: 768,
            ignore_hidden: false,
            flatten_alpha: false,
            diagnostics: false,
        }
    }
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    pub theme: Theme,
    pub preview: bool,
    pub recursive: bool,
    pub structure: bool,
    pub remember_output: bool,
    pub last_output: Option<PathBuf>,
    pub advanced: Advanced,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: Theme::System,
            preview: true,
            recursive: true,
            structure: true,
            remember_output: true,
            last_output: None,
            advanced: Advanced::default(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum MetadataMode {
    #[default]
    Preserve,
    RemoveGps,
    RemoveAll,
    Edit,
}

#[derive(Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Edits {
    pub captured: String,
    pub author: String,
    pub copyright: String,
    pub title: String,
    pub description: String,
    pub latitude: String,
    pub longitude: String,
    pub make: String,
    pub model: String,
    pub lens: String,
}

impl Edits {
    pub fn is_empty(&self) -> bool {
        [
            &self.captured,
            &self.author,
            &self.copyright,
            &self.title,
            &self.description,
            &self.latitude,
            &self.longitude,
            &self.make,
            &self.model,
            &self.lens,
        ]
        .iter()
        .all(|value| value.trim().is_empty())
    }
}

#[derive(Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Destination {
    Folder,
    Alongside,
    Overwrite,
}

#[derive(Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Conflict {
    Rename,
    Skip,
    Replace,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Request {
    pub source: PathBuf,
    pub output: Option<PathBuf>,
    pub format: Option<Format>,
    pub quality: u8,
    pub metadata: MetadataMode,
    pub edits: Edits,
    pub destination: Destination,
    pub conflict: Conflict,
    pub confirmed_overwrite: bool,
}

#[derive(Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Preview {
    pub metadata: MetadataMode,
    pub supported: usize,
    pub non_images: usize,
    pub unsupported: usize,
    pub unreadable: usize,
    pub ignored: usize,
    pub conflicts: usize,
    pub scheduled: usize,
    pub inputs: BTreeMap<Format, usize>,
    pub outputs: BTreeMap<Format, usize>,
    pub destination: PathBuf,
    pub warnings: Vec<String>,
    pub samples: Vec<Entry>,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Success,
    Failed,
    Skipped,
}

impl Status {
    pub fn label(self) -> &'static str {
        match self {
            Self::Success => "Success",
            Self::Failed => "Failed",
            Self::Skipped => "Skipped",
        }
    }
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    pub source: PathBuf,
    pub output: Option<PathBuf>,
    pub status: Status,
    pub message: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    pub success: usize,
    pub failed: usize,
    pub skipped: usize,
    pub duration_ms: u64,
    pub cancelled: bool,
    pub destination: PathBuf,
    pub entries: Vec<Entry>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    pub completed: usize,
    pub total: usize,
    pub file: PathBuf,
}

impl Format {
    pub const ALL: [Self; 9] = [
        Self::Jpeg,
        Self::Png,
        Self::Webp,
        Self::Heic,
        Self::Avif,
        Self::Tiff,
        Self::Bmp,
        Self::Gif,
        Self::Jxl,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Jpeg => "JPEG",
            Self::Png => "PNG",
            Self::Webp => "WebP",
            Self::Heic => "HEIC/HEIF",
            Self::Avif => "AVIF",
            Self::Tiff => "TIFF",
            Self::Bmp => "BMP",
            Self::Gif => "GIF",
            Self::Jxl => "JPEG XL",
        }
    }
}
