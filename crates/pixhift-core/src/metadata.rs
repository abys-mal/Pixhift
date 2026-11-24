use crate::model::{Edits, Format, MetadataMode};
use anyhow::{Context, Result, bail, ensure};
use image::metadata::Orientation;
use img_parts::{
    Bytes, ImageEXIF, ImageICC,
    jpeg::{Jpeg, JpegSegment},
    png::{Png, PngChunk},
    riff::{RiffChunk, RiffContent},
    webp::WebP,
};
use little_exif::{
    exif_tag::ExifTag, filetype::FileExtension, ifd::ExifTagGroup, metadata::Metadata,
    rational::uR64,
};

const XMP_JPEG: &[u8] = b"http://ns.adobe.com/xap/1.0/\0";
const XMP_PNG: &[u8] = b"XML:com.adobe.xmp\0";
const IPTC_PNG: &[u8] = b"Raw profile type iptc\0";
const PHOTOSHOP: &[u8] = b"Photoshop 3.0\0";

#[derive(Default)]
pub struct Tags {
    pub exif: Option<Vec<u8>>,
    pub xmp: Option<Vec<u8>>,
    pub iptc: Option<Vec<u8>>,
    pub icc: Option<Vec<u8>>,
}

pub struct Changes<'a> {
    pub mode: MetadataMode,
    pub edits: &'a Edits,
    pub reset_orientation: bool,
}

pub fn validate_edits(edits: &Edits) -> Result<()> {
    if !edits.captured.trim().is_empty() {
        capture_date(&edits.captured)?;
    }
    coordinates(edits)?;
    for value in [
        &edits.author,
        &edits.copyright,
        &edits.title,
        &edits.description,
        &edits.make,
        &edits.model,
        &edits.lens,
    ] {
        ensure!(
            value.len() <= 4000,
            "Each metadata field can contain up to 4,000 UTF-8 bytes."
        );
        ensure!(
            !value
                .chars()
                .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t')),
            "Metadata fields contain an unsupported control character."
        );
    }
    Ok(())
}

fn capture_date(value: &str) -> Result<chrono::NaiveDateTime> {
    ["%Y-%m-%dT%H:%M:%S", "%Y-%m-%dT%H:%M"]
        .into_iter()
        .find_map(|format| chrono::NaiveDateTime::parse_from_str(value.trim(), format).ok())
        .context("Enter a valid capture date and time.")
}

fn coordinates(edits: &Edits) -> Result<Option<(f64, f64)>> {
    if edits.latitude.trim().is_empty() && edits.longitude.trim().is_empty() {
        return Ok(None);
    }
    let parse = |value: &str, max: f64| -> Result<f64> {
        let number: f64 = value
            .trim()
            .parse()
            .context("Enter both GPS coordinates as decimal degrees.")?;
        ensure!(
            (-max..=max).contains(&number),
            "GPS coordinates are outside their valid range."
        );
        Ok(number)
    };
    Ok(Some((
        parse(&edits.latitude, 90.0)?,
        parse(&edits.longitude, 180.0)?,
    )))
}

fn dms(value: f64) -> Vec<uR64> {
    let total = (value.abs() * 3_600_000.0).round() as u32;
    [
        (total / 3_600_000, 1),
        (total / 60_000 % 60, 1),
        (total % 60_000, 1000),
    ]
    .into_iter()
    .map(|(nominator, denominator)| uR64 {
        nominator,
        denominator,
    })
    .collect()
}

fn ascii(value: &str) -> String {
    value
        .trim()
        .chars()
        .map(|c| if c.is_ascii() { c } else { '?' })
        .collect()
}

