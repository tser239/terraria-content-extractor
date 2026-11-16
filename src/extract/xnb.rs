//! XNB container parsing and asset conversion.
//!
//! Supports the four reader types present in Terraria:
//! * `Texture2DReader` -> PNG
//! * `SoundEffectReader` -> WAV (PCM)
//! * `DynamicSpriteFontReader` -> atlas page PNGs + glyph JSON
//! * `EffectReader` -> recognised but intentionally skipped

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

use anyhow::{Context, Result, bail};
use image::{ExtendedColorType, ImageEncoder};
use lzxd::{Lzxd, WindowSize};
use serde_json::json;

use super::{Options, Outcome, read_file, with_extension};

/// Decompress an XNB into a "virtual" uncompressed XNB: the original 10 byte
/// header (with the compression flag cleared) followed by the raw content.
pub fn decompress(raw: &[u8]) -> Result<Vec<u8>> {
    if raw.len() < 10 || &raw[0..3] != b"XNB" {
        bail!("not an XNB file");
    }

    let flags = raw[5];
    if flags & 0x40 != 0 {
        bail!("LZ4-compressed XNB is not supported");
    }
    if flags & 0x80 == 0 {
        return Ok(raw.to_vec());
    }
    if raw.len() < 14 {
        bail!("truncated XNB header");
    }

    let expected = u32::from_le_bytes(raw[10..14].try_into().unwrap()) as usize;
    let target = expected
        .checked_add(10)
        .context("decompressed size overflows")?;

    let mut out = Vec::with_capacity(target);
    out.extend_from_slice(&raw[..10]);
    out[5] &= 0x7f;
    out[6..10].copy_from_slice(&((target as u32).to_le_bytes()));

    let mut position = 14usize;
    let mut lzx = Lzxd::new(WindowSize::KB64);

    while position < raw.len() && out.len() < target {
        if position + 2 > raw.len() {
            bail!("truncated LZX block header");
        }

        let lead = raw[position];
        let (frame_size, block_size, header_len) = if lead == 0xFF {
            if position + 6 > raw.len() {
                bail!("truncated LZX frame header");
            }
            (
                u16::from_be_bytes([raw[position + 1], raw[position + 2]]) as usize,
                u16::from_be_bytes([raw[position + 3], raw[position + 4]]) as usize,
                5,
            )
        } else {
            (
                0x8000,
                u16::from_be_bytes([raw[position], raw[position + 1]]) as usize,
                2,
            )
        };

        if frame_size == 0 || block_size == 0 {
            break;
        }

        position += header_len;
        let end = position
            .checked_add(block_size)
            .filter(|&end| end <= raw.len())
            .context("LZX block runs past end of file")?;
        let chunk = &raw[position..end];
        position = end;

        let decoded = lzx
            .decompress_next(chunk, frame_size)
            .map_err(|e| anyhow::anyhow!("LZX decompression failed: {e:?}"))?;
        out.extend_from_slice(decoded);
    }

    if out.len() != target {
        bail!(
            "decompressed size mismatch: got {} bytes, expected {}",
            out.len(),
            target
        );
    }

    Ok(out)
}

/// A parsed XNB header: reader table plus the offset of the primary object.
pub struct Asset<'a> {
    pub readers: Vec<String>,
    pub primary_type: usize,
    body: usize,
    data: &'a [u8],
}

impl<'a> Asset<'a> {
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        if data.len() < 10 || &data[0..3] != b"XNB" {
            bail!("not an XNB file");
        }

        let mut reader = Reader { data, position: 10 };

        let reader_count = reader.seven_bit()? as usize;
        if reader_count > 4096 {
            bail!("implausible XNB reader count {reader_count}");
        }

        let mut readers = Vec::with_capacity(reader_count);
        for _ in 0..reader_count {
            readers.push(reader.string()?);
            reader.i32()?; // reader version
        }

        // Terraria's assets do not use shared resources; skip the count.
        let _shared_resources = reader.seven_bit()?;
        let primary_type = reader.seven_bit()? as usize;

        Ok(Self {
            readers,
            primary_type,
            body: reader.position,
            data,
        })
    }

    pub fn primary_reader(&self) -> Result<&str> {
        if self.primary_type == 0 || self.primary_type > self.readers.len() {
            bail!(
                "primary type id {} is out of range ({} readers)",
                self.primary_type,
                self.readers.len()
            );
        }
        Ok(self.readers[self.primary_type - 1].as_str())
    }

    fn body(&self) -> Reader<'a> {
        Reader {
            data: self.data,
            position: self.body,
        }
    }
}

struct Texture {
    format: i32,
    width: u32,
    height: u32,
    data: Vec<u8>,
}

