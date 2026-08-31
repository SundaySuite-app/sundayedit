//! Audio extraction + waveform peaks — Phase 1.2.
//!
//! The waveform is the user's main spatial reference in the editor, so it
//! must be instant. We:
//!   1. Extract the audio to a 16 kHz mono WAV via ffmpeg (also exactly
//!      what Whisper wants as input in Phase 2 — one extraction, two uses).
//!   2. Read the WAV samples (`hound`).
//!   3. Downsample to peak data at multiple zoom levels, cached to disk.
//!
//! The peak computation is a pure function tested against synthetic
//! samples — no audio file or ffmpeg required for the unit tests.

use std::path::Path;
use std::process::Command;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::error::{AppError, AppResult};
use crate::services::video::ffmpeg_path;

/// Whisper wants 16 kHz mono; the waveform is happy with it too.
pub const TARGET_SAMPLE_RATE: u32 = 16_000;

/// One vertical slice of the waveform: the min and max sample in a bucket.
/// Rendering draws a vertical line from `min` to `max` per pixel column.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../src/lib/bindings/Peak.ts")]
pub struct Peak {
    pub min: f32,
    pub max: f32,
}

/// Multi-resolution peak data. `levels[0]` is the coarsest (whole file in
/// few buckets); higher indices are finer. The editor picks the level
/// closest to the current pixel-per-second zoom.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../src/lib/bindings/WaveformData.ts")]
pub struct WaveformData {
    pub sample_rate: u32,
    #[ts(type = "number")]
    pub total_samples: u64,
    /// One entry per zoom level; each is a Vec<Peak>.
    pub levels: Vec<Vec<Peak>>,
}

/// Extract audio to a 16 kHz mono WAV at `out_wav` using ffmpeg.
/// Returns the command that was run on success (for logging/diagnostics).
pub fn extract_audio_wav(input: &Path, out_wav: &Path) -> AppResult<()> {
    if !input.exists() {
        return Err(AppError::VideoMissing(input.to_string_lossy().to_string()));
    }
    if let Some(parent) = out_wav.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let status = Command::new(ffmpeg_path())
        .arg("-y") // overwrite
        .arg("-i")
        .arg(input)
        .args(["-ac", "1"]) // mono
        .args(["-ar", &TARGET_SAMPLE_RATE.to_string()]) // 16 kHz
        .args(["-c:a", "pcm_s16le"]) // 16-bit PCM
        .arg("-vn") // drop video
        .arg(out_wav)
        .status()
        .map_err(|e| AppError::Internal(format!("failed to launch ffmpeg: {e}")))?;

    if !status.success() {
        return Err(AppError::Internal(format!(
            "ffmpeg audio extraction failed for '{}'",
            input.display()
        )));
    }
    Ok(())
}

/// Read a 16-bit PCM mono WAV and return normalized f32 samples in [-1, 1].
///
/// Used by local ASR, which needs one contiguous `&[f32]` — whisper-rs's API
/// takes the whole buffer at once, so there is no way to avoid holding it
/// resident on that path without changing the ASR integration itself (see
/// the module doc). The waveform-peaks path does NOT go through this
/// function any more; it streams (`stream_compute_levels`) instead.
pub fn read_wav_samples(path: &Path) -> AppResult<(Vec<f32>, u32)> {
    let reader = hound::WavReader::open(path)
        .map_err(|e| AppError::Internal(format!("failed to open WAV: {e}")))?;
    let spec = reader.spec();
    let sample_rate = spec.sample_rate;
    // `len()` is the WAV header's own claimed sample count (interleaved
    // across channels) — reserving it up front means the loops below fill
    // ONE allocation instead of `Vec`'s amortized-doubling growth, which both
    // re-copies on every grow step and briefly holds two buffers at once (a
    // real cost here: a 2h 16kHz mono file is ~115M samples, ~461MB as f32 —
    // doubling growth adds a transient ~2x peak on top of that final size).
    let expected = reader.len() as usize;
    let mut samples: Vec<f32> = Vec::with_capacity(expected);

    match spec.sample_format {
        hound::SampleFormat::Int => {
            // hound only enforces "multiple of 8"; a crafted/extensible header
            // can still advertise an out-of-range depth. Integer samples are
            // read as i32, so anything above 32 bits is unrepresentable — and a
            // depth ≥ 33 would overflow the `1 << (bits - 1)` shift below
            // (panic in debug, garbage divisor in release). Reject up front.
            if spec.bits_per_sample == 0 || spec.bits_per_sample > 32 {
                return Err(AppError::Internal(format!(
                    "unsupported WAV bit depth: {} (expected 1..=32)",
                    spec.bits_per_sample
                )));
            }
            let max = (1i64 << (spec.bits_per_sample - 1)) as f32;
            for s in reader.into_samples::<i32>().flatten() {
                samples.push(s as f32 / max);
            }
        }
        hound::SampleFormat::Float => {
            for s in reader.into_samples::<f32>().flatten() {
                samples.push(s);
            }
        }
    }
    Ok((samples, sample_rate))
}