fn edit_exif(raw: Option<Vec<u8>>, changes: &Changes<'_>) -> Result<Option<Vec<u8>>> {
    if changes.mode == MetadataMode::RemoveAll {
        return Ok(None);
    }
    let edit = changes.mode == MetadataMode::Edit && !changes.edits.is_empty();
    let has_orientation = raw
        .as_deref()
        .and_then(Orientation::from_exif_chunk)
        .is_some();
    if !edit
        && changes.mode != MetadataMode::RemoveGps
        && !(changes.reset_orientation && has_orientation)
    {
        return Ok(raw);
    }
    if raw.is_none() && !edit {
        return Ok(None);
    }
    let mut exif = match raw {
        Some(bytes) => {
            // A bare Exif payload is a classic TIFF directory, independent of the image container.
            ensure!(
                bytes.starts_with(b"II*\0") || bytes.starts_with(b"MM\0*"),
                "Invalid EXIF header."
            );
            Metadata::new_from_vec(&bytes, FileExtension::TIFF)
                .context("Cannot safely update this EXIF block")?
        }
        None => Metadata::new(),
    };
    if changes.reset_orientation {
        exif.remove_tag(ExifTag::Orientation(vec![]));
    }
    if changes.mode == MetadataMode::RemoveGps {
        let gps_ids: Vec<_> = exif
            .get_ifds()
            .iter()
            .filter(|ifd| ifd.get_ifd_type() == ExifTagGroup::GPS)
            .flat_map(|ifd| ifd.get_tags().iter().map(ExifTag::as_u16))
            .collect();
        for id in gps_ids {
            exif.remove_tag_by_hex_group(id, ExifTagGroup::GPS);
        }
        exif.remove_tag(ExifTag::GPSInfo(vec![]));
    }
    if edit {
        let values = changes.edits;
        for (value, tag) in [
            (&values.author, ExifTag::Artist as fn(String) -> ExifTag),
            (&values.copyright, ExifTag::Copyright),
            (&values.title, |value| {
                ExifTag::UnknownSTRING(value, 0x010d, ExifTagGroup::GENERIC)
            }),
            (&values.description, ExifTag::ImageDescription),
            (&values.make, ExifTag::Make),
            (&values.model, ExifTag::Model),
            (&values.lens, ExifTag::LensModel),
        ] {
            if !value.trim().is_empty() {
                exif.set_tag(tag(ascii(value)));
            }
        }
        if !values.captured.trim().is_empty() {
            exif.set_tag(ExifTag::DateTimeOriginal(
                capture_date(&values.captured)?
                    .format("%Y:%m:%d %H:%M:%S")
                    .to_string(),
            ));
            exif.remove_tag(ExifTag::SubSecTimeOriginal(String::new()));
            exif.remove_tag(ExifTag::OffsetTimeOriginal(String::new()));
        }
        if let Some((lat, lon)) = coordinates(values)? {
            exif.set_tag(ExifTag::GPSVersionID(vec![2, 3, 0, 0]));
            exif.set_tag(ExifTag::GPSLatitudeRef(
                if lat < 0.0 { "S" } else { "N" }.into(),
            ));
            exif.set_tag(ExifTag::GPSLatitude(dms(lat)));
            exif.set_tag(ExifTag::GPSLongitudeRef(
                if lon < 0.0 { "W" } else { "E" }.into(),
            ));
            exif.set_tag(ExifTag::GPSLongitude(dms(lon)));
        }
    }
    Ok(Some(exif.encode().context("Could not serialize EXIF")?))
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn location_key(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.contains("gps")
        || matches!(
            lower.as_str(),
            "latitude" | "longitude" | "altitude" | "geotag" | "geotags" | "geolocation"
        )
}

fn rewrite_xmp(packet: &[u8], appended: &[u8], remove: impl Fn(&str) -> bool) -> Result<Vec<u8>> {
    use quick_xml::{
        Reader, Writer,
        events::{BytesStart, Event},
    };

    std::str::from_utf8(packet).context("XMP editing requires a UTF-8 packet")?;
    let mut reader = Reader::from_reader(packet);
    let mut writer = Writer::new(Vec::with_capacity(packet.len() + appended.len()));
    let mut skipped = 0usize;
    let mut saw_rdf = false;
    let mut appended = Some(appended);
    loop {
        let event = reader
            .read_event()
            .context("Cannot safely parse this XMP packet")?;
        if skipped != 0 {
            match event {
                Event::Start(_) => skipped += 1,
                Event::End(_) => skipped -= 1,
                Event::Eof => bail!("XMP packet ends inside a metadata property."),
                _ => (),
            }
            continue;
        }
        match event {
            Event::Start(ref element) | Event::Empty(ref element) => {
                let empty = matches!(event, Event::Empty(_));
                let name = element.local_name();
                let local = std::str::from_utf8(name.as_ref())?;
                if remove(local) {
                    skipped = usize::from(!empty);
                    continue;
                }
                let name = element.name();
                let mut updated = BytesStart::new(std::str::from_utf8(name.as_ref())?);
                for attribute in element.attributes() {
                    let attribute = attribute?;
                    if !remove(std::str::from_utf8(attribute.key.local_name().as_ref())?) {
                        updated.push_attribute(attribute);
                    }
                }
                if empty && local == "RDF" {
                    saw_rdf = true;
                    let content = appended.take().unwrap_or_default();
                    if !content.is_empty() {
                        writer.write_event(Event::Start(updated.borrow()))?;
                        writer.get_mut().extend_from_slice(content);
                        writer.write_event(Event::End(updated.to_end()))?;
                        continue;
                    }
                }
                writer.write_event(if empty {
                    Event::Empty(updated)
                } else {
                    Event::Start(updated)
                })?;
            }
            Event::End(ref element) if element.local_name().as_ref() == b"RDF" => {
                saw_rdf = true;
                writer
                    .get_mut()
                    .extend_from_slice(appended.take().unwrap_or_default());
                writer.write_event(event)?;
            }
            Event::DocType(_) => bail!("XMP packets with a document type cannot be edited."),
            Event::Eof => break,
            event => writer.write_event(event)?,
        }
    }
    ensure!(saw_rdf, "XMP packet has no RDF element.");
    Ok(writer.into_inner())
}

fn edit_xmp(raw: Option<Vec<u8>>, changes: &Changes<'_>) -> Result<Option<Vec<u8>>> {
    if changes.mode == MetadataMode::RemoveAll {
        return Ok(None);
    }
    let values = changes.edits;
    let edit = changes.mode == MetadataMode::Edit && !values.is_empty();
    if !edit
        && ((!changes.reset_orientation && changes.mode != MetadataMode::RemoveGps)
            || raw.is_none())
    {
        return Ok(raw);
    }
    let packet = raw.as_deref().unwrap_or(
        br#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"/></x:xmpmeta>"#,
    );
    let mut properties = Vec::<(&str, String, bool)>::new();
    if edit {
        for (key, value, localized) in [
            ("dc:creator", &values.author, false),
            ("dc:rights", &values.copyright, true),
            ("dc:title", &values.title, true),
            ("dc:description", &values.description, true),
            ("tiff:Make", &values.make, false),
            ("tiff:Model", &values.model, false),
            ("aux:Lens", &values.lens, false),
        ] {
            if !value.trim().is_empty() {
                properties.push((key, value.trim().to_string(), localized));
            }
        }
        if !values.captured.trim().is_empty() {
            let date = capture_date(&values.captured)?
                .format("%Y-%m-%dT%H:%M:%S")
                .to_string();
            properties.extend([
                ("exif:DateTimeOriginal", date.clone(), false),
                ("photoshop:DateCreated", date, false),
            ]);
        }
        if let Some((lat, lon)) = coordinates(values)? {
            let coordinate = |value: f64, positive: char, negative: char| {
                let degrees = value.abs().floor();
                let minutes = (value.abs() - degrees) * 60.0;
                format!(
                    "{degrees},{minutes:.7}{}",
                    if value < 0.0 { negative } else { positive }
                )
            };
            properties.extend([
                ("exif:GPSLatitude", coordinate(lat, 'N', 'S'), false),
                ("exif:GPSLongitude", coordinate(lon, 'E', 'W'), false),
            ]);
        }
    }
    let remove = |name: &str| {
        (changes.mode == MetadataMode::RemoveGps && location_key(name))
            || (changes.reset_orientation && name == "Orientation")
            || properties
                .iter()
                .any(|(key, _, _)| key.split_once(':').is_some_and(|(_, local)| local == name))
    };
    let mut description = String::new();
    if !properties.is_empty() {
        description.push_str(
            r#"<rdf:Description rdf:about="" xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:exif="http://ns.adobe.com/exif/1.0/" xmlns:tiff="http://ns.adobe.com/tiff/1.0/" xmlns:aux="http://ns.adobe.com/exif/1.0/aux/" xmlns:photoshop="http://ns.adobe.com/photoshop/1.0/">"#,
        );
        for &(key, ref value, localized) in &properties {
            let value = xml_escape(value);
            if localized {
                description.push_str(&format!(r#"<{key}><rdf:Alt><rdf:li xml:lang="x-default">{value}</rdf:li></rdf:Alt></{key}>"#));
            } else if key == "dc:creator" {
                description.push_str(&format!(
                    "<{key}><rdf:Seq><rdf:li>{value}</rdf:li></rdf:Seq></{key}>"
                ));
            } else {
                description.push_str(&format!("<{key}>{value}</{key}>"));
            }
        }
        description.push_str("</rdf:Description>");
    }
    Ok(Some(rewrite_xmp(packet, description.as_bytes(), remove)?))
}

fn iptc_record(record: u8, id: u8, bytes: &[u8], output: &mut Vec<u8>) {
    output.extend([0x1c, record, id]);
    output.extend((bytes.len() as u16).to_be_bytes());
    output.extend(bytes);
}

fn edit_iptc(raw: Option<Vec<u8>>, values: &Edits) -> Result<Option<Vec<u8>>> {
    let date = if values.captured.trim().is_empty() {
        None
    } else {
        Some(capture_date(&values.captured)?)
    };
    let date_string = date
        .map(|date| date.format("%Y%m%d").to_string())
        .unwrap_or_default();
    let time_string = date
        .map(|date| date.format("%H%M%S").to_string())
        .unwrap_or_default();
    let fields = [
        (5, values.title.trim()),
        (55, date_string.as_str()),
        (60, time_string.as_str()),
        (80, values.author.trim()),
        (116, values.copyright.trim()),
        (120, values.description.trim()),
    ];
    if fields.iter().all(|(_, value)| value.is_empty()) {
        return Ok(raw);
    }
    let mut data = raw.as_deref().unwrap_or_default();
    let mut output = Vec::new();
    let mut utf8 = data.is_empty();
    if utf8 {
        iptc_record(1, 90, b"\x1b%G", &mut output);
        iptc_record(2, 0, &[0, 4], &mut output);
    }
    while !data.is_empty() {
        ensure!(
            data.len() >= 5 && data[0] == 0x1c,
            "Unsupported IPTC dataset."
        );
        let mut header = 5;
        let mut length = u16::from_be_bytes([data[3], data[4]]) as usize;
        if length & 0x8000 != 0 {
            let width = length & 0x7fff;
            ensure!(
                (1..=4).contains(&width) && data.len() >= 5 + width,
                "Invalid IPTC length."
            );
            length = data[5..5 + width]
                .iter()
                .fold(0, |size, byte| (size << 8) | usize::from(*byte));
            header += width;
        }
        let record = data
            .get(..header + length)
            .context("Truncated IPTC dataset")?;
        if data[1..3] == [1, 90] && &record[header..] == b"\x1b%G" {
            utf8 = true;
        }
        if data[1] != 2
            || !fields
                .iter()
                .any(|(id, value)| *id == data[2] && !value.is_empty())
        {
            output.extend(record);
        }
        data = &data[record.len()..];
    }
    for (id, value) in fields {
        if !value.is_empty() {
            let encoded = if utf8 {
                value.to_string()
            } else {
                ascii(value)
            };
            let max = match id {
                5 | 80 => 64,
                116 => 128,
                120 => 2000,
                _ => 16,
            };
            let mut end = encoded.len().min(max);
            while !encoded.is_char_boundary(end) {
                end -= 1;
            }
            iptc_record(2, id, &encoded.as_bytes()[..end], &mut output);
        }
    }
    Ok(Some(output))
}

struct PhotoshopBlock<'a> {
    id: u16,
    payload: &'a [u8],
    bytes: &'a [u8],
}

fn photoshop_blocks(mut bytes: &[u8]) -> Result<Vec<PhotoshopBlock<'_>>> {
    let mut blocks = Vec::new();
    while !bytes.is_empty() {
        ensure!(
            bytes.len() >= 12 && &bytes[..4] == b"8BIM",
            "Unsupported Photoshop metadata block."
        );
        let id = u16::from_be_bytes([bytes[4], bytes[5]]);
        let name = (usize::from(bytes[6]) + 2) & !1;
        let start = 6 + name + 4;
        let size = bytes
            .get(start - 4..start)
            .context("Truncated Photoshop metadata block")?;
        let size = u32::from_be_bytes(size.try_into()?) as usize;
        let end = start + size;
        let block = bytes
            .get(..end + (size % 2))
            .context("Truncated Photoshop metadata block")?;
        blocks.push(PhotoshopBlock {
            id,
            payload: &block[start..end],
            bytes: block,
        });
        bytes = &bytes[block.len()..];
    }
    Ok(blocks)
}

fn photoshop_iptc(data: &[u8]) -> Result<Option<Vec<u8>>> {
    Ok(photoshop_blocks(data)?
        .into_iter()
        .find(|block| block.id == 0x0404)
        .map(|block| block.payload.to_vec()))
}

fn append_iptc(output: &mut Vec<u8>, data: &[u8]) {
    output.extend(b"8BIM\x04\x04\0\0");
    output.extend((data.len() as u32).to_be_bytes());
    output.extend(data);
    if !data.len().is_multiple_of(2) {
        output.push(0);
    }
}

fn jpeg_segment(marker: u8, content: Vec<u8>) -> Result<JpegSegment> {
    ensure!(
        content.len() <= 65_533,
        "Metadata exceeds the size of a JPEG segment."
    );
    Ok(JpegSegment::new_with_contents(marker, content.into()))
}

fn rewrite_jpeg(bytes: Bytes, tags: Tags, changes: &Changes<'_>, same: bool) -> Result<Bytes> {
    let mode = changes.mode;
    if same && mode == MetadataMode::RemoveAll {
        return strip_jpeg(&bytes);
    }
    let mut jpeg = Jpeg::from_bytes(bytes)?;
    if !same || mode != MetadataMode::Preserve {
        if let Some(ref data) = tags.exif {
            ensure!(
                data.len() <= 65_527,
                "EXIF exceeds the size of a JPEG segment."
            );
        }
        jpeg.set_exif(tags.exif.map(Into::into));
        if !same {
            jpeg.set_icc_profile(tags.icc.map(Into::into));
        }
        let xmp_segment = tags
            .xmp
            .map(|data| jpeg_segment(0xe1, [XMP_JPEG, &data].concat()))
            .transpose()?;
        jpeg.segments_mut().retain(|segment| {
            !(segment.marker() == 0xe1 && segment.contents().starts_with(XMP_JPEG))
        });
        if let Some(segment) = xmp_segment {
            jpeg.segments_mut().insert(0, segment);
        }
        let edit_iptc_fields = mode == MetadataMode::Edit
            && [
                &changes.edits.author,
                &changes.edits.title,
                &changes.edits.description,
                &changes.edits.copyright,
                &changes.edits.captured,
            ]
            .iter()
            .any(|v| !v.trim().is_empty());
        if !same || edit_iptc_fields {
            let mut replacement = PHOTOSHOP.to_vec();
            if same {
                for segment in jpeg.segments_by_marker(0xed) {
                    if let Some(data) = segment.contents().strip_prefix(PHOTOSHOP) {
                        for block in photoshop_blocks(data)? {
                            // The optional IPTC digest covers the old payload.
                            if !matches!(block.id, 0x0404 | 0x0425) {
                                replacement.extend(block.bytes);
                            }
                        }
                    }
                }
            }
            if let Some(data) = tags.iptc {
                append_iptc(&mut replacement, &data);
            }
            jpeg.segments_mut()
                .retain(|s| s.marker() != 0xed || !s.contents().starts_with(PHOTOSHOP));
            if replacement.len() > PHOTOSHOP.len() {
                jpeg.segments_mut()
                    .insert(0, jpeg_segment(0xed, replacement)?);
            }
        }
    }
    Ok(jpeg.encoder().bytes())
}

fn strip_jpeg(bytes: &[u8]) -> Result<Bytes> {
    ensure!(bytes.starts_with(&[0xff, 0xd8]), "Invalid JPEG signature.");
    let mut output = Vec::with_capacity(bytes.len());
    output.extend(&bytes[..2]);
    let mut position = 2;
    let mut in_scan = false;
    loop {
        let start = position;
        ensure!(
            bytes.get(position) == Some(&0xff),
            "JPEG marker is invalid."
        );
        while bytes.get(position) == Some(&0xff) {
            position += 1;
        }
        let marker = *bytes.get(position).context("JPEG has no end marker")?;
        position += 1;
        if marker == 0xd9 {
            output.extend(&bytes[start..position]);
            return Ok(output.into());
        }
        if in_scan && (marker == 0 || (0xd0..=0xd7).contains(&marker)) {
            output.extend(&bytes[start..position]);
        } else {
            in_scan = marker == 0xda;
            let length = bytes
                .get(position..position + 2)
                .context("Truncated JPEG segment")?;
            let length = usize::from(u16::from_be_bytes(length.try_into()?));
            ensure!(length >= 2, "Invalid JPEG segment length.");
            let segment = bytes
                .get(position + 2..position + length)
                .context("Truncated JPEG segment")?;
            position += length;
            if !matches!(marker, 0xe0..=0xef | 0xfe)
                || (marker == 0xee && segment.starts_with(b"Adobe"))
            {
                output.extend(&bytes[start..position]);
            }
        }
        if in_scan {
            let count = bytes[position..]
                .iter()
                .position(|byte| *byte == 0xff)
                .context("JPEG scan has no end marker")?;
            output.extend(&bytes[position..position + count]);
            position += count;
        }
    }
}

fn png_keyword(chunk: &PngChunk, keyword: &[u8]) -> bool {
    matches!(&chunk.kind(), b"iTXt" | b"zTXt" | b"tEXt") && chunk.contents().starts_with(keyword)
}

fn rewrite_png(bytes: Bytes, tags: Tags, changes: &Changes<'_>, same: bool) -> Result<Bytes> {
    let mut png = Png::from_bytes(bytes)?;
    if same && changes.mode == MetadataMode::RemoveAll {
        // Keep pixel data, palette, transparency, animation and rendering declarations.
        png.chunks_mut().retain(|chunk| {
            matches!(
                &chunk.kind(),
                b"IHDR"
                    | b"PLTE"
                    | b"IDAT"
                    | b"IEND"
                    | b"tRNS"
                    | b"acTL"
                    | b"fcTL"
                    | b"fdAT"
                    | b"gAMA"
                    | b"cHRM"
                    | b"sRGB"
                    | b"cICP"
                    | b"sBIT"
                    | b"bKGD"
            )
        });
        return Ok(png.encoder().bytes());
    }
    let position = |png: &Png| {
        png.chunks()
            .iter()
            .position(|c| c.kind() == *b"IDAT")
            .unwrap_or(1)
    };
    png.set_exif(tags.exif.map(Into::into));
    if !same {
        png.set_icc_profile(tags.icc.map(Into::into));
    }
    png.chunks_mut().retain(|c| !png_keyword(c, XMP_PNG));
    if let Some(xmp) = tags.xmp {
        let at = position(&png);
        png.chunks_mut().insert(
            at,
            PngChunk::new(*b"iTXt", [XMP_PNG, b"\0\0\0\0", &xmp].concat().into()),
        );
    }
    if !same || changes.mode == MetadataMode::Edit {
        png.chunks_mut().retain(|c| !png_keyword(c, IPTC_PNG));
        if let Some(iptc) = tags.iptc {
            use std::fmt::Write as _;
            let mut profile = format!("\niptc\n{:8}\n", iptc.len());
            for line in iptc.chunks(36) {
                for byte in line {
                    write!(&mut profile, "{byte:02x}")?;
                }
                profile.push('\n');
            }
            let at = position(&png);
            png.chunks_mut().insert(
                at,
                PngChunk::new(*b"tEXt", [IPTC_PNG, profile.as_bytes()].concat().into()),
            );
        }
    }
    Ok(png.encoder().bytes())
}

fn rewrite_webp(bytes: Bytes, tags: Tags, changes: &Changes<'_>, same: bool) -> Result<Bytes> {
    let mut webp = WebP::from_bytes(bytes)?;
    webp.remove_chunks_by_id(*b"XMP ");
    if let Some(data) = tags.xmp {
        webp.chunks_mut()
            .push(RiffChunk::new(*b"XMP ", RiffContent::Data(data.into())));
    }
    if same && changes.mode == MetadataMode::RemoveAll {
        webp.chunks_mut().retain(|chunk| {
            matches!(
                &chunk.id(),
                b"VP8X" | b"VP8 " | b"VP8L" | b"ALPH" | b"ANIM" | b"ANMF"
            )
        });
    }
    webp.set_exif(tags.exif.map(Into::into));
    if !same {
        webp.set_icc_profile(tags.icc.map(Into::into));
    }
    let xmp = webp.has_chunk(*b"XMP ");
    if let Some(RiffContent::Data(data)) = webp
        .chunks_mut()
        .iter_mut()
        .find(|chunk| chunk.id() == *b"VP8X")
        .map(RiffChunk::content_mut)
    {
        let mut flags = data.to_vec();
        ensure!(flags.len() == 10, "Invalid WebP extended header.");
        flags[0] = (flags[0] & !4) | if xmp { 4 } else { 0 };
        *data = flags.into();
    }
    Ok(webp.encoder().bytes())
}

pub fn supports_rewrite(format: Format) -> bool {
    matches!(format, Format::Jpeg | Format::Png | Format::Webp)
}

pub fn read_jpeg_iptc(bytes: &[u8]) -> Result<Option<Vec<u8>>> {
    let jpeg = Jpeg::from_bytes(Bytes::copy_from_slice(bytes))?;
    for segment in jpeg.segments_by_marker(0xed) {
        if let Some(data) = segment.contents().strip_prefix(PHOTOSHOP)
            && let Some(iptc) = photoshop_iptc(data)?
        {
            return Ok(Some(iptc));
        }
    }
    Ok(None)
}

pub fn decode_png_iptc(bytes: &[u8]) -> Result<Vec<u8>> {
    let text = std::str::from_utf8(bytes)?;
    let mut tokens = text.split_ascii_whitespace();
    ensure!(
        tokens.next() == Some("iptc"),
        "Unsupported PNG IPTC profile."
    );
    let length: usize = tokens.next().context("Missing PNG IPTC length")?.parse()?;
    let encoded: String = tokens.collect();
    ensure!(
        encoded.len() / 2 == length && encoded.len().is_multiple_of(2),
        "Invalid PNG IPTC length."
    );
    encoded
        .as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|hex| Ok(u8::from_str_radix(std::str::from_utf8(hex)?, 16)?))
        .collect()
}

