use crate::{
    metadata::{self, Changes, Tags},
    model::{Advanced, Format, MetadataMode, Request},
};
use anyhow::{Context, Result, bail, ensure};
use image::{
    DynamicImage, ImageDecoder, ImageEncoder, ImageFormat, ImageReader,
    codecs::{
        avif::AvifEncoder, jpeg::JpegEncoder, png::PngEncoder, tiff::TiffEncoder, webp::WebPEncoder,
    },
    metadata::Orientation,
};
use img_parts::Bytes;
use std::{
    fs::File,
    io::{BufReader, Cursor, Read, Write},
    path::Path,
};

const MIB: u64 = 1024 * 1024;

#[derive(Debug, PartialEq, Eq)]
pub enum Kind {
    Supported(Format),
    Unsupported,
    Invalid,
    Other,
}

pub fn kind(path: &Path, bytes: &[u8]) -> Kind {
    use Format::*;
    let found =
        if bytes.starts_with(&[0xff, 0x0a]) || bytes.starts_with(b"\0\0\0\x0cJXL \r\n\x87\n") {
            Some(Jxl)
        } else if bytes.get(4..8) == Some(b"ftyp") {
            let brands = brands(bytes);
            if brands
                .iter()
                .any(|brand| matches!(*brand, b"avif" | b"avis"))
            {
                Some(Avif)
            } else if brands.iter().any(|brand| {
                matches!(
                    *brand,
                    b"heic"
                        | b"heix"
                        | b"hevc"
                        | b"hevx"
                        | b"heim"
                        | b"heis"
                        | b"hevm"
                        | b"hevs"
                        | b"mif1"
                        | b"msf1"
                )
            }) {
                Some(Heic)
            } else {
                None
            }
        } else {
            match image::guess_format(bytes) {
                Ok(ImageFormat::Jpeg) => Some(Jpeg),
                Ok(ImageFormat::Png) => Some(Png),
                Ok(ImageFormat::WebP) => Some(Webp),
                Ok(ImageFormat::Tiff) => Some(Tiff),
                Ok(ImageFormat::Bmp) => Some(Bmp),
                Ok(ImageFormat::Gif) => Some(Gif),
                Ok(_) => return Kind::Unsupported,
                Err(_) => None,
            }
        };
    if let Some(format) = found {
        return Kind::Supported(format);
    }
    let extension = path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if matches!(
        extension.as_str(),
        "jpg"
            | "jpeg"
            | "jpe"
            | "jfif"
            | "png"
            | "apng"
            | "webp"
            | "heic"
            | "heif"
            | "hif"
            | "heics"
            | "avif"
            | "tif"
            | "tiff"
            | "bmp"
            | "gif"
            | "jxl"
    ) {
        return Kind::Invalid;
    }
    if matches!(
        extension.as_str(),
        "svg"
            | "svgz"
            | "ico"
            | "cur"
            | "ppm"
            | "pgm"
            | "pbm"
            | "pnm"
            | "qoi"
            | "tga"
            | "psd"
            | "psb"
            | "exr"
            | "jp2"
            | "j2k"
            | "j2c"
            | "raw"
            | "dng"
            | "cr2"
            | "cr3"
            | "arw"
            | "nef"
            | "nrw"
            | "orf"
            | "rw2"
            | "raf"
            | "pef"
            | "srw"
            | "x3f"
            | "dds"
            | "hdr"
            | "icns"
            | "xcf"
            | "jxr"
            | "wdp"
            | "ktx"
            | "ktx2"
            | "pcx"
            | "eps"
    ) || bytes.starts_with(b"8BPS")
        || bytes.starts_with(b"\0\0\0\x0cjP  \r\n\x87\n")
    {
        return Kind::Unsupported;
    }
    Kind::Other
}

fn brands(bytes: &[u8]) -> Vec<&[u8; 4]> {
    if bytes.get(4..8) != Some(b"ftyp") {
        return Vec::new();
    }
    let length = u32::from_be_bytes(bytes[..4].try_into().unwrap()) as usize;
    let end = length.min(bytes.len());
    if end < 16 {
        return Vec::new();
    }
    std::iter::once(bytes[8..12].try_into().unwrap())
        .chain(bytes[16..end].as_chunks::<4>().0.iter())
        .collect()
}