/// The half-open sample range bucket `i` of `bucket_count` buckets covers,
/// out of `len` total samples. Shared by `compute_peaks` (reduces over an
/// in-memory slice) and the streaming path below (reduces over a sequential
/// read) so both bucketing schemes are the exact same formula — an in-memory
/// waveform and a streamed one must never disagree about where a bucket
/// boundary falls.
fn bucket_range(i: usize, len: usize, bucket_count: usize) -> (usize, usize) {
    let per_bucket = len as f64 / bucket_count as f64;
    let start = (i as f64 * per_bucket).floor() as usize;
    let end = (((i + 1) as f64 * per_bucket).floor() as usize)
        .min(len)
        .max(start + 1);
    (start, end)
}

/// Downsample samples into `bucket_count` peaks. Each peak holds the min
/// and max sample within its bucket — this preserves the visual envelope
/// of the waveform even at extreme zoom-out (a transient spike still
/// shows because it becomes the bucket's max).
pub fn compute_peaks(samples: &[f32], bucket_count: usize) -> Vec<Peak> {
    if samples.is_empty() || bucket_count == 0 {
        return Vec::new();
    }
    let bucket_count = bucket_count.min(samples.len());

    let mut peaks = Vec::with_capacity(bucket_count);
    for i in 0..bucket_count {
        let (start, end) = bucket_range(i, samples.len(), bucket_count);
        let slice = &samples[start..end];
        let mut min = f32::MAX;
        let mut max = f32::MIN;
        for &s in slice {
            if s < min {
                min = s;
            }
            if s > max {
                max = s;
            }
        }
        peaks.push(Peak { min, max });
    }
    peaks
}

/// Same bucketing math as `compute_peaks`, but reducing over an array of
/// already-computed `Peak`s instead of raw samples. A `Peak`'s min/max IS
/// the true envelope of the range it covers, so taking min-of-mins /
/// max-of-maxes over a sub-range of peaks yields EXACTLY the same result
/// `compute_peaks` would over the raw samples in that same range — the
/// reduction is associative and doesn't care how finely the input was
/// already bucketed. This is what lets `stream_compute_levels` derive every
/// coarser level from the finest one without ever re-touching the samples.
fn compute_peaks_from_peaks(peaks: &[Peak], bucket_count: usize) -> Vec<Peak> {
    if peaks.is_empty() || bucket_count == 0 {
        return Vec::new();
    }
    let bucket_count = bucket_count.min(peaks.len());
    let mut out = Vec::with_capacity(bucket_count);
    for i in 0..bucket_count {
        let (start, end) = bucket_range(i, peaks.len(), bucket_count);
        let mut min = f32::MAX;
        let mut max = f32::MIN;
        for p in &peaks[start..end] {
            if p.min < min {
                min = p.min;
            }
            if p.max > max {
                max = p.max;
            }
        }
        out.push(Peak { min, max });
    }
    out
}

/// Incremental bucketizer for the streaming waveform path: consumes one
/// normalized sample at a time and emits completed `Peak`s using the exact
/// same `bucket_range` boundaries `compute_peaks` would compute over the
/// same `(total_len, bucket_count)` pair — so the sample data itself never
/// needs to be resident all at once, only this small fixed amount of state.
struct StreamingPeaks {
    total_len: usize,
    bucket_count: usize,
    bucket: usize,
    range_end: usize,
    idx: usize,
    min: f32,
    max: f32,
    peaks: Vec<Peak>,
}