pub fn is_multipicture_jpeg(bytes: &[u8]) -> Result<bool> {
    let jpeg = Jpeg::from_bytes(Bytes::copy_from_slice(bytes))?;
    Ok(jpeg
        .segments_by_marker(0xe2)
        .any(|s| s.contents().starts_with(b"MPF\0")))
}

pub fn check_jpeg_metadata(bytes: &Bytes, mode: MetadataMode) -> Result<()> {
    let jpeg = Jpeg::from_bytes(bytes.clone())?;
    ensure!(
        !jpeg
            .segments_by_marker(0xe2)
            .any(|s| s.contents().starts_with(b"MPF\0")),
        "This multi-picture JPEG cannot be rewritten without risking auxiliary images."
    );
    if matches!(mode, MetadataMode::RemoveGps | MetadataMode::Edit) {
        ensure!(
            !jpeg.segments_by_marker(0xe1).any(|s| s
                .contents()
                .starts_with(b"http://ns.adobe.com/xmp/extension/\0")),
            "Extended JPEG XMP requires a different metadata editor."
        );
    }
    Ok(())
}

pub fn update(tags: Tags, changes: &Changes<'_>, format: Format) -> Result<Tags> {
    if changes.mode == MetadataMode::RemoveAll {
        return Ok(Tags::default());
    }
    let mut tags = Tags {
        exif: edit_exif(tags.exif, changes)?,
        xmp: edit_xmp(tags.xmp, changes)?,
        ..tags
    };
    if changes.mode == MetadataMode::Edit && matches!(format, Format::Jpeg | Format::Png) {
        tags.iptc = edit_iptc(tags.iptc, changes.edits)?;
    }
    Ok(tags)
}

