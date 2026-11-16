//! XACT `Wave Bank.xwb` / `Sound Bank.xsb` parsing.
//!
//! Terraria ships a XACT v46 wave bank containing stereo MS ADPCM tracks.
//! The sound bank provides the cue names (for example `Music_1`, `Music_2`, ...)
//! that map to wave bank indices.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;

use anyhow::{Context, Result, bail};

use super::{LogLevel, Options, Progress, WorkerMsg, format_bytes, log, wav};

#[derive(Debug, Default)]
pub struct MusicStats {
    pub exported: usize,
    pub skipped: usize,
    pub errors: usize,
    pub bytes_written: u64,
}

struct Entry {
    wave_format: u32,
    play_offset: u32,
    play_length: u32,
    duration: u32,
}

struct WaveBank {
    file: File,
    entries: Vec<Entry>,
    data_offset: u64,
    data_length: u64,
}

pub fn extract_music(
    content: &Path,
    output_root: &Path,
    options: &Options,
    cancel: &AtomicBool,
    _progress: &Progress,
    tx: &Sender<WorkerMsg>,
) -> Result<MusicStats> {
    let bank_path = content.join("Wave Bank.xwb");
    let mut bank =
        WaveBank::open(&bank_path).with_context(|| format!("opening {}", bank_path.display()))?;

    let names = match read_cue_names(&content.join("Sound Bank.xsb")) {
        Ok(names) => {
            log(
                tx,
                LogLevel::Info,
                format!(
                    "Loaded {} music cue name(s) from Sound Bank.xsb",
                    names.len()
                ),
            );
            names
        }
        Err(error) => {
            log(
                tx,
                LogLevel::Warn,
                format!("Could not read Sound Bank.xsb ({error:#}); using generic track names"),
            );
            HashMap::new()
        }
    };

    let music_dir = output_root.join("Music");
    std::fs::create_dir_all(&music_dir)
        .with_context(|| format!("creating {}", music_dir.display()))?;

    let mut stats = MusicStats::default();

    for index in 0..bank.entries.len() {
        if cancel.load(Ordering::Relaxed) {
            break;
        }

        let name = names
            .get(&index)
            .cloned()
            .unwrap_or_else(|| format!("Music_{:03}", index + 1));
        let output = music_dir.join(format!("{name}.wav"));

        if options.skip_existing && output.is_file() {
            stats.skipped += 1;
            if options.verbose {
                log(
                    tx,
                    LogLevel::Verbose,
                    format!("SKIP Music/{name}.wav (already exists)"),
                );
            }
            continue;
        }

        match bank.write_track(index, &output) {
            Ok((bytes, rate, channels, sample_count)) => {
                stats.exported += 1;
                stats.bytes_written += bytes;
                if options.verbose {
                    let seconds = sample_count as f64 / rate as f64;
                    log(
                        tx,
                        LogLevel::Verbose,
                        format!(
                            "WAV  Music/{name}.wav ({channels}ch {rate} Hz, {seconds:.1}s, {})",
                            format_bytes(bytes)
                        ),
                    );
                }
            }
            Err(error) => {
                stats.errors += 1;
                log(
                    tx,
                    LogLevel::Error,
                    format!("FAIL Music/{name}.wav: {error:#}"),
                );
            }
        }
    }

    Ok(stats)
}

impl WaveBank {
    fn open(path: &Path) -> Result<Self> {
        let mut file = File::open(path)?;
        let file_length = file.metadata()?.len();

        let mut header = [0u8; 52];
        file.read_exact(&mut header)?;

        if &header[0..4] != b"WBND" {
            bail!("not an XWB file");
        }
        let version = u32_le(&header[4..8]);
        if version < 42 {
            bail!("unsupported wave bank version {version}");
        }

        // Five {offset, length} segments starting at offset 12.
        let mut segments = [(0u64, 0u64); 5];
        for (index, segment) in segments.iter_mut().enumerate() {
            let offset = 12 + index * 8;
            *segment = (
                u32_le(&header[offset..offset + 4]) as u64,
                u32_le(&header[offset + 4..offset + 8]) as u64,
            );
        }

        let bank_offset = segments[0].0;
        let entry_offset = segments[1].0;
        let entry_length = segments[1].1;
        let data_offset = segments[4].0;
        let data_length = segments[4].1;

        if data_offset + data_length > file_length {
            bail!("wave data segment lies outside the file");
        }

        let prefix_length = entry_offset
            .checked_add(entry_length)
            .filter(|&end| end <= file_length)
            .context("entry metadata segment lies outside the file")?;

        file.seek(SeekFrom::Start(0))?;
        let mut prefix = vec![0u8; prefix_length as usize];
        file.read_exact(&mut prefix)?;

        let bank = &prefix[bank_offset as usize..];
        if bank.len() < 92 {
            bail!("bank data header is truncated");
        }

        let entry_count = u32_le(&bank[4..8]) as usize;
        let entry_size = u32_le(&bank[72..76]) as usize;
        if entry_size < 24 {
            bail!("unexpected wave bank entry size {entry_size}");
        }
        if entry_count > 1_000_000 {
            bail!("implausible wave bank entry count {entry_count}");
        }

        let entries_start = entry_offset as usize;
        let entries_end = entries_start + entry_count * entry_size;
        if entries_end > prefix.len() {
            bail!("entry metadata is truncated");
        }

        let mut entries = Vec::with_capacity(entry_count);
        for index in 0..entry_count {
            let offset = entries_start + index * entry_size;
            let record = &prefix[offset..offset + 24];
            entries.push(Entry {
                duration: u32_le(&record[0..4]) >> 4,
                wave_format: u32_le(&record[4..8]),
                play_offset: u32_le(&record[8..12]),
                play_length: u32_le(&record[12..16]),
            });
        }

        Ok(Self {
            file,
            entries,
            data_offset,
            data_length,
        })
    }