struct Reader<'a> {
    data: &'a [u8],
    position: usize,
}

impl<'a> Reader<'a> {
    fn remaining(&self) -> usize {
        self.data.len().saturating_sub(self.position)
    }

    fn u8(&mut self) -> Result<u8> {
        let byte = *self
            .data
            .get(self.position)
            .context("unexpected end of XNB data")?;
        self.position += 1;
        Ok(byte)
    }

    fn take(&mut self, count: usize) -> Result<&'a [u8]> {
        let end = self
            .position
            .checked_add(count)
            .filter(|&end| end <= self.data.len())
            .with_context(|| {
                format!(
                    "wanted {count} bytes at offset {} but only {} remain",
                    self.position,
                    self.remaining()
                )
            })?;
        let slice = &self.data[self.position..end];
        self.position = end;
        Ok(slice)
    }

    fn i32(&mut self) -> Result<i32> {
        let bytes = self.take(4)?;
        Ok(i32::from_le_bytes(bytes.try_into().unwrap()))
    }

    fn f32(&mut self) -> Result<f32> {
        let bytes = self.take(4)?;
        Ok(f32::from_le_bytes(bytes.try_into().unwrap()))
    }

    /// .NET `BinaryReader.Read7BitEncodedInt`.
    fn seven_bit(&mut self) -> Result<u32> {
        let mut value = 0u32;
        let mut shift = 0u32;
        loop {
            let byte = self.u8()?;
            if shift >= 35 {
                bail!("invalid 7-bit encoded integer");
            }
            value |= ((byte & 0x7f) as u32) << shift;
            if byte & 0x80 == 0 {
                return Ok(value);
            }
            shift += 7;
        }
    }

    fn string(&mut self) -> Result<String> {
        let length = self.seven_bit()? as usize;
        let bytes = self.take(length)?;
        String::from_utf8(bytes.to_vec()).context("XNB string is not valid UTF-8")
    }

    /// A single UTF-8 encoded `System.Char`.
    fn char(&mut self) -> Result<char> {
        let lead = self.u8()?;
        let extra = match lead {
            0x00..=0x7f => 0,
            0xc0..=0xdf => 1,
            0xe0..=0xef => 2,
            0xf0..=0xf7 => 3,
            _ => bail!("invalid UTF-8 leading byte {lead:#04x} in char list"),
        };
        let mut bytes = [0u8; 4];
        bytes[0] = lead;
        if extra > 0 {
            bytes[1..1 + extra].copy_from_slice(self.take(extra)?);
        }
        let text = std::str::from_utf8(&bytes[..1 + extra]).context("invalid UTF-8 character")?;
        text.chars().next().context("empty UTF-8 character")
    }
}

pub fn extract(
    source: &Path,
    rel: &Path,
    output_root: &Path,
    options: &Options,
) -> Result<Outcome> {
    let raw = read_file(source)?;
    let data = decompress(&raw).with_context(|| format!("decompressing {}", source.display()))?;
    let asset = Asset::parse(&data)?;
    let reader = asset.primary_reader()?.to_string();

    if reader.contains("DynamicSpriteFontReader") {
        if !options.extract_fonts {
            return Ok(Outcome::Skipped {
                reason: "font extraction disabled".into(),
            });
        }
        return extract_font(&asset, rel, output_root, options);
    }

    if reader.contains("Texture2DReader") {
        return extract_texture(&asset, rel, output_root, options);
    }

    if reader.contains("SoundEffectReader") {
        return extract_sound(&asset, rel, output_root, options);
    }

    if reader.contains("EffectReader") {
        return Ok(Outcome::Unsupported {
            reason: "compiled shader effect has no useful direct export format".into(),
        });
    }

    bail!("unsupported XNB reader {reader}");
}

fn extract_texture(
    asset: &Asset,
    rel: &Path,
    output_root: &Path,
    options: &Options,
) -> Result<Outcome> {
    let mut reader = asset.body();
    let texture = read_texture(&mut reader)?;

    let output = output_root.join(with_extension(rel, "png"));
    if options.skip_existing && output.is_file() {
        return Ok(Outcome::Skipped {
            reason: "file already exists".into(),
        });
    }
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }

    let mut rgba = decode_texture(&texture)?;
    if options.unpremultiply {
        unpremultiply(&mut rgba);
    }
    let bytes = write_png(&output, texture.width, texture.height, &rgba)?;

    Ok(Outcome::Image {
        width: texture.width,
        height: texture.height,
        bytes,
    })
}