pub fn write(
    bytes: Bytes,
    tags: Tags,
    changes: &Changes<'_>,
    format: Format,
    same: bool,
) -> Result<Bytes> {
    match format {
        Format::Jpeg => rewrite_jpeg(bytes, tags, changes, same),
        Format::Png => rewrite_png(bytes, tags, changes, same),
        Format::Webp => rewrite_webp(bytes, tags, changes, same),
        _ => bail!("This format has no container metadata writer."),
    }
}

pub fn exif_summary(raw: Option<&[u8]>) -> Result<std::collections::BTreeMap<String, String>> {
    let mut fields = std::collections::BTreeMap::new();
    let Some(raw) = raw else {
        return Ok(fields);
    };
    let tags =
        Metadata::new_from_vec(&raw.to_vec(), FileExtension::TIFF).context("Cannot parse EXIF")?;
    for ifd in tags.get_ifds() {
        for tag in ifd.get_tags() {
            let field = match tag {
                ExifTag::DateTimeOriginal(value) => ("captureDate", value.to_owned()),
                ExifTag::Artist(value) => ("author", value.to_owned()),
                ExifTag::Copyright(value) => ("copyright", value.to_owned()),
                ExifTag::ImageDescription(value) => ("description", value.to_owned()),
                ExifTag::Make(value) => ("cameraMake", value.to_owned()),
                ExifTag::Model(value) => ("cameraModel", value.to_owned()),
                ExifTag::LensModel(value) => ("lensModel", value.to_owned()),
                ExifTag::Orientation(value) => (
                    "orientation",
                    value.first().map(u16::to_string).unwrap_or_default(),
                ),
                ExifTag::GPSLatitude(_) => ("gps", "present".into()),
                ExifTag::UnknownSTRING(value, 0x010d, ExifTagGroup::GENERIC) => {
                    ("title", value.to_owned())
                }
                _ => continue,
            };
            fields
                .entry(field.0.into())
                .or_insert(field.1.trim_end_matches('\0').to_owned());
        }
    }
    Ok(fields)
}
