# Pixhift

Pixhift is an offline desktop and command-line utility for converting folders of photos and editing embedded metadata. The TypeScript desktop interface uses Tauri 2 and the system webview. Both modes use the same executable and call the same Rust core.

The current release is **v1.0.0**. See the [changelog](CHANGELOG.md).

## Run locally on Windows

From the repository root, use PowerShell with Rust 1.98 or newer on the MSVC toolchain, Node.js 20.12 or newer, Visual Studio C++ build tools with a Windows SDK, and Edge WebView2 Runtime.

~~~powershell
npm.cmd ci
npm.cmd run dev
~~~

The desktop command compiles the frontend and opens a debug build.

Double-click pixhift.exe or run it without arguments to open the desktop. Pass a command or option to use the CLI. In an interactive PowerShell session, pipe output to Out-Host or capture it in a variable when you need the shell to wait for the command to finish.

~~~powershell
npm.cmd run build
cargo build -p pixhift --features custom-protocol --locked
.\target\debug\pixhift.exe
.\target\debug\pixhift.exe --help | Out-Host
~~~

To build the standalone Windows release executable, run the following after compiling the frontend.

~~~powershell
cargo build --release -p pixhift --features custom-protocol --locked
.\target\release\pixhift.exe --version | Out-Host
~~~

Dependency downloads require network access on the first setup. The development executable processes files offline. After the dependencies are cached, Rust builds and tests can use Cargo's --offline flag.

## CLI

~~~powershell
.\target\debug\pixhift.exe convert "D:\Phone photos" --format jpeg --output "D:\Converted" --dry-run --json
.\target\debug\pixhift.exe convert "D:\Phone photos" --format jpeg --output "D:\Converted" --set-author "Studio name"
.\target\debug\pixhift.exe metadata "D:\Converted" --remove-gps --overwrite
.\target\debug\pixhift.exe info "D:\Converted\photo.jpg" --json
~~~

- Supply one source folder. Recursion and folder structure preservation are enabled by default. Use --no-recursive for a top-level scan and --flat with an output folder for a flat result.
- An explicit --output path must be an existing folder outside the source tree. With no output path, the CLI creates sibling files and automatically renames collisions.
- --overwrite can replace originals for same-format metadata changes. Converting to a different format always creates a sibling file and retains the source. --conflict replace explicitly permits replacing an eligible existing destination file.
- Conversion and one metadata operation can run together. Use --help on either batch command for capture date, GPS coordinates, camera, quality, memory, worker and destination options.
- --dry-run reads headers and produces the same counts and file decisions as the desktop preview. --json produces one JSON document on stdout. Interactive progress and diagnostics use stderr.
- Exit codes are 0 for success, including skipped files and a completed dry-run, 1 for individual file failures, 2 for invalid arguments or a batch-level error, and 130 for interrupted processing. Ctrl+C stops queueing work and abandons unfinished outputs before commit.

## Formats

| Format | Import | Export | Scope |
| --- | --- | --- | --- |
| JPEG / JPG / JFIF | Yes | Yes | Adjustable quality for conversions, JPEG requires opaque pixels or optional white flattening |
| PNG / APNG | Yes | Yes | Still export supports alpha and 16-bit samples. Same-format copy and compatible metadata rewrites retain APNG frames |
| WebP | Yes | Yes | Lossless still export. Same-format copy and compatible metadata rewrites retain animation |
| HEIC / HEIF | Yes | Keep original | HEVC primary still images, including tiled grids and supported 8/10-bit profiles. Alpha and transforms are handled when supported by the decoder |
| AVIF | Yes | Yes | Still images. Adjustable quality, EXIF export, and 8-bit export samples |
| TIFF | Yes | Yes | Common still TIFF, including 16-bit output. Multi-page conversions are blocked |
| BMP | Yes | Yes | Common still images |
| GIF | Yes | Yes | Paletted still export, with one-bit transparency. Same-format copy and metadata stripping retain animation |
| JPEG XL | Yes | Keep original | Still conversion via jxl-oxide |

Keep original format copies unchanged image data for Preserve metadata. When the source already matches a selected format, Pixhift uses the same lossless copy or metadata-edit path. The quality slider affects conversions that actually encode JPEG or AVIF.

Frame-losing conversions are blocked for supported animated containers and multipage TIFF. HEIC conversion exports the primary still and may omit auxiliary depth, gain maps, and other independent items. Explicitly identified PQ/HLG HDR inputs and some unusual HEIF/AVIF layouts fail with an explanation. Windows playback of the output is independent of whether the OS supplies its own HEIC extension.

## Metadata

- Same-format JPEG, PNG and WebP support EXIF and XMP editing, removing standard EXIF/XMP GPS fields, and clearing embedded metadata without recompressing pixels where possible. Orientation may require pixel normalization when stripping. Extended JPEG XMP and multi-picture JPEGs are handled conservatively.
- JPEG and PNG also support common IPTC IIM fields. XMP carries the Unicode form of edited text. EXIF string fields are ASCII, and IPTC mirrors use bounded field sizes and the original encoding where possible. Blank edits leave existing fields unchanged.
- Conversion transfers compatible EXIF, XMP, IPTC and ICC data. Some format-specific chunks, thumbnails and maker-note details cannot be carried across formats. JPEG, PNG and WebP support ICC embedding. TIFF supports ICC; AVIF supports EXIF. Output warnings identify metadata or color information that cannot transfer.
- Keep-format stripping is also available for GIF and for compatible single-image TIFF, BMP and AVIF. TIFF, BMP and AVIF stripping can re-encode the image. HEIC and JPEG XL metadata changes require conversion to a writable metadata format.
- GPS-only removal targets structured EXIF and XMP GPS fields. Free-text location references, vendor-specific blocks, embedded imagery and external files are outside that operation. Removing ICC profiles can change appearance. Structural color, transparency, and animation declarations may be retained to keep the image viewable.
- Pixhift applies display orientation on conversions, preserves transparency in capable outputs, and reports lossy limits such as reduced bit depth and GIF palette quantization.

## Development and verification

~~~powershell
npm.cmd run lint
npm.cmd run build
cargo fmt --all --check
cargo clippy --workspace --all-targets --features pixhift/custom-protocol --locked -- -D warnings
cargo test --workspace --features pixhift/custom-protocol --locked
cargo build -p pixhift --features custom-protocol --locked
~~~

The GUI requires the platform's Tauri 2 prerequisites. macOS uses WebKit and Linux uses WebKitGTK and the common Tauri development libraries. The codec code uses the same Rust implementations on all targets. The development verification for this project is on Windows. See [Tauri prerequisites](https://v2.tauri.app/start/prerequisites/) and [third-party notices](THIRD-PARTY.md).

Settings are stored in the operating system's application configuration directory. Results are kept for the running desktop session and can be explicitly exported as CSV or TXT. Diagnostic logging, when enabled, goes to the development terminal. Temporary outputs are created in the destination filesystem, validated, and moved into place. Closing the window during a batch requests cancellation. Abrupt termination can leave a .pixhift-*.tmp working file.

Pixhift’s original code and assets are released under [The Unlicense](LICENSE). Third-party components retain their own licenses, and codec patent obligations apply separately. See [THIRD-PARTY.md](THIRD-PARTY.md). No installers, updates, background jobs, telemetry, or history database are configured.
