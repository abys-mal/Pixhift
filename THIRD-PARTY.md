# Third-party dependencies and codec choices

Pixhift uses in-process Rust codecs. There is no dependency on separately installed image-processing utilities or on the Windows HEIC extension.

| Components | License identifiers | Purpose and tradeoffs |
| --- | --- | --- |
| Tauri, Tauri API and Tauri plugins | MIT OR Apache-2.0 | System webview and native dialogs. The webview remains outside the executable. Pixhift initializes the webview and dialogs in desktop mode |
| image, image-webp, tiff, heic-rs and jxl-oxide | MIT OR Apache-2.0, with tiff under MIT | Narrow image feature selection, portable HEIC still decode and JPEG XL import. JPEG XL adds decoder code and supplies import only |
| avif-decode, avif-parse, rav1d, ravif and rav1e | BSD-3-Clause, MPL-2.0, BSD-2-Clause, BSD-3-Clause and BSD-2-Clause, respectively | Separate AV1 decode and encode stacks. Pixhift vendors avif-decode 3.0.0, disables its default assembly dependency, and uses one decode thread per worker. This avoids a NASM or native codec dependency on Windows at the cost of AV1 decode speed |
| img-parts, little_exif, moxcms, quick-xml | MIT OR Apache-2.0 for img-parts and little_exif, BSD-3-Clause OR Apache-2.0 for moxcms, MIT for quick-xml | Container changes, EXIF tags, color profiles and XML-based XMP editing without ExifTool |
| Clap and ctrlc | MIT OR Apache-2.0 for Clap. See ctrlc's upstream notice | Native CLI parsing and graceful keyboard interruption with limited Clap features |
| TypeScript | Apache-2.0 | Frontend development tool |

HEIC was evaluated against libheif with an HEVC decoder. libheif has a broader, mature codec ecosystem, with native build and licensing requirements that vary with codec choices. Its LGPL obligations and separate codec/linking setup make it more complex for this local cross-platform build. The selected heic-rs provides tiled still-image decoding and metadata access under permissive terms, with a smaller build surface. Its supported profiles and localized image coverage are still narrower than a full libheif installation. HEIC export is disabled. HEVC patent licensing is separate from the implementation's source license and requires review before distribution.

For AVIF, image's optional native decoder uses libdav1d. The selected rav1d-backed path avoids managing a platform-specific native decoder library. The original BSD license and attribution are included under src-tauri/vendor/avif-decode. The local changes are limited to the manifest and decoder worker/size limits.

Before redistribution, use Cargo.lock and package-lock.json to inventory the exact resolved dependency versions. Preserve applicable license texts, copyright notices, attribution and NOTICE files, including those of transitive dependencies. The MPL-2.0 on avif-parse has file-level source availability and notice requirements when distributing covered executable code. Review all applicable conditions for the intended manner and jurisdictions of distribution. The source license choice does not cover codec patent rights.

Sources include the [Tauri project](https://github.com/tauri-apps/tauri), [image](https://github.com/image-rs/image), [heic-rs](https://github.com/tbraun96/heic-rs), [libheif](https://github.com/strukturag/libheif), [avif-decode](https://github.com/kornelski/avif-decode), [rav1d](https://github.com/memorysafety/rav1d), [jxl-oxide](https://github.com/tirr-c/jxl-oxide), and [little_exif](https://github.com/TechnikTobi/little_exif).

## Fixtures

The checker and EXIF-bearing HEIC grids in crates/pixhift-core/fixtures are generated with macOS image encoding by the heic-rs project and tested here as actual HEIC files. Their synthetic imagery is MIT OR Apache-2.0. The redistributable license texts are included with the files. The JPEG XL square is from the JPEG XL Project test data. Its CC BY 4.0 license is included alongside it. See the [HEIC fixture descriptions](https://github.com/tbraun96/heic-rs/tree/main/tests/fixtures) and [the JPEG XL container fixture](https://github.com/libjxl/testdata/blob/73695d3/jxl/boxes/square-extended-size-container.jxl).