fn extract_sound(
    asset: &Asset,
    rel: &Path,
    output_root: &Path,
    options: &Options,
) -> Result<Outcome> {
    let mut reader = asset.body();

    let format_len = reader.i32()?;
    if format_len < 16 {
        bail!("sound effect format chunk is too short ({format_len} bytes)");
    }
    let format = reader.take(format_len as usize)?.to_vec();

    let data_len = reader.i32()?;
    if data_len < 0 {
        bail!("negative sound data length");
    }
    let data = reader.take(data_len as usize)?;

    let output = output_root.join(with_extension(rel, "wav"));
    if options.skip_existing && output.is_file() {
        return Ok(Outcome::Skipped {
            reason: "file already exists".into(),
        });
    }
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }

    let wav = super::wav::pcm(&format, data);
    std::fs::write(&output, &wav).with_context(|| format!("writing {}", output.display()))?;

    Ok(Outcome::Sound {
        bytes: wav.len() as u64,
    })
}

#[derive(Default)]
struct GlyphEntry {
    character: char,
    codepoint: u32,
    glyph: [i32; 4],
    cropping: [i32; 4],
    kerning: [f32; 3],
}

struct PageMeta {
    page: usize,
    texture: String,
    width: u32,
    height: u32,
    glyphs: Vec<GlyphEntry>,
}