pub fn sniff(path: &Path) -> Result<Kind> {
    let mut buffer = Vec::with_capacity(128);
    File::open(path)?.take(128).read_to_end(&mut buffer)?;
    Ok(kind(path, &buffer))
}

fn reader_limits(memory: u64) -> image::Limits {
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(memory);
    limits
}

pub fn read_bounded(path: &Path, max: u64) -> Result<Bytes> {
    let mut buffer = Vec::new();
    File::open(path)?
        .take(max.saturating_add(1))
        .read_to_end(&mut buffer)?;
    ensure!(
        buffer.len() as u64 <= max,
        "The input file exceeds the memory profile."
    );
    Ok(buffer.into())
}

pub fn dimensions(path: &Path, format: Format, max_file: u64) -> Result<(u32, u32)> {
    let mut reader = BufReader::new(File::open(path)?);
    let dimensions = match format {
        Format::Heic => {
            let bytes = read_bounded(path, max_file)?;
            let info = heic_rs::probe(&bytes)?;
            (info.width, info.height)
        }
        Format::Avif => {
            let data = avif_parse::read_avif(&mut reader.by_ref().take(max_file))?;
            let info = data.primary_item_metadata()?;
            (info.max_frame_width.get(), info.max_frame_height.get())
        }
        Format::Jxl => jxl_oxide::integration::JxlDecoder::new(reader)?.dimensions(),
        format => {
            let mut input = ImageReader::with_format(
                reader,
                format.image().context("Unsupported image format")?,
            );
            input.limits(reader_limits(max_file));
            input.into_dimensions()?
        }
    };
    ensure!(
        dimensions.0 > 0 && dimensions.1 > 0,
        "Image has invalid dimensions."
    );
    Ok(dimensions)
}

pub fn estimate(
    size: u64,
    dimensions: (u32, u32),
    input: Format,
    output: Format,
    request: &Request,
) -> u64 {
    let same = input == output;
    if same
        && (request.metadata == MetadataMode::Preserve
            || (request.metadata == MetadataMode::Edit && request.edits.is_empty()))
    {
        return 4 * MIB;
    }
    let pixels = u64::from(dimensions.0) * u64::from(dimensions.1);
    let bytes_per_pixel = if output == Format::Avif {
        30
    } else if matches!(input, Format::Heic | Format::Avif | Format::Jxl) {
        20
    } else {
        14
    };
    if same && !matches!(request.metadata, MetadataMode::RemoveAll) {
        return (8 * MIB).saturating_add(size.saturating_mul(5));
    }
    (12 * MIB)
        .saturating_add(size.saturating_mul(5))
        .saturating_add(pixels.saturating_mul(bytes_per_pixel))
}

fn gif_blocks(bytes: &Bytes, strip: bool) -> Result<(usize, Option<Bytes>)> {
    let mut position = 13;
    let flags = *bytes.get(10).context("Truncated GIF header")?;
    if flags & 0x80 != 0 {
        position += 3 << (usize::from(flags & 7) + 1);
    }
    let mut frames = 0;
    let mut output = if strip {
        bytes
            .get(..position)
            .context("Truncated GIF palette")?
            .to_vec()
    } else {
        Vec::new()
    };
    loop {
        let start = position;
        let marker = *bytes.get(position).context("GIF has no trailer")?;
        position += 1;
        let keep = match marker {
            0x3b => {
                if strip {
                    output.push(marker);
                }
                ensure!(frames > 0, "GIF has no image frames.");
                return Ok((frames, strip.then(|| output.into())));
            }
            0x2c => {
                frames += 1;
                let descriptor = bytes
                    .get(position..position + 9)
                    .context("Truncated GIF descriptor")?;
                position += 9;
                let flags = descriptor[8];
                if flags & 0x80 != 0 {
                    position += 3 << (usize::from(flags & 7) + 1);
                }
                position += 1; // LZW code size
                true
            }
            0x21 => {
                let label = *bytes.get(position).context("Truncated GIF extension")?;
                position += 1;
                ensure!(
                    label != 0x01,
                    "GIF plain-text graphics cannot be safely processed."
                );
                label == 0xf9
                    || (label == 0xff
                        && matches!(
                            bytes.get(position..position + 12),
                            Some(b"\x0bNETSCAPE2.0" | b"\x0bANIMEXTS1.0")
                        ))
            }
            _ => bail!("Invalid GIF data block."),
        };
        loop {
            let length = usize::from(*bytes.get(position).context("Truncated GIF data")?);
            position += length + 1;
            if length == 0 {
                break;
            }
        }
        if strip && keep {
            output.extend_from_slice(&bytes[start..position]);
        }
    }
}