impl StreamingPeaks {
    fn new(total_len: usize, bucket_count: usize) -> Self {
        let bucket_count = if total_len == 0 {
            0
        } else {
            bucket_count.min(total_len)
        };
        let range_end = if bucket_count == 0 {
            0
        } else {
            bucket_range(0, total_len, bucket_count).1
        };
        StreamingPeaks {
            total_len,
            bucket_count,
            bucket: 0,
            range_end,
            idx: 0,
            min: f32::MAX,
            max: f32::MIN,
            peaks: Vec::with_capacity(bucket_count),
        }
    }

    fn push(&mut self, v: f32) {
        if self.bucket_count == 0 {
            return;
        }
        if v < self.min {
            self.min = v;
        }
        if v > self.max {
            self.max = v;
        }
        self.idx += 1;
        // `while`, not `if`: keeps this correct even if some pathological
        // bucket boundary ever produced an empty bucket (bucket_count is
        // clamped `<= total_len` above, so in practice this runs at most once
        // per sample).
        while self.idx >= self.range_end && self.bucket + 1 < self.bucket_count {
            self.peaks.push(Peak {
                min: self.min,
                max: self.max,
            });
            self.bucket += 1;
            self.min = f32::MAX;
            self.max = f32::MIN;
            self.range_end = bucket_range(self.bucket, self.total_len, self.bucket_count).1;
        }
    }

    /// Close out the final bucket and return every peak collected.
    fn finish(mut self) -> Vec<Peak> {
        if self.bucket_count > 0 && self.peaks.len() < self.bucket_count {
            self.peaks.push(Peak {
                min: self.min,
                max: self.max,
            });
        }
        self.peaks
    }
}

/// Stream the finest level's peaks directly off the WAV reader — samples are
/// consumed one at a time and folded into `StreamingPeaks`, so the file's
/// samples are never collected into a `Vec` for this path.
fn stream_finest_level(
    reader: hound::WavReader<std::io::BufReader<std::fs::File>>,
    spec: hound::WavSpec,
    total_samples: usize,
    bucket_count: usize,
) -> AppResult<Vec<Peak>> {
    let mut sp = StreamingPeaks::new(total_samples, bucket_count);
    match spec.sample_format {
        hound::SampleFormat::Int => {
            // Same crafted-header guard as `read_wav_samples` — see there.
            if spec.bits_per_sample == 0 || spec.bits_per_sample > 32 {
                return Err(AppError::Internal(format!(
                    "unsupported WAV bit depth: {} (expected 1..=32)",
                    spec.bits_per_sample
                )));
            }
            let max = (1i64 << (spec.bits_per_sample - 1)) as f32;
            for s in reader.into_samples::<i32>().flatten() {
                sp.push(s as f32 / max);
            }
        }
        hound::SampleFormat::Float => {
            for s in reader.into_samples::<f32>().flatten() {
                sp.push(s);
            }
        }
    }
    Ok(sp.finish())
}

/// Build multi-resolution peak data with a single sequential pass over the
/// WAV file — the whole-file `Vec<f32>` that `read_wav_samples` +
/// `compute_levels` would otherwise need is never allocated. Used by
/// `waveform_compute`, which only ever needs the peaks, never the raw
/// samples (local ASR does need them — see `read_wav_samples`'s doc comment
/// for why that side can't stream through the same whisper-rs call).
///
/// Levels are built with the SAME progression as `compute_levels`:
/// `base_buckets`, then ×`factor` per level, up to `max_levels` (stopping
/// early once a level would have more buckets than samples). Only the
/// FINEST level is streamed directly off the reader; every coarser level is
/// derived from it via `compute_peaks_from_peaks`, which is provably exact
/// (see that function's doc) rather than an approximation — confirmed
/// against `compute_levels` on the same samples by this module's tests.
pub fn stream_compute_levels(
    path: &Path,
    base_buckets: usize,
    factor: usize,
    max_levels: usize,
) -> AppResult<WaveformData> {
    let reader = hound::WavReader::open(path)
        .map_err(|e| AppError::Internal(format!("failed to open WAV: {e}")))?;
    let spec = reader.spec();
    let sample_rate = spec.sample_rate;
    let total_samples = reader.len() as usize;

    let mut bucket_counts = Vec::new();
    let mut buckets = base_buckets.max(1);
    for _ in 0..max_levels.max(1) {
        bucket_counts.push(buckets.min(total_samples));
        if total_samples == 0 || buckets >= total_samples {
            break;
        }
        buckets = buckets.saturating_mul(factor.max(2));
    }

    if total_samples == 0 {
        return Ok(WaveformData {
            sample_rate,
            total_samples: 0,
            levels: bucket_counts.iter().map(|_| Vec::new()).collect(),
        });
    }

    let finest_count = *bucket_counts
        .last()
        .expect("loop above always pushes at least one level");
    let finest = stream_finest_level(reader, spec, total_samples, finest_count)?;

    let mut levels: Vec<Vec<Peak>> = vec![Vec::new(); bucket_counts.len()];
    let last = levels.len() - 1;
    levels[last] = finest;
    for i in (0..last).rev() {
        levels[i] = compute_peaks_from_peaks(&levels[i + 1], bucket_counts[i]);
    }

    Ok(WaveformData {
        sample_rate,
        total_samples: total_samples as u64,
        levels,
    })
}