fn extract_font(
    asset: &Asset,
    rel: &Path,
    output_root: &Path,
    options: &Options,
) -> Result<Outcome> {
    let mut reader = asset.body();

    let spacing = reader.f32()?;
    let line_spacing = reader.i32()?;
    let default_character = reader.char()?;
    let page_count = reader.i32()?;
    if !(1..=4096).contains(&page_count) {
        bail!("implausible font page count {page_count}");
    }

    let stem = rel
        .file_stem()
        .and_then(|s| s.to_str())
        .context("font file has no usable name")?
        .to_string();

    let output_dir = match rel.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => output_root.join(parent),
        _ => output_root.to_path_buf(),
    };

    let json_path = output_dir.join(format!("{stem}.json"));
    if options.skip_existing && json_path.is_file() {
        return Ok(Outcome::Skipped {
            reason: "font JSON already exists".into(),
        });
    }

    std::fs::create_dir_all(&output_dir)
        .with_context(|| format!("creating {}", output_dir.display()))?;

    let mut bytes_written = 0u64;
    let mut pages = Vec::with_capacity(page_count as usize);

    for page in 0..page_count as usize {
        let _type_id = reader.seven_bit()?;
        let texture = read_texture(&mut reader)?;

        let glyphs = read_rect_list(&mut reader)?;
        let cropping = read_rect_list(&mut reader)?;
        let characters = read_char_list(&mut reader)?;
        let kerning = read_vector3_list(&mut reader)?;

        let mut rgba = decode_texture(&texture)?;
        if options.unpremultiply {
            unpremultiply(&mut rgba);
        }

        let file_name = format!("{stem}_{page}.png");
        let path = output_dir.join(&file_name);
        bytes_written += write_png(&path, texture.width, texture.height, &rgba)?;

        let count = glyphs
            .len()
            .min(cropping.len())
            .min(characters.len())
            .min(kerning.len());
        let mut entries = Vec::with_capacity(count);
        for index in 0..count {
            entries.push(GlyphEntry {
                character: characters[index],
                codepoint: characters[index] as u32,
                glyph: glyphs[index],
                cropping: cropping[index],
                kerning: kerning[index],
            });
        }

        pages.push(PageMeta {
            page,
            texture: file_name,
            width: texture.width,
            height: texture.height,
            glyphs: entries,
        });
    }

    let document = json!({
        "name": stem,
        "spacing": spacing,
        "line_spacing": line_spacing,
        "default_character": default_character.to_string(),
        "pages": pages.iter().map(|page| json!({
            "page": page.page,
            "texture": page.texture,
            "width": page.width,
            "height": page.height,
            "glyphs": page.glyphs.iter().map(|glyph| json!({
                "character": glyph.character.to_string(),
                "codepoint": glyph.codepoint,
                "glyph": glyph.glyph,
                "cropping": glyph.cropping,
                "kerning": glyph.kerning,
            })).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
    });

    let encoded = serde_json::to_vec_pretty(&document).context("serialising font JSON")?;
    std::fs::write(&json_path, &encoded)
        .with_context(|| format!("writing {}", json_path.display()))?;
    bytes_written += encoded.len() as u64;

    Ok(Outcome::Font {
        name: stem,
        pages: pages.len(),
        bytes: bytes_written,
    })
}

fn read_texture(reader: &mut Reader) -> Result<Texture> {
    let format = reader.i32()?;
    let width = reader.i32()?;
    let height = reader.i32()?;
    let mip_count = reader.i32()?;

    if width <= 0 || height <= 0 {
        bail!("invalid texture size {width}x{height}");
    }
    if width > 16_384 || height > 16_384 {
        bail!("texture too large: {width}x{height}");
    }
    if !(1..=32).contains(&mip_count) {
        bail!("invalid mip level count {mip_count}");
    }

    let mut base = None;
    for level in 0..mip_count {
        let size = reader.i32()?;
        if size < 0 {
            bail!("negative mip level size");
        }
        let data = reader.take(size as usize)?;
        if level == 0 {
            base = Some(data.to_vec());
        }
    }

    Ok(Texture {
        format,
        width: width as u32,
        height: height as u32,
        data: base.context("texture has no base mip level")?,
    })
}

fn read_rect_list(reader: &mut Reader) -> Result<Vec<[i32; 4]>> {
    let _type_id = reader.seven_bit()?;
    let count = reader.i32()?;
    if count < 0 {
        bail!("negative list count {count}");
    }
    let mut items = Vec::with_capacity(count as usize);
    for _ in 0..count {
        items.push([reader.i32()?, reader.i32()?, reader.i32()?, reader.i32()?]);
    }
    Ok(items)
}

fn read_vector3_list(reader: &mut Reader) -> Result<Vec<[f32; 3]>> {
    let _type_id = reader.seven_bit()?;
    let count = reader.i32()?;
    if count < 0 {
        bail!("negative list count {count}");
    }
    let mut items = Vec::with_capacity(count as usize);
    for _ in 0..count {
        items.push([reader.f32()?, reader.f32()?, reader.f32()?]);
    }
    Ok(items)
}

fn read_char_list(reader: &mut Reader) -> Result<Vec<char>> {
    let _type_id = reader.seven_bit()?;
    let count = reader.i32()?;
    if count < 0 {
        bail!("negative list count {count}");
    }
    let mut items = Vec::with_capacity(count as usize);
    for _ in 0..count {
        items.push(reader.char()?);
    }
    Ok(items)
}

fn decode_texture(texture: &Texture) -> Result<Vec<u8>> {
    let width = texture.width as usize;
    let height = texture.height as usize;
    let blocks = width.div_ceil(4) * height.div_ceil(4);
    let expected = match texture.format {
        0 => width * height * 4,
        4 => blocks * 8,
        5 | 6 => blocks * 16,
        other => bail!("unsupported XNB surface format {other}"),
    };
    if texture.data.len() < expected {
        bail!(
            "texture data is too short: {} bytes, need {expected} for format {}",
            texture.data.len(),
            texture.format
        );
    }

    match texture.format {
        0 => Ok(texture.data[..expected].to_vec()),
        format => {
            let mut words = vec![0u32; width * height];
            let result = match format {
                4 => texture2ddecoder::decode_bc1(&texture.data, width, height, &mut words),
                5 => texture2ddecoder::decode_bc2(&texture.data, width, height, &mut words),
                6 => texture2ddecoder::decode_bc3(&texture.data, width, height, &mut words),
                _ => unreachable!(),
            };
            result.map_err(|e| anyhow::anyhow!("texture decoding failed: {e}"))?;

            let mut rgba = Vec::with_capacity(words.len() * 4);
            for word in words {
                let a = (word >> 24) as u8;
                let r = (word >> 16) as u8;
                let g = (word >> 8) as u8;
                let b = word as u8;
                rgba.extend_from_slice(&[r, g, b, a]);
            }
            Ok(rgba)
        }
    }
}

fn unpremultiply(rgba: &mut [u8]) {
    let (pixels, _) = rgba.as_chunks_mut::<4>();
    for pixel in pixels {
        let alpha = pixel[3] as u32;
        if alpha == 0 || alpha == 255 {
            continue;
        }
        for channel in &mut pixel[..3] {
            let value = (*channel as u32 * 255 + alpha / 2) / alpha;
            *channel = value.min(255) as u8;
        }
    }
}

fn write_png(path: &Path, width: u32, height: u32, rgba: &[u8]) -> Result<u64> {
    let expected = width as usize * height as usize * 4;
    if rgba.len() != expected {
        bail!(
            "internal error: image buffer has {} bytes, need {expected}",
            rgba.len()
        );
    }

    let file = File::create(path).with_context(|| format!("creating {}", path.display()))?;
    let mut writer = BufWriter::new(file);
    image::codecs::png::PngEncoder::new(&mut writer)
        .write_image(rgba, width, height, ExtendedColorType::Rgba8)
        .with_context(|| format!("encoding {}", path.display()))?;
    writer
        .flush()
        .with_context(|| format!("flushing {}", path.display()))?;
    drop(writer);

    Ok(std::fs::metadata(path)
        .with_context(|| format!("reading metadata for {}", path.display()))?
        .len())
}