fn image_tags(
    decoder: &mut dyn ImageDecoder,
    input: Format,
    bytes: &Bytes,
    mode: MetadataMode,
) -> Result<Tags> {
    if mode == MetadataMode::RemoveAll {
        return Ok(Tags::default());
    }
    let iptc = match input {
        Format::Jpeg => metadata::read_jpeg_iptc(bytes)?,
        Format::Png => decoder
            .iptc_metadata()?
            .map(|data| metadata::decode_png_iptc(&data))
            .transpose()?,
        _ => None,
    };
    Ok(Tags {
        exif: decoder.exif_metadata()?,
        xmp: decoder.xmp_metadata()?,
        icc: decoder.icc_profile()?,
        iptc,
    })
}

fn decode_common(
    bytes: &Bytes,
    input: Format,
    limit: u64,
    mode: MetadataMode,
) -> Result<(DynamicImage, Tags, Orientation)> {
    match input {
        Format::Gif => ensure!(
            gif_blocks(bytes, false)?.0 == 1,
            "Animated GIF conversion would lose frames. Keep the original format."
        ),
        Format::Png => {
            let decoder = image::codecs::png::PngDecoder::new(Cursor::new(bytes))?;
            ensure!(
                !decoder.is_apng()?,
                "Animated PNG conversion would lose frames. Keep the original format."
            );
        }
        Format::Webp => {
            let decoder = image::codecs::webp::WebPDecoder::new(Cursor::new(bytes))?;
            ensure!(
                !decoder.has_animation(),
                "Animated WebP conversion would lose frames. Keep the original format."
            );
        }
        Format::Tiff => {
            let decoder = tiff::decoder::Decoder::new(Cursor::new(bytes))?;
            ensure!(
                !decoder.more_images(),
                "Multi-page TIFF conversion would lose pages. Keep the original file."
            );
        }
        Format::Jpeg => ensure!(
            !metadata::is_multipicture_jpeg(bytes)?,
            "Converting this multi-picture JPEG would lose its auxiliary images."
        ),
        _ => (),
    }
    let mut reader = ImageReader::with_format(
        Cursor::new(bytes),
        input.image().context("Unsupported image format")?,
    );
    reader.limits(reader_limits(limit));
    let mut decoder = reader.into_decoder()?;
    let orientation = decoder.orientation()?;
    let tags = image_tags(&mut decoder, input, bytes, mode)?;
    Ok((DynamicImage::from_decoder(decoder)?, tags, orientation))
}

fn exif_orientation(tags: &Tags) -> Orientation {
    tags.exif
        .as_deref()
        .and_then(Orientation::from_exif_chunk)
        .unwrap_or(Orientation::NoTransforms)
}