/// Build multi-resolution peak data. `base_buckets` is the coarsest
/// level; each subsequent level multiplies by `factor`. Levels stop once
/// a level would have more buckets than samples (no point oversampling).
pub fn compute_levels(
    samples: &[f32],
    sample_rate: u32,
    base_buckets: usize,
    factor: usize,
    max_levels: usize,
) -> WaveformData {
    let mut levels = Vec::new();
    let mut buckets = base_buckets.max(1);
    for _ in 0..max_levels {
        levels.push(compute_peaks(samples, buckets));
        if buckets >= samples.len() {
            break;
        }
        buckets = buckets.saturating_mul(factor.max(2));
    }
    WaveformData {
        sample_rate,
        total_samples: samples.len() as u64,
        levels,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_samples_produce_no_peaks() {
        assert!(compute_peaks(&[], 10).is_empty());
        assert!(compute_peaks(&[0.1, 0.2], 0).is_empty());
    }

    #[test]
    fn single_bucket_spans_whole_signal() {
        let samples = vec![-0.5, 0.3, -0.9, 0.7, 0.1];
        let peaks = compute_peaks(&samples, 1);
        assert_eq!(peaks.len(), 1);
        assert_eq!(peaks[0].min, -0.9);
        assert_eq!(peaks[0].max, 0.7);
    }

    #[test]
    fn peaks_preserve_transient_spike() {
        // A flat-ish signal with one big spike in the middle bucket.
        let mut samples = vec![0.01_f32; 300];
        samples[150] = 0.95; // spike
        samples[151] = -0.93;
        let peaks = compute_peaks(&samples, 3);
        assert_eq!(peaks.len(), 3);
        // The middle bucket must capture the spike envelope.
        assert!(peaks[1].max >= 0.95 - 0.001);
        assert!(peaks[1].min <= -0.93 + 0.001);
    }

    #[test]
    fn bucket_count_clamped_to_sample_count() {
        let samples = vec![0.5, -0.5];
        let peaks = compute_peaks(&samples, 100);
        assert_eq!(peaks.len(), 2, "can't have more buckets than samples");
    }

    #[test]
    fn all_samples_covered_no_gaps() {
        // Ramp from -1 to 1; min of first bucket should be near -1,
        // max of last bucket near +1 — proving full coverage.
        let n = 1000;
        let samples: Vec<f32> = (0..n)
            .map(|i| -1.0 + 2.0 * (i as f32 / (n - 1) as f32))
            .collect();
        let peaks = compute_peaks(&samples, 10);
        assert_eq!(peaks.len(), 10);
        assert!(peaks[0].min <= -0.99);
        assert!(peaks[9].max >= 0.99);
    }

    #[test]
    fn levels_get_progressively_finer() {
        let samples: Vec<f32> = (0..10_000).map(|i| ((i as f32) * 0.01).sin()).collect();
        let wf = compute_levels(&samples, 16_000, 100, 4, 4);
        assert_eq!(wf.sample_rate, 16_000);
        assert_eq!(wf.total_samples, 10_000);
        assert!(wf.levels.len() >= 2);
        // Each level finer than the last (until clamped)
        assert_eq!(wf.levels[0].len(), 100);
        assert_eq!(wf.levels[1].len(), 400);
    }

    #[test]
    fn levels_stop_when_buckets_exceed_samples() {
        let samples = vec![0.1_f32; 50];
        let wf = compute_levels(&samples, 16_000, 100, 4, 10);
        // base_buckets (100) already > 50 samples → clamped to 50, stop.
        assert_eq!(wf.levels.len(), 1);
        assert_eq!(wf.levels[0].len(), 50);
    }

    /// Write a minimal little-endian WAV header + body with a caller-chosen
    /// `bits_per_sample`. Used to feed `read_wav_samples` a hostile header.
    fn write_raw_wav(path: &Path, bits: u16, block_align: u16, body: &[u8]) {
        let n_channels: u16 = 1;
        let sample_rate: u32 = 16_000;
        let byte_rate: u32 = block_align as u32 * sample_rate;
        let mut fmt = Vec::new();
        fmt.extend_from_slice(&1u16.to_le_bytes()); // PCM
        fmt.extend_from_slice(&n_channels.to_le_bytes());
        fmt.extend_from_slice(&sample_rate.to_le_bytes());
        fmt.extend_from_slice(&byte_rate.to_le_bytes());
        fmt.extend_from_slice(&block_align.to_le_bytes());
        fmt.extend_from_slice(&bits.to_le_bytes());
        let mut chunks = Vec::new();
        chunks.extend_from_slice(b"fmt ");
        chunks.extend_from_slice(&(fmt.len() as u32).to_le_bytes());
        chunks.extend_from_slice(&fmt);
        chunks.extend_from_slice(b"data");
        chunks.extend_from_slice(&(body.len() as u32).to_le_bytes());
        chunks.extend_from_slice(body);
        let mut riff = Vec::new();
        riff.extend_from_slice(b"RIFF");
        riff.extend_from_slice(&((4 + chunks.len()) as u32).to_le_bytes());
        riff.extend_from_slice(b"WAVE");
        riff.extend_from_slice(&chunks);
        std::fs::write(path, &riff).unwrap();
    }

    // A crafted WAV whose fmt chunk advertises an out-of-range bits_per_sample
    // (72) — accepted by hound (it only requires a multiple of 8) — must not
    // panic on the `1i64 << (bits - 1)` shift. A shift of 64+ overflows i64
    // (debug panic / wrong divisor in release). The reader must reject such a
    // file with a clean error instead.
    #[test]
    fn rejects_wav_with_out_of_range_bit_depth() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.wav");
        // block_align 9 → bytes_per_sample 9, ×8 = 72 ≥ bits, passes hound.
        write_raw_wav(&path, 72, 9, &[0u8; 9]);
        let result = read_wav_samples(&path);
        assert!(
            result.is_err(),
            "an out-of-range bit depth must be a clean error, not a panic"
        );
    }

    // Round-trips a synthetic WAV through hound to exercise read_wav_samples
    // without needing ffmpeg or a real recording.
    #[test]
    fn reads_synthetic_wav() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tone.wav");
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 16_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        {
            let mut writer = hound::WavWriter::create(&path, spec).unwrap();
            // 0.5s of a 440 Hz tone
            for i in 0..8_000 {
                let t = i as f32 / 16_000.0;
                let v = (2.0 * std::f32::consts::PI * 440.0 * t).sin();
                writer.write_sample((v * i16::MAX as f32) as i16).unwrap();
            }
            writer.finalize().unwrap();
        }
        let (samples, sr) = read_wav_samples(&path).unwrap();
        assert_eq!(sr, 16_000);
        assert_eq!(samples.len(), 8_000);
        // A sine wave should swing close to ±1
        let max = samples.iter().cloned().fold(f32::MIN, f32::max);
        let min = samples.iter().cloned().fold(f32::MAX, f32::min);
        assert!(max > 0.9 && min < -0.9);
    }

    /// `read_wav_samples` must reserve the Vec's capacity up front from the
    /// WAV header's own sample count, not grow it via `collect`'s amortized
    /// doubling. Doubling growth would leave `capacity() > len()` (typically
    /// the next power of two above `len`) AND transiently hold two buffers
    /// at once while copying — a real cost on a multi-hundred-MB file. A
    /// reserved-up-front Vec's capacity is EXACTLY the sample count, which is
    /// what this measures (mutation-checked: reverting to `.collect()` turns
    /// this red — capacity lands at 16384 for 12_345 samples, not 12_345).
    #[test]
    fn read_wav_samples_reserves_exact_capacity_no_realloc_growth() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("capacity.wav");
        let n = 12_345usize;
        write_tone_wav(&path, n);

        let (samples, _sr) = read_wav_samples(&path).unwrap();
        assert_eq!(samples.len(), n);
        assert_eq!(
            samples.capacity(),
            n,
            "capacity must be reserved exactly from the WAV header's sample \
             count, not grown by doubling"
        );
    }

    /// Write a mono 16-bit PCM WAV holding `n` samples of a 440 Hz tone.
    fn write_tone_wav(path: &Path, n: usize) {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 16_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(path, spec).unwrap();
        for i in 0..n {
            let t = i as f32 / 16_000.0;
            let v = (2.0 * std::f32::consts::PI * 440.0 * t).sin();
            writer.write_sample((v * i16::MAX as f32) as i16).unwrap();
        }
        writer.finalize().unwrap();
    }

    /// The streamed multi-level computation must be BIT-IDENTICAL to loading
    /// the whole file and computing levels in memory — not merely visually
    /// similar. This is the case where no level gets clamped by the sample
    /// count (every level's bucket_count divides the next level's exactly),
    /// which is the common case for any real recording longer than a few
    /// seconds.
    #[test]
    fn stream_compute_levels_matches_in_memory_when_unclamped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stream_unclamped.wav");
        let n = 200_000usize; // 800 * 4^4 = 204_800 > n is NOT required here —
                              // this n keeps every level below n, so none clamp.
        write_tone_wav(&path, n);

        let (samples, sample_rate) = read_wav_samples(&path).unwrap();
        let expected = compute_levels(&samples, sample_rate, 800, 4, 3);

        let actual = stream_compute_levels(&path, 800, 4, 3).unwrap();
        assert_eq!(actual, expected);
        assert_eq!(actual.levels.len(), 3);
    }

    /// Same equivalence check, but with a sample count small enough that the
    /// FINEST requested level gets clamped down to one bucket per sample
    /// (`compute_levels`' own `bucket_count.min(samples.len())` rule) — the
    /// special case the streaming path's derivation reasoning depends on
    /// still being exact for.
    #[test]
    fn stream_compute_levels_matches_in_memory_when_finest_level_clamped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stream_clamped.wav");
        let n = 500usize; // well under base_buckets=800 → clamps immediately.
        write_tone_wav(&path, n);

        let (samples, sample_rate) = read_wav_samples(&path).unwrap();
        let expected = compute_levels(&samples, sample_rate, 800, 4, 5);
        let actual = stream_compute_levels(&path, 800, 4, 5).unwrap();

        assert_eq!(actual, expected);
        // Clamping means the loop stops after the very first (already
        // oversized) level — confirms this test actually exercises the
        // clamped branch instead of accidentally taking the unclamped path.
        assert_eq!(actual.levels.len(), 1);
        assert_eq!(actual.levels[0].len(), n);
    }

    /// A middle-ground sample count that clamps only the LAST of several
    /// levels, so most of the derivation chain exercises the unclamped
    /// (exact-factor) path and only the final hop exercises the clamped one.
    #[test]
    fn stream_compute_levels_matches_in_memory_when_only_last_level_clamped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stream_mixed.wav");
        // 800 -> 3200: 3200 > n, so level 1 clamps to n while level 0 (800)
        // stays unclamped — mixes both derivation cases in one file.
        let n = 3_000usize;
        write_tone_wav(&path, n);

        let (samples, sample_rate) = read_wav_samples(&path).unwrap();
        let expected = compute_levels(&samples, sample_rate, 800, 4, 5);
        let actual = stream_compute_levels(&path, 800, 4, 5).unwrap();

        assert_eq!(actual, expected);
        assert_eq!(actual.levels.len(), 2);
        assert_eq!(actual.levels[1].len(), n, "finest level clamped to n");
        assert_eq!(actual.levels[0].len(), 800, "coarser level stays exact");
    }

    /// An empty (zero-sample) WAV must not panic and must match
    /// `compute_levels`'s own empty-input behaviour: one empty level, not an
    /// error.
    #[test]
    fn stream_compute_levels_handles_empty_wav() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.wav");
        write_tone_wav(&path, 0);

        let actual = stream_compute_levels(&path, 800, 4, 5).unwrap();
        assert_eq!(actual.total_samples, 0);
        assert_eq!(actual.levels.len(), 1);
        assert!(actual.levels[0].is_empty());
    }
}