    fn write_track(&mut self, index: usize, output: &Path) -> Result<(u64, u32, u16, u32)> {
        let entry = self
            .entries
            .get(index)
            .context("wave bank entry index out of range")?;

        let tag = entry.wave_format & 0b11;
        let channels = ((entry.wave_format >> 2) & 0x7) as u16;
        let sample_rate = (entry.wave_format >> 5) & 0x3ffff;
        let stored_block_align = ((entry.wave_format >> 23) & 0xff) as u16;
        let bits_per_sample = 8u16 << ((entry.wave_format >> 31) & 1);

        if channels == 0 {
            bail!("wave format declares zero channels");
        }
        if sample_rate == 0 {
            bail!("wave format declares zero sample rate");
        }
        if tag != 2 {
            bail!("unsupported wave format tag {tag} (only MS ADPCM, tag 2, can be exported)");
        }
        if bits_per_sample != 8 {
            bail!("unexpected ADPCM bit depth {bits_per_sample}");
        }

        let block_align = (stored_block_align as u32 + 22)
            .checked_mul(channels as u32)
            .context("ADPCM block alignment overflow")?;
        if block_align <= 7 * channels as u32 {
            bail!("invalid ADPCM block alignment {block_align}");
        }
        let samples_per_block =
            ((block_align - 7 * channels as u32) * 2 / channels as u32 + 2) as u16;

        let end = entry.play_offset as u64 + entry.play_length as u64;
        if end > self.data_length {
            bail!("track data lies outside the wave data segment");
        }

        let mut data = vec![0u8; entry.play_length as usize];
        self.file
            .seek(SeekFrom::Start(self.data_offset + entry.play_offset as u64))?;
        self.file
            .read_exact(&mut data)
            .with_context(|| format!("reading track {index}"))?;

        let wav = wav::ms_adpcm(
            sample_rate,
            channels,
            block_align as u16,
            samples_per_block,
            entry.duration,
            &data,
        );

        std::fs::write(output, &wav).with_context(|| format!("writing {}", output.display()))?;

        Ok((wav.len() as u64, sample_rate, channels, entry.duration))
    }
}

/// Map wave bank track index -> cue name using the simple cues in `Sound Bank.xsb`.
fn read_cue_names(path: &Path) -> Result<HashMap<usize, String>> {
    let data = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    if data.len() < 46 || &data[0..4] != b"SDBK" {
        bail!("not a sound bank file");
    }

    let num_simple = u16_le(&data[19..21]) as usize;
    let cue_name_length = u16_le(&data[30..32]) as usize;
    let simple_offset = u32_le(&data[34..38]) as usize;
    let names_offset = u32_le(&data[42..46]) as usize;

    let names_end = names_offset
        .checked_add(cue_name_length)
        .filter(|&end| end <= data.len())
        .context("cue name table lies outside the file")?;

    let mut names: Vec<&str> = data[names_offset..names_end]
        .split(|&byte| byte == 0)
        .filter(|part| !part.is_empty())
        .map(|part| std::str::from_utf8(part).unwrap_or(""))
        .collect();

    let mut mapping = HashMap::new();
    for index in 0..num_simple {
        let cue = simple_offset + index * 5;
        if cue + 5 > data.len() || index >= names.len() {
            break;
        }

        let sound_offset = u32_le(&data[cue + 1..cue + 5]) as usize;
        if sound_offset + 12 > data.len() {
            continue;
        }
        // Bit 0 marks a complex (multi-clip) sound; those do not map to a single track.
        if data[sound_offset] & 1 != 0 {
            continue;
        }

        let track = u16_le(&data[sound_offset + 9..sound_offset + 11]) as usize;
        let wave_bank = data[sound_offset + 11];
        if wave_bank != 0 {
            continue;
        }

        let name = names[index].trim();
        if name.is_empty() {
            continue;
        }
        mapping.entry(track).or_insert_with(|| name.to_string());
    }

    names.clear();
    Ok(mapping)
}

fn u16_le(bytes: &[u8]) -> u16 {
    u16::from_le_bytes([bytes[0], bytes[1]])
}

fn u32_le(bytes: &[u8]) -> u32 {
    u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}