fn decode_heic(
    bytes: &Bytes,
    limit: u64,
    mode: MetadataMode,
    warnings: &mut Vec<String>,
) -> Result<(DynamicImage, Tags, Orientation)> {
    ensure!(
        !brands(bytes)
            .iter()
            .any(|brand| matches!(*brand, b"hevc" | b"hevx" | b"msf1" | b"hevm" | b"hevs")),
        "HEIF image sequences cannot be converted without losing frames."
    );
    let info = heic_rs::probe(bytes)?;
    let ctx = heic_rs::context::Context::open(bytes)?;
    let props = ctx.props(info.primary_item)?;
    let color = ctx.declared_nclx(info.primary_item)?;
    ensure!(
        !color.is_some_and(|c| matches!(c.transfer, 16 | 18)),
        "PQ/HLG HDR HEIC conversion requires tone mapping or HDR output. Keep the original format."
    );
    if props.pasp.is_some() || ctx.images().len() > 1 {
        warnings.push("HEIC contains additional image items or aspect-ratio information. Only the primary image and its transparency are exported.".into());
    }
    let mut tags = Tags::default();
    if let Some(id) = ctx.exif_item(info.primary_item) {
        let item = ctx.item_data(id)?;
        tags.exif = Some(heic_rs::metadata::exif_tiff(&item)?.to_vec());
    }
    if let Some(id) = ctx.xmp_item(info.primary_item) {
        tags.xmp = Some(ctx.item_data(id)?.into_owned());
    }
    if mode != MetadataMode::RemoveAll {
        tags.icc = ctx.declared_icc(info.primary_item)?.map(<[u8]>::to_vec);
        if tags.icc.is_none()
            && let Some(color) = color
        {
            if color.primaries == 12 && color.transfer == 13 {
                tags.icc = Some(moxcms::ColorProfile::new_display_p3().encode()?);
            } else if !(matches!(color.primaries, 1 | 2) && matches!(color.transfer, 1 | 2 | 13)) {
                warnings.push("The HEIC color declaration cannot be embedded as an ICC profile. Output appearance may differ.".into());
            }
        }
    }
    let orientation = if props.transforms.is_empty() {
        exif_orientation(&tags)
    } else {
        Orientation::NoTransforms
    };
    use heic_rs::PixelLayout::*;
    let layout = match (info.bit_depth > 8, info.has_alpha) {
        (false, false) => Rgb8,
        (false, true) => Rgba8,
        (true, false) => Rgb16,
        (true, true) => Rgba16,
    };
    let options = heic_rs::DecodeOptions::default()
        .with_threads(Some(1))
        .with_layout(layout)
        .with_max_pixels(Some((limit / 12).max(1)))
        .with_strict(true);
    let output = heic_rs::decode(bytes, &options)?;
    let (width, height) = (output.width, output.height);
    let pixels = match layout {
        Rgb8 => image::RgbImage::from_raw(width, height, output.data).map(DynamicImage::ImageRgb8),
        Rgba8 => {
            image::RgbaImage::from_raw(width, height, output.data).map(DynamicImage::ImageRgba8)
        }
        Rgb16 => image::ImageBuffer::from_raw(width, height, words(&output.data))
            .map(DynamicImage::ImageRgb16),
        Rgba16 => image::ImageBuffer::from_raw(width, height, words(&output.data))
            .map(DynamicImage::ImageRgba16),
        _ => None,
    }
    .context("Invalid HEIC pixel buffer")?;
    Ok((pixels, tags, orientation))
}

fn words(bytes: &[u8]) -> Vec<u16> {
    bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|word| u16::from_ne_bytes(*word))
        .collect()
}

fn decode_avif(
    bytes: &Bytes,
    limit: u64,
    warnings: &mut Vec<String>,
) -> Result<(DynamicImage, Tags, Orientation)> {
    ensure!(
        !brands(bytes).contains(&b"avis"),
        "Animated AVIF conversion would lose frames. Keep the original format."
    );
    let ctx = iso_context(bytes, Format::Avif);
    let mut tags = Tags::default();
    let mut orientation = Orientation::NoTransforms;
    if let Ok(ctx) = ctx {
        let id = ctx.meta.primary;
        let props = ctx.props(id)?;
        ensure!(
            props.transforms.is_empty(),
            "AVIF with a container crop, mirror or rotation is not supported for conversion."
        );
        ensure!(
            !props.nclx.is_some_and(|c| matches!(c.transfer, 16 | 18)),
            "PQ/HLG HDR AVIF conversion requires tone mapping or HDR output."
        );
        tags.icc = props.icc.map(<[u8]>::to_vec);
        if tags.icc.is_none()
            && let Some(color) = props.nclx
        {
            if color.primaries == 12 && color.transfer == 13 {
                tags.icc = Some(moxcms::ColorProfile::new_display_p3().encode()?);
            } else if !(matches!(color.primaries, 1 | 2) && matches!(color.transfer, 1 | 2 | 13)) {
                warnings.push("The AVIF color declaration cannot be embedded as an ICC profile. Output appearance may differ.".into());
            }
        }
        if let Some(item) = ctx.exif_item(id) {
            tags.exif = Some(heic_rs::metadata::exif_tiff(&ctx.item_data(item)?)?.to_vec());
        }
        if let Some(item) = ctx.xmp_item(id) {
            tags.xmp = Some(ctx.item_data(item)?.into_owned());
        }
        orientation = exif_orientation(&tags);
    } else {
        warnings.push(
            "The AVIF metadata container could not be read. Auxiliary properties may not transfer."
                .into(),
        );
    }
    macro_rules! convert_pixels {
        ($data:expr, $variant:path, $sample:ty) => {{
            let (width, height) = ($data.width() as u32, $data.height() as u32);
            let pixels: Vec<$sample> = bytemuck::cast_slice($data.buf()).to_vec();
            image::ImageBuffer::from_raw(width, height, pixels).map($variant)
        }};
    }
    use avif_decode::Image::*;
    let max_pixels = (limit / 20).clamp(1, u64::from(u32::MAX)) as u32;
    let image =
        match avif_decode::Decoder::from_reader_with_limit(&mut bytes.as_ref(), max_pixels)?
            .to_image()?
        {
            Rgb8(data) => convert_pixels!(data, DynamicImage::ImageRgb8, u8),
            Rgba8(data) => convert_pixels!(data, DynamicImage::ImageRgba8, u8),
            Gray8(data) => convert_pixels!(data, DynamicImage::ImageLuma8, u8),
            Rgb16(data) => convert_pixels!(data, DynamicImage::ImageRgb16, u16),
            Rgba16(data) => convert_pixels!(data, DynamicImage::ImageRgba16, u16),
            Gray16(data) => convert_pixels!(data, DynamicImage::ImageLuma16, u16),
        }
        .context("Invalid AVIF pixel buffer")?;
    Ok((image, tags, orientation))
}

fn decode_jxl(
    bytes: &Bytes,
    limit: u64,
    mode: MetadataMode,
) -> Result<(DynamicImage, Tags, Orientation)> {
    let header = jxl_oxide::JxlImage::builder()
        .read(Cursor::new(bytes))
        .map_err(anyhow::Error::from_boxed)?;
    ensure!(
        header.image_header().metadata.animation.is_none(),
        "Animated JPEG XL conversion would lose frames."
    );
    drop(header);
    let mut decoder = jxl_oxide::integration::JxlDecoder::new(Cursor::new(bytes))?;
    decoder.set_limits(reader_limits(limit))?;
    let tags = image_tags(&mut decoder, Format::Jxl, bytes, mode)?;
    let orientation = decoder.orientation()?;
    Ok((DynamicImage::from_decoder(decoder)?, tags, orientation))
}

fn pixels_require_alpha(image: &DynamicImage) -> bool {
    if !image.color().has_alpha() {
        return false;
    }
    match image {
        DynamicImage::ImageRgba16(pixels) => pixels.pixels().any(|pixel| pixel.0[3] != u16::MAX),
        DynamicImage::ImageLumaA16(pixels) => pixels.pixels().any(|pixel| pixel.0[1] != u16::MAX),
        _ => image.to_rgba8().pixels().any(|pixel| pixel.0[3] != u8::MAX),
    }
}

fn flatten_white(image: &DynamicImage) -> DynamicImage {
    let rgba = image.to_rgba8();
    let rgb = image::RgbImage::from_fn(rgba.width(), rgba.height(), |x, y| {
        let [r, g, b, a] = rgba.get_pixel(x, y).0;
        let blend =
            |c: u8| ((u32::from(c) * u32::from(a) + 255 * u32::from(255 - a) + 127) / 255) as u8;
        image::Rgb([blend(r), blend(g), blend(b)])
    });
    DynamicImage::ImageRgb8(rgb)
}

fn encode(
    image: DynamicImage,
    output: Format,
    quality: u8,
    tags: &mut Tags,
    advanced: &Advanced,
    warnings: &mut Vec<String>,
) -> Result<Vec<u8>> {
    let mut image = image;
    if output == Format::Jpeg && pixels_require_alpha(&image) {
        ensure!(
            advanced.flatten_alpha,
            "JPEG cannot store transparency. Choose a format with alpha or enable white background flattening."
        );
        image = flatten_white(&image);
        warnings.push("Transparency was flattened onto white.".into());
    }
    if output == Format::Gif {
        warnings.push("GIF output uses palette color and one-bit transparency.".into());
        if image.color().has_alpha()
            && image
                .to_rgba8()
                .pixels()
                .any(|p| !matches!(p.0[3], 0 | 255))
        {
            bail!("GIF cannot store partial transparency. Choose PNG, WebP, TIFF or AVIF.");
        }
    }
    if image.color().bits_per_pixel() / u16::from(image.color().channel_count()) > 8
        && !matches!(output, Format::Png | Format::Tiff)
    {
        warnings.push("Output encoding reduces this image to 8-bit samples.".into());
    }
    let mut encoded = Vec::new();
    let (width, height) = (image.width(), image.height());
    match output {
        Format::Jpeg => {
            let encoder = JpegEncoder::new_with_quality(&mut encoded, quality.clamp(1, 100));
            if matches!(
                image.color(),
                image::ColorType::L8
                    | image::ColorType::L16
                    | image::ColorType::La8
                    | image::ColorType::La16
            ) {
                encoder.write_image(
                    image.to_luma8().as_raw(),
                    width,
                    height,
                    image::ExtendedColorType::L8,
                )?;
            } else {
                encoder.write_image(
                    image.to_rgb8().as_raw(),
                    width,
                    height,
                    image::ExtendedColorType::Rgb8,
                )?;
            }
        }
        Format::Png => PngEncoder::new(&mut encoded).write_image(
            image.as_bytes(),
            width,
            height,
            image.color().into(),
        )?,
        Format::Webp => WebPEncoder::new_lossless(&mut encoded).encode(
            image.to_rgba8().as_raw(),
            width,
            height,
            image::ExtendedColorType::Rgba8,
        )?,
        Format::Tiff => {
            let mut encoder = TiffEncoder::new(Cursor::new(&mut encoded));
            if let Some(profile) = tags.icc.take() {
                encoder.set_icc_profile(profile)?;
            }
            encoder.write_image(image.as_bytes(), width, height, image.color().into())?;
        }
        Format::Avif => {
            let mut encoder =
                AvifEncoder::new_with_speed_quality(&mut encoded, 8, quality.clamp(1, 100));
            if let Some(exif) = tags.exif.take() {
                encoder.set_exif_metadata(exif)?;
            }
            encoder.write_image(
                image.to_rgba8().as_raw(),
                width,
                height,
                image::ExtendedColorType::Rgba8,
            )?;
        }
        Format::Bmp | Format::Gif => {
            image.write_to(Cursor::new(&mut encoded), output.image().unwrap())?
        }
        _ => bail!("This output format has no encoder."),
    }
    Ok(encoded)
}

fn pixel_conversion(
    bytes: &Bytes,
    input: Format,
    output: Format,
    request: &Request,
    advanced: &Advanced,
    limit: u64,
    warnings: &mut Vec<String>,
) -> Result<Bytes> {
    let (mut pixels, tags, orientation) = match input {
        Format::Heic => decode_heic(bytes, limit, request.metadata, warnings)?,
        Format::Avif => decode_avif(bytes, limit, warnings)?,
        Format::Jxl => decode_jxl(bytes, limit, request.metadata)?,
        _ => decode_common(bytes, input, limit, request.metadata)?,
    };
    pixels.apply_orientation(orientation);
    let changes = Changes {
        mode: request.metadata,
        edits: &request.edits,
        reset_orientation: true,
    };
    let mut tags = metadata::update(tags, &changes, output)?;
    let mut encoded = Bytes::from(encode(
        pixels,
        output,
        request.quality,
        &mut tags,
        advanced,
        warnings,
    )?);
    if !matches!(output, Format::Jpeg | Format::Png) && tags.iptc.take().is_some() {
        warnings.push("The output format cannot store IPTC IIM metadata.".into());
    }
    if metadata::supports_rewrite(output) {
        if tags.exif.is_some() || tags.xmp.is_some() || tags.iptc.is_some() || tags.icc.is_some() {
            encoded = metadata::write(encoded, tags, &changes, output, false)?;
        }
    } else {
        if tags.exif.is_some() {
            warnings.push("The output encoder cannot transfer EXIF.".into());
        }
        if tags.xmp.is_some() {
            warnings.push("The output encoder cannot transfer XMP.".into());
        }
        if tags.icc.is_some() {
            warnings.push(
                "The output encoder cannot embed the ICC profile. Appearance may differ.".into(),
            );
        }
        if request.metadata == MetadataMode::Edit
            && !request.edits.is_empty()
            && output != Format::Avif
        {
            bail!(
                "This output format cannot store the requested metadata edits. Select JPEG, PNG or WebP."
            );
        }
    }
    if request.metadata == MetadataMode::Preserve {
        warnings.push(
            "Conversion transfers supported metadata fields. Format-specific data may differ."
                .into(),
        );
    }
    Ok(encoded)
}

pub fn render(
    source: &Path,
    input: Format,
    output: Format,
    request: &Request,
    advanced: &Advanced,
    memory_mib: u32,
    writer: &mut File,
) -> Result<Vec<String>> {
    let mode = request.metadata;
    if input == output
        && (mode == MetadataMode::Preserve
            || (mode == MetadataMode::Edit && request.edits.is_empty()))
    {
        std::io::copy(&mut File::open(source)?, writer)?;
        return Ok(Vec::new());
    }
    let limit = u64::from(memory_mib) * MIB;
    let bytes = read_bounded(source, limit / 3)?;
    let mut warnings = Vec::new();
    let encoded = if input == output && metadata::supports_rewrite(input) {
        if input == Format::Jpeg {
            metadata::check_jpeg_metadata(&bytes, mode)?;
        }
        let mut reader = ImageReader::with_format(Cursor::new(&bytes), input.image().unwrap());
        reader.limits(reader_limits(limit));
        let mut decoder = reader.into_decoder()?;
        let orientation = decoder.orientation()?;
        if mode == MetadataMode::RemoveAll && orientation != Orientation::NoTransforms {
            warnings.push(
                "Pixels were re-encoded to preserve the visible orientation while removing EXIF."
                    .into(),
            );
            pixel_conversion(
                &bytes,
                input,
                output,
                request,
                advanced,
                limit,
                &mut warnings,
            )?
        } else {
            let tags = image_tags(&mut decoder, input, &bytes, mode)?;
            let changes = Changes {
                mode,
                edits: &request.edits,
                reset_orientation: false,
            };
            let tags = metadata::update(tags, &changes, output)?;
            metadata::write(bytes.clone(), tags, &changes, output, true)?
        }
    } else if input == output && input == Format::Gif && mode == MetadataMode::RemoveAll {
        gif_blocks(&bytes, true)?.1.unwrap()
    } else {
        if input == output {
            ensure!(
                mode == MetadataMode::RemoveAll
                    && matches!(input, Format::Tiff | Format::Bmp | Format::Avif),
                "This format cannot be safely edited in place. Choose JPEG, PNG or WebP as the output."
            );
        }
        pixel_conversion(
            &bytes,
            input,
            output,
            request,
            advanced,
            limit,
            &mut warnings,
        )?
    };
    if mode == MetadataMode::RemoveAll {
        warnings.push("Embedded descriptive metadata and ICC profiles were removed. Colors may appear different.".into());
    }
    if mode == MetadataMode::RemoveGps {
        warnings.push("Standard EXIF and XMP GPS fields were removed. Free text and proprietary blocks are outside this operation.".into());
    }
    if mode == MetadataMode::Edit && output == Format::Avif {
        warnings.push(
            "AVIF edits are limited to EXIF. Use JPEG or PNG for EXIF, XMP and IPTC together."
                .into(),
        );
    }
    writer.write_all(&encoded)?;
    Ok(warnings)
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageInfo {
    pub format: Format,
    pub width: u32,
    pub height: u32,
    pub bytes: u64,
    pub exif: bool,
    pub xmp: bool,
    pub iptc: bool,
    pub icc: bool,
    pub fields: std::collections::BTreeMap<String, String>,
    pub notes: Vec<String>,
}

pub fn info(path: &Path) -> Result<ImageInfo> {
    const LIMIT: u64 = 256 * MIB;
    let Kind::Supported(format) = sniff(path)? else {
        bail!("This file is not a supported image.");
    };
    let (width, height) = dimensions(path, format, LIMIT)?;
    let bytes = read_bounded(path, LIMIT)?;
    let mut notes = Vec::new();
    let tags = match format {
        Format::Heic | Format::Avif => match iso_context(&bytes, format) {
            Ok(ctx) => iso_tags(&ctx, ctx.meta.primary)?,
            Err(error) => {
                notes.push(format!(
                    "Cannot read the ISO image metadata container: {error}"
                ));
                Tags::default()
            }
        },
        Format::Jxl => {
            let mut decoder = jxl_oxide::integration::JxlDecoder::new(Cursor::new(&bytes))?;
            image_tags(&mut decoder, format, &bytes, MetadataMode::Preserve)?
        }
        _ => {
            let mut reader = ImageReader::with_format(Cursor::new(&bytes), format.image().unwrap());
            reader.limits(reader_limits(LIMIT));
            let mut decoder = reader.into_decoder()?;
            image_tags(&mut decoder, format, &bytes, MetadataMode::Preserve)?
        }
    };
    let fields = metadata::exif_summary(tags.exif.as_deref())?;
    Ok(ImageInfo {
        format,
        width,
        height,
        bytes: bytes.len() as u64,
        exif: tags.exif.is_some(),
        xmp: tags.xmp.is_some(),
        iptc: tags.iptc.is_some(),
        icc: tags.icc.is_some(),
        fields,
        notes,
    })
}

fn iso_tags(ctx: &heic_rs::context::Context<'_>, id: u32) -> Result<Tags> {
    let mut tags = Tags {
        icc: ctx.declared_icc(id)?.map(<[u8]>::to_vec),
        ..Tags::default()
    };
    if let Some(item) = ctx.exif_item(id) {
        tags.exif = Some(heic_rs::metadata::exif_tiff(&ctx.item_data(item)?)?.to_vec());
    }
    if let Some(item) = ctx.xmp_item(id) {
        tags.xmp = Some(ctx.item_data(item)?.into_owned());
    }
    Ok(tags)
}

fn iso_context(bytes: &[u8], format: Format) -> Result<heic_rs::context::Context<'_>> {
    if format == Format::Heic {
        return Ok(heic_rs::context::Context::open(bytes)?);
    }
    // HEIC's Context refuses AVIF's ftyp by design. Both use the same item metadata
    // boxes. Parse them without asking the HEVC reader to interpret AV1 pixels.
    use heic_rs::{Brand, context::Context, ftyp::FileType};
    Ok(Context {
        file: bytes,
        ftyp: FileType {
            major: Brand::Avif,
            effective: Brand::Avif,
            minor_version: 0,
            compatible_count: 0,
        },
        meta: heic_rs::meta::parse(bytes)?,
    })
}
