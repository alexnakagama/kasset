//! Audio output: decoding files and sending them to the default output device.
//!
//! [`Audio`] is the small interface the rest of the program drives; [`Player`]
//! implements it with `rodio`, and tests implement it with a fake so playback
//! logic can be checked without sound hardware.
//!
//! Every decoder is wrapped in [`Guarded`], which runs on rodio's audio thread.
//! Besides turning decoder panics into the end of the track, it is the audio
//! tap for the visualizer: it mixes each decoded frame to mono and hands the
//! samples over in batches to the track's [`TapBuffer`], which keeps the newest
//! [`FFT_SIZE`] of them for [`Audio::recent_samples`]. Tap invariants:
//! - the audio thread never blocks or allocates for it: the batch is allocated
//!   in [`Guarded::new`] and the ring with the track's [`SourceFlags`] (both
//!   on the UI thread), and a batch is handed over with `try_lock`, so it is
//!   dropped when the UI holds the lock. A dropped batch leaves a small jump
//!   in the ring's audio, which only blurs one spectrum frame; it is rare,
//!   since the UI holds the lock just long enough to copy the ring;
//! - it is taken before the volume (rodio applies the volume downstream of
//!   `Guarded`), so the samples do not depend on it;
//! - the ring only ever holds samples at one sample rate: a rate change clears
//!   it, and a channel-count change never mixes samples of two frame layouts;
//! - every `load` gets fresh [`SourceFlags`], so a seek or track change starts
//!   with an empty tap.

use std::fs::File;
use std::io::BufReader;
use std::panic::AssertUnwindSafe;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError, TryLockError};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use rodio::cpal::traits::HostTrait;
use rodio::source::SeekError;
use rodio::{ChannelCount, CpalError, Decoder, DeviceSinkBuilder, DeviceTrait, MixerDeviceSink};
use rodio::{Sample, SampleRate, Source};

use crate::panic_guard::quietly;
use crate::text::sanitize;
use crate::visualizer::FFT_SIZE;

/// How far one seek step moves.
pub const SEEK_STEP: Duration = Duration::from_secs(5);

/// Message used when the output device cannot be opened at startup.
pub const NO_DEVICE: &str = "No audio output device available";

/// Playback volume as a whole percentage, 0–100, changed in 5% steps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Volume(u8);

impl Volume {
    /// Full volume, the starting level.
    pub const MAX: Self = Self(100);
    /// Size of one volume step, in percent.
    pub const STEP: u8 = 5;

    /// A volume of `percent`, clamped to 0–100.
    #[must_use]
    pub fn new(percent: u8) -> Self {
        Self(percent.min(Self::MAX.0))
    }

    /// The level in percent.
    #[must_use]
    pub const fn percent(self) -> u8 {
        self.0
    }

    /// One step louder, saturating at 100%.
    #[must_use]
    pub fn up(self) -> Self {
        Self::new(self.0.saturating_add(Self::STEP))
    }

    /// One step quieter, saturating at 0%.
    #[must_use]
    pub const fn down(self) -> Self {
        Self(self.0.saturating_sub(Self::STEP))
    }

    /// Linear amplitude factor for the mixer (1.0 at 100%).
    #[must_use]
    pub fn gain(self) -> f32 {
        f32::from(self.0) / 100.0
    }
}

impl Default for Volume {
    fn default() -> Self {
        Self::MAX
    }
}

/// What the output is doing with the loaded track.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    /// Nothing is loaded.
    Idle,
    /// A track is loaded and has audio left (it may be paused).
    Active,
    /// The loaded track played to its end.
    Ended,
    /// The loaded track stopped because it could not be decoded.
    Failed(String),
}

/// What [`Audio::recent_samples`] found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Recent {
    /// Sample rate of the samples copied, in Hz.
    pub sample_rate: u32,
    /// Total samples ever decoded into the tap; unchanged between two calls
    /// means no new audio arrived.
    pub written: u64,
}

/// The operations playback logic needs from an audio output.
pub trait Audio {
    /// Opens `path`, positions it at `start` and starts it (or leaves it paused),
    /// replacing whatever was loaded. On error the previous track (if any) is
    /// left exactly as it was.
    ///
    /// # Errors
    ///
    /// Fails when the file cannot be opened, is not a decodable audio file, or
    /// cannot be positioned at `start`.
    fn load(&mut self, path: &Path, start: Duration, paused: bool) -> Result<()>;

    /// Pauses or resumes the loaded track, keeping its position.
    fn set_paused(&mut self, paused: bool);

    /// Stops and unloads the current track.
    fn stop(&mut self);

    /// Sets the output level for this and later tracks.
    fn set_volume(&mut self, volume: Volume);

    /// Position in the loaded track from its start (zero when idle).
    fn position(&self) -> Duration;

    /// Total length of the loaded track as reported by its decoder, if known.
    fn duration(&self) -> Option<Duration>;

    /// The state of the loaded track.
    fn status(&self) -> Status;

    /// The most recent output-device error since the last call, if any.
    fn take_device_error(&mut self) -> Option<String>;

    /// Clears `out`, then appends the newest decoded mono samples (at most
    /// [`FFT_SIZE`], oldest first). Returns `None` when no track is loaded or
    /// no samples have reached the tap yet (they arrive in batches, so the
    /// last few milliseconds of a track may never show). The default has no
    /// samples.
    fn recent_samples(&self, out: &mut Vec<f32>) -> Option<Recent> {
        out.clear();
        None
    }
}

/// Mono samples `Guarded` collects before handing them to the tap.
const TAP_BATCH: usize = 512;

/// Span length assumed when a source does not report one, so the format is
/// not re-read on every sample.
const UNKNOWN_SPAN: usize = 4096;

/// The newest decoded mono samples of a track, shared between its [`Guarded`]
/// source (audio thread, writer) and the [`Player`] (UI thread, reader).
#[derive(Debug)]
struct TapBuffer {
    /// Circular storage, always [`FFT_SIZE`] long, allocated up front so the
    /// audio thread never allocates.
    ring: Box<[f32]>,
    /// Where the next sample goes; the oldest sample once the ring is full.
    head: usize,
    /// How many samples of `ring` are valid, at most [`FFT_SIZE`].
    len: usize,
    /// Rate of the samples in `ring`; `None` until the first batch arrives.
    sample_rate: Option<SampleRate>,
    /// Total samples ever appended, across rate changes.
    written: u64,
}

impl Default for TapBuffer {
    fn default() -> Self {
        Self {
            ring: vec![0.0; FFT_SIZE].into_boxed_slice(),
            head: 0,
            len: 0,
            sample_rate: None,
            written: 0,
        }
    }
}

impl TapBuffer {
    /// Appends `samples` decoded at `rate`, dropping the oldest ones. Samples
    /// at a different rate than those held replace them instead.
    fn push(&mut self, samples: &[f32], rate: SampleRate) {
        if self.sample_rate != Some(rate) {
            self.sample_rate = Some(rate);
            self.head = 0;
            self.len = 0;
        }
        let capacity = self.ring.len();
        // Only the newest `capacity` samples can survive.
        let keep = &samples[samples.len().saturating_sub(capacity)..];
        for &sample in keep {
            if let Some(slot) = self.ring.get_mut(self.head) {
                *slot = sample;
            }
            self.head = (self.head + 1) % capacity;
        }
        self.len = (self.len + keep.len()).min(capacity);
        // A `usize` always fits in a `u64` on supported targets; saturate anyway.
        let added = u64::try_from(samples.len()).unwrap_or(u64::MAX);
        self.written = self.written.saturating_add(added);
    }

    /// Clears `out` and appends the held samples, oldest first.
    fn copy_to(&self, out: &mut Vec<f32>) -> Option<Recent> {
        out.clear();
        let rate = self.sample_rate?;
        let capacity = self.ring.len();
        let start = (self.head + capacity - self.len) % capacity;
        let end = start + self.len;
        if end <= capacity {
            out.extend_from_slice(self.ring.get(start..end).unwrap_or_default());
        } else {
            out.extend_from_slice(self.ring.get(start..).unwrap_or_default());
            out.extend_from_slice(self.ring.get(..end - capacity).unwrap_or_default());
        }
        Some(Recent {
            sample_rate: rate.get(),
            written: self.written,
        })
    }
}

/// State shared between a playing source (audio thread) and the [`Player`].
#[derive(Debug, Default)]
struct SourceFlags {
    /// At least one sample was decoded.
    produced: AtomicBool,
    /// The decoder panicked; the source ended early.
    panicked: AtomicBool,
    /// The newest decoded samples, for the visualizer.
    tap: Mutex<TapBuffer>,
}

/// The span format `Guarded` reads from its decoder after a sample.
type SpanFormat = (Option<usize>, ChannelCount, SampleRate);

/// A source wrapper that turns a decoder panic into the end of the track, and
/// taps the decoded samples for the visualizer.
///
/// Decoders parse untrusted files on the audio thread; a panic there would kill
/// the output stream for the rest of the session. Instead the panic is caught,
/// recorded in [`SourceFlags::panicked`] and the source reports exhaustion.
///
/// The tap tracks spans itself: rodio's `current_span_len()` is the total size
/// of the current span, not what is left of it, so `span_left` counts down,
/// and the format is re-read after the sample that starts a new span, and only
/// on a frame boundary.
struct Guarded<S> {
    inner: Option<S>,
    flags: Arc<SourceFlags>,
    produced: bool,
    /// Channel count of the current span (the last one, once `inner` is gone).
    channels: ChannelCount,
    /// Sample rate of the current span (the last one, once `inner` is gone).
    sample_rate: SampleRate,
    /// Samples left in the current span; 0 means "read the format again".
    span_left: usize,
    /// Sum of the samples of the frame being built.
    frame_sum: f32,
    /// Samples in the frame being built, below `channels`.
    frame_count: u16,
    /// Mono samples not yet handed to the tap, at most [`TAP_BATCH`].
    batch: Vec<f32>,
}

impl<S: Source> Guarded<S> {
    fn new(inner: S, flags: Arc<SourceFlags>) -> Self {
        Self {
            channels: inner.channels(),
            sample_rate: inner.sample_rate(),
            inner: Some(inner),
            flags,
            produced: false,
            span_left: 0,
            frame_sum: 0.0,
            frame_count: 0,
            batch: Vec::with_capacity(TAP_BATCH),
        }
    }

    fn poison(&mut self) {
        self.flags.panicked.store(true, Ordering::Release);
        // Dropping a decoder in an inconsistent state may panic again.
        let inner = self.inner.take();
        let _ignored = quietly(AssertUnwindSafe(move || drop(inner)));
    }

    /// Forgets the span and the samples not yet handed over, so the next
    /// sample starts a new span and a new frame.
    fn restart_tap(&mut self) {
        self.span_left = 0;
        self.frame_sum = 0.0;
        self.frame_count = 0;
        self.batch.clear();
    }

    /// Starts a new span with the format read from the decoder.
    fn start_span(&mut self, (len, channels, rate): SpanFormat) {
        self.span_left = match len {
            None | Some(0) => UNKNOWN_SPAN,
            Some(len) => len,
        };
        if channels != self.channels {
            self.channels = channels;
            self.frame_sum = 0.0;
            self.frame_count = 0;
        }
        if rate != self.sample_rate {
            self.sample_rate = rate;
            // The tap clears its ring when the next batch arrives at the new rate.
            self.batch.clear();
        }
    }

    /// Adds one decoded sample to the tap.
    fn tap(&mut self, sample: Sample) {
        self.span_left = self.span_left.saturating_sub(1);
        self.frame_sum += sample;
        self.frame_count += 1;
        let channels = self.channels.get();
        if self.frame_count < channels {
            return;
        }
        let mono = self.frame_sum / f32::from(channels);
        self.frame_sum = 0.0;
        self.frame_count = 0;
        // Never grows past the capacity reserved in `new`: it is flushed when full.
        self.batch.push(mono);
        if self.batch.len() >= TAP_BATCH {
            self.flush();
        }
    }

    /// Hands the batch to the tap if it is free, else drops it.
    fn flush(&mut self) {
        match self.flags.tap.try_lock() {
            Ok(mut tap) => tap.push(&self.batch, self.sample_rate),
            Err(TryLockError::Poisoned(poisoned)) => {
                poisoned.into_inner().push(&self.batch, self.sample_rate);
            }
            Err(TryLockError::WouldBlock) => {}
        }
        self.batch.clear();
    }
}

impl<S: Source> Iterator for Guarded<S> {
    type Item = Sample;

    fn next(&mut self) -> Option<Sample> {
        let inner = self.inner.as_mut()?;
        let new_span = self.span_left == 0 && self.frame_count == 0;
        // The decoder is dropped on panic and never touched again, so no broken
        // invariant can be observed. Requires `panic = "unwind"` (Cargo.toml).
        // The format is read after `next()`, which is when a decoder switches
        // to its next buffer.
        let read = quietly(AssertUnwindSafe(|| {
            let sample = inner.next();
            let format = (new_span && sample.is_some())
                .then(|| (inner.current_span_len(), inner.channels(), inner.sample_rate()));
            (sample, format)
        }));
        match read {
            Ok((Some(sample), format)) => {
                if !self.produced {
                    self.produced = true;
                    self.flags.produced.store(true, Ordering::Release);
                }
                if let Some(format) = format {
                    self.start_span(format);
                }
                self.tap(sample);
                Some(sample)
            }
            Ok((None, _)) => None,
            Err(_) => {
                self.poison();
                None
            }
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner
            .as_ref()
            .map_or((0, Some(0)), |inner| (0, inner.size_hint().1))
    }
}

impl<S: Source> Source for Guarded<S> {
    fn current_span_len(&self) -> Option<usize> {
        self.inner
            .as_ref()
            .map_or(Some(0), Source::current_span_len)
    }

    fn channels(&self) -> ChannelCount {
        self.inner.as_ref().map_or(self.channels, Source::channels)
    }

    fn sample_rate(&self) -> SampleRate {
        self.inner
            .as_ref()
            .map_or(self.sample_rate, Source::sample_rate)
    }

    fn total_duration(&self) -> Option<Duration> {
        self.inner.as_ref().and_then(Source::total_duration)
    }

    fn try_seek(&mut self, pos: Duration) -> Result<(), SeekError> {
        // Samples after the seek must not share a frame or a batch with
        // samples before it.
        self.restart_tap();
        let Some(inner) = self.inner.as_mut() else {
            return Err(SeekError::Other(Arc::new(DecoderCrashed)));
        };
        quietly(AssertUnwindSafe(|| inner.try_seek(pos))).unwrap_or_else(|_| {
            self.poison();
            Err(SeekError::Other(Arc::new(DecoderCrashed)))
        })
    }
}

/// Error reported when a decoder panicked.
#[derive(Debug)]
struct DecoderCrashed;

impl std::fmt::Display for DecoderCrashed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the decoder crashed on malformed data")
    }
}

impl std::error::Error for DecoderCrashed {}

/// A file opened but could not be positioned where asked.
#[derive(Debug)]
pub struct SeekFailed {
    /// The target lies beyond the end of the track, as opposed to the file
    /// not supporting seeking or a decode error.
    pub past_end: bool,
    source: SeekError,
}

impl SeekFailed {
    fn new(source: SeekError) -> Self {
        Self {
            past_end: is_past_end(&source),
            source,
        }
    }

    /// A failure with the given classification, for fakes in tests.
    #[cfg(test)]
    pub fn for_test(past_end: bool) -> Self {
        Self {
            past_end,
            source: SeekError::NotSupported {
                underlying_source: "test",
            },
        }
    }
}

impl std::fmt::Display for SeekFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("cannot seek in this file")
    }
}

impl std::error::Error for SeekFailed {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

/// Whether a seek failed because the target is past the end of the stream.
///
/// "Not supported" variants are typed in rodio and never count. Symphonia
/// reports the end either as an unexpected-EOF I/O error while scanning
/// forward (typed) or as `SeekErrorKind::OutOfRange`, which is not reachable
/// as a type without depending on symphonia directly, so it is recognised by
/// its message. Anything unrecognised counts as "not past the end", which
/// only costs an error message instead of skipping a track.
fn is_past_end(err: &SeekError) -> bool {
    use rodio::decoder::symphonia::SeekError as Symphonia;
    match err {
        SeekError::NotSupported { .. }
        | SeekError::SymphoniaDecoder(
            Symphonia::AccurateSeekNotSupported | Symphonia::RandomAccessNotSupported,
        ) => false,
        _ => {
            // `Other` is `#[error(transparent)]`: its `source()` skips the
            // wrapped error itself, so start the walk there.
            let start: &(dyn std::error::Error + 'static) = match err {
                SeekError::Other(inner) => inner.as_ref(),
                _ => err,
            };
            let mut cause = Some(start);
            while let Some(e) = cause {
                if e.downcast_ref::<std::io::Error>()
                    .is_some_and(|io| io.kind() == std::io::ErrorKind::UnexpectedEof)
                    || e.to_string().contains("out-of-range")
                {
                    return true;
                }
                cause = e.source();
            }
            false
        }
    }
}

/// The track currently handed to the output.
struct Loaded {
    sink: rodio::Player,
    flags: Arc<SourceFlags>,
    /// Position the source was started from; `sink.get_pos()` counts from here.
    start: Duration,
    duration: Option<Duration>,
}

/// Latest error reported by the output stream's callback.
type DeviceErrorSlot = Arc<Mutex<Option<String>>>;

/// Audio output through `rodio` on the default output device.
pub struct Player {
    // Field order matters: `loaded` (its sink) is dropped before the device.
    loaded: Option<Loaded>,
    device: MixerDeviceSink,
    volume: Volume,
    device_error: DeviceErrorSlot,
}

impl std::fmt::Debug for Player {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Player")
            .field("loaded", &self.loaded.is_some())
            .field("volume", &self.volume)
            .finish_non_exhaustive()
    }
}

impl Player {
    /// Opens the default audio output device, falling back to other
    /// configurations and other real devices if that fails.
    ///
    /// Stream errors are captured (see [`Audio::take_device_error`]) instead of
    /// being printed, so they cannot corrupt the terminal UI.
    ///
    /// # Errors
    ///
    /// Fails with [`NO_DEVICE`] when no output device can be opened.
    pub fn open() -> Result<Self> {
        let device_error = DeviceErrorSlot::default();
        let mut device = open_device(&device_error).context(NO_DEVICE)?;
        // Otherwise rodio prints a notice to stderr when the device is dropped.
        device.log_on_drop(false);
        Ok(Self {
            loaded: None,
            device,
            volume: Volume::default(),
            device_error,
        })
    }
}

/// Builds the stream error callback: keeps only the latest message, sanitised,
/// so a misbehaving device cannot grow memory or inject terminal sequences.
fn error_sink(slot: &DeviceErrorSlot) -> impl FnMut(CpalError) + Clone + Send + 'static {
    let slot = Arc::clone(slot);
    move |err: CpalError| {
        let msg = sanitize(&err.to_string()).unwrap_or_else(|| "unknown error".to_owned());
        *slot.lock().unwrap_or_else(PoisonError::into_inner) = Some(msg);
    }
}

/// Mirrors `DeviceSinkBuilder::open_default_sink`, but with our error callback
/// and without printing anything.
fn open_device(slot: &DeviceErrorSlot) -> Result<MixerDeviceSink> {
    let default = DeviceSinkBuilder::from_default_device()
        .map(|b| b.with_error_callback(error_sink(slot)))
        .and_then(|b| b.open_sink_or_fallback());
    let first_err = match default {
        Ok(sink) => return Ok(sink),
        Err(err) => err,
    };
    let Ok(devices) = rodio::cpal::default_host().output_devices() else {
        return Err(first_err.into());
    };
    devices
        .filter(|dev| {
            dev.description()
                .is_ok_and(|desc| desc.driver().is_some_and(|driver| driver != "null"))
        })
        .find_map(|dev| {
            DeviceSinkBuilder::from_device(dev)
                .map(|b| b.with_error_callback(error_sink(slot)))
                .and_then(|b| b.open_sink_or_fallback())
                .ok()
        })
        .ok_or_else(|| first_err.into())
}

/// Opens and decodes `path`, positioned at `start`. Decoder panics on the
/// calling thread are caught and reported as errors.
fn open_source(
    path: &Path,
    start: Duration,
    flags: &Arc<SourceFlags>,
) -> Result<Guarded<Decoder<BufReader<File>>>> {
    // Refuse anything but a regular file: opening a FIFO or device that has
    // replaced a library entry since the scan could block or never end.
    let meta = std::fs::metadata(path).context("cannot read file")?;
    if !meta.is_file() {
        bail!("not a regular file");
    }
    let file = File::open(path).context("cannot open file")?;
    let meta = file.metadata().context("cannot read file")?;
    if !meta.is_file() {
        bail!("not a regular file");
    }

    let mut builder = Decoder::builder()
        .with_data(BufReader::new(file))
        .with_byte_len(meta.len())
        .with_seekable(true);
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        builder = builder.with_hint(&ext.to_ascii_lowercase());
    }

    let decoder = quietly(AssertUnwindSafe(move || builder.build()))
        .map_err(|_| anyhow!(DecoderCrashed))?
        .context("not a playable audio file")?;
    let mut source = Guarded::new(decoder, Arc::clone(flags));
    if !start.is_zero() {
        source.try_seek(start).map_err(SeekFailed::new)?;
    }
    Ok(source)
}

impl Audio for Player {
    fn load(&mut self, path: &Path, start: Duration, paused: bool) -> Result<()> {
        let flags = Arc::new(SourceFlags::default());
        let source = open_source(path, start, &flags)?;
        let duration = source.total_duration();

        // The new track opened: stop the old one (dropping its sink stops its
        // source without blocking) before the new one starts.
        self.loaded = None;
        let sink = rodio::Player::connect_new(self.device.mixer());
        sink.set_volume(self.volume.gain());
        if paused {
            sink.pause();
        }
        sink.append(source);
        self.loaded = Some(Loaded {
            sink,
            flags,
            start,
            duration,
        });
        Ok(())
    }

    fn set_paused(&mut self, paused: bool) {
        if let Some(loaded) = &self.loaded {
            if paused {
                loaded.sink.pause();
            } else {
                loaded.sink.play();
            }
        }
    }

    fn stop(&mut self) {
        // Dropping the sink stops its source on the audio thread without blocking.
        self.loaded = None;
    }

    fn set_volume(&mut self, volume: Volume) {
        self.volume = volume;
        if let Some(loaded) = &self.loaded {
            loaded.sink.set_volume(volume.gain());
        }
    }

    fn position(&self) -> Duration {
        self.loaded.as_ref().map_or(Duration::ZERO, |l| {
            let pos = l.start.saturating_add(l.sink.get_pos());
            l.duration.map_or(pos, |d| pos.min(d))
        })
    }

    fn duration(&self) -> Option<Duration> {
        self.loaded.as_ref().and_then(|l| l.duration)
    }

    fn status(&self) -> Status {
        let Some(loaded) = &self.loaded else {
            return Status::Idle;
        };
        if loaded.flags.panicked.load(Ordering::Acquire) {
            return Status::Failed(DecoderCrashed.to_string());
        }
        if !loaded.sink.empty() {
            return Status::Active;
        }
        // A track started from its beginning that ends without a single sample
        // is undecodable; one resumed after a seek may legitimately be at its end.
        if loaded.start.is_zero() && !loaded.flags.produced.load(Ordering::Acquire) {
            Status::Failed("no audio could be decoded".to_owned())
        } else {
            Status::Ended
        }
    }

    fn take_device_error(&mut self) -> Option<String> {
        self.device_error
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }

    fn recent_samples(&self, out: &mut Vec<f32>) -> Option<Recent> {
        out.clear();
        let loaded = self.loaded.as_ref()?;
        // Reserve before locking, so the audio thread is never kept waiting
        // on an allocation.
        out.reserve(FFT_SIZE);
        let tap = loaded.flags.tap.lock().unwrap_or_else(PoisonError::into_inner);
        tap.copy_to(out)
    }
}

/// Where a seek by `step` (forward or back) from `pos` lands, clamped to the
/// track bounds. With an unknown `duration` only the start is a bound.
#[must_use]
pub fn seek_target(
    pos: Duration,
    step: Duration,
    forward: bool,
    duration: Option<Duration>,
) -> Duration {
    if forward {
        let target = pos.saturating_add(step);
        duration.map_or(target, |d| target.min(d))
    } else {
        pos.saturating_sub(step)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rodio::buffer::SamplesBuffer;
    use std::num::NonZero;

    #[test]
    fn volume_starts_at_full() {
        assert_eq!(Volume::default().percent(), 100);
        assert!((Volume::default().gain() - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn volume_steps_by_five_and_clamps() {
        let v = Volume::default();
        assert_eq!(v.up(), Volume::MAX, "stays at 100%");
        assert_eq!(v.down().percent(), 95);

        let mut v = Volume::MAX;
        for _ in 0..25 {
            v = v.down();
        }
        assert_eq!(v.percent(), 0, "stays at 0%");
        assert_eq!(v.gain(), 0.0);
        assert_eq!(v.up().percent(), 5);
        assert_eq!(Volume::new(250), Volume::MAX);
        assert_eq!(Volume::new(98).up(), Volume::MAX);
        assert_eq!(Volume::new(3).down().percent(), 0);
    }

    #[test]
    fn seek_is_clamped_to_track_bounds() {
        let s = Duration::from_secs;
        assert_eq!(seek_target(s(60), SEEK_STEP, true, Some(s(200))), s(65));
        assert_eq!(seek_target(s(3), SEEK_STEP, false, Some(s(200))), s(0));
        assert_eq!(seek_target(s(198), SEEK_STEP, true, Some(s(200))), s(200));
        assert_eq!(seek_target(s(198), SEEK_STEP, true, None), s(203));
        assert_eq!(seek_target(s(65), SEEK_STEP, false, None), s(60));
    }

    /// A source that yields `n` samples, then panics if `panic_after` is set.
    struct Scripted {
        left: usize,
        panic_after: bool,
    }

    impl Iterator for Scripted {
        type Item = Sample;
        fn next(&mut self) -> Option<Sample> {
            if self.left == 0 {
                assert!(!self.panic_after, "decoder bug");
                return None;
            }
            self.left -= 1;
            Some(0.0)
        }
    }

    impl Source for Scripted {
        fn current_span_len(&self) -> Option<usize> {
            None
        }
        fn channels(&self) -> ChannelCount {
            NonZero::<u16>::MIN
        }
        fn sample_rate(&self) -> SampleRate {
            NonZero::<u32>::MIN
        }
        fn total_duration(&self) -> Option<Duration> {
            None
        }
    }

    #[test]
    fn guarded_source_passes_samples_and_records_output() {
        let flags = Arc::new(SourceFlags::default());
        let mut src = Guarded::new(
            Scripted {
                left: 3,
                panic_after: false,
            },
            Arc::clone(&flags),
        );
        assert!(!flags.produced.load(Ordering::Acquire));
        assert_eq!(src.by_ref().count(), 3);
        assert!(flags.produced.load(Ordering::Acquire));
        assert!(!flags.panicked.load(Ordering::Acquire));
    }

    #[test]
    fn guarded_source_turns_panic_into_end() {
        let flags = Arc::new(SourceFlags::default());
        let mut src = Guarded::new(
            Scripted {
                left: 2,
                panic_after: true,
            },
            Arc::clone(&flags),
        );
        assert_eq!(src.by_ref().count(), 2);
        assert!(flags.panicked.load(Ordering::Acquire));
        assert_eq!(src.next(), None, "stays exhausted");
        assert_eq!(src.current_span_len(), Some(0));
        assert_eq!(src.channels(), NonZero::<u16>::MIN);
        assert!(src.try_seek(Duration::ZERO).is_err());
    }

    /// One span of a [`Spans`] source.
    struct Span {
        channels: u16,
        rate: u32,
        /// What `current_span_len` reports while in this span.
        reported: Option<usize>,
        data: Vec<Sample>,
    }

    impl Span {
        fn new(channels: u16, rate: u32, data: Vec<Sample>) -> Self {
            Self {
                channels,
                rate,
                reported: Some(data.len()),
                data,
            }
        }
    }

    /// A decoder-like source made of spans. Like rodio's Symphonia decoder,
    /// its format getters describe the span of the sample `next()` returned
    /// last (the first span before any). Counts format reads.
    struct Spans {
        parts: Vec<Span>,
        span: usize,
        pos: usize,
        reads: std::rc::Rc<std::cell::Cell<usize>>,
    }

    impl Spans {
        fn new(parts: Vec<Span>) -> Self {
            Self {
                parts,
                span: 0,
                pos: 0,
                reads: std::rc::Rc::default(),
            }
        }

        fn current(&self) -> Option<&Span> {
            self.parts.get(self.span)
        }
    }

    impl Iterator for Spans {
        type Item = Sample;
        fn next(&mut self) -> Option<Sample> {
            loop {
                let span = self.parts.get(self.span)?;
                if let Some(&sample) = span.data.get(self.pos) {
                    self.pos += 1;
                    return Some(sample);
                }
                if self.span + 1 >= self.parts.len() {
                    return None;
                }
                self.span += 1;
                self.pos = 0;
            }
        }
    }

    impl Source for Spans {
        fn current_span_len(&self) -> Option<usize> {
            self.reads.set(self.reads.get() + 1);
            self.current().and_then(|s| s.reported)
        }
        fn channels(&self) -> ChannelCount {
            self.current()
                .and_then(|s| NonZero::new(s.channels))
                .unwrap_or(NonZero::<u16>::MIN)
        }
        fn sample_rate(&self) -> SampleRate {
            self.current()
                .and_then(|s| NonZero::new(s.rate))
                .unwrap_or(NonZero::<u32>::MIN)
        }
        fn total_duration(&self) -> Option<Duration> {
            None
        }
        /// Supports only seeking back to the start.
        fn try_seek(&mut self, pos: Duration) -> Result<(), SeekError> {
            if !pos.is_zero() {
                return Err(SeekError::NotSupported {
                    underlying_source: "Spans",
                });
            }
            self.span = 0;
            self.pos = 0;
            Ok(())
        }
    }

    fn rate(hz: u32) -> SampleRate {
        NonZero::new(hz).unwrap_or(NonZero::<u32>::MIN)
    }

    fn channels(n: u16) -> ChannelCount {
        NonZero::new(n).unwrap_or(NonZero::<u16>::MIN)
    }

    /// The tap's samples and what `copy_to` reports.
    fn tap_of(flags: &SourceFlags) -> (Vec<f32>, Option<Recent>) {
        let mut out = vec![9.0; 3];
        let tap = flags.tap.lock().unwrap_or_else(PoisonError::into_inner);
        let recent = tap.copy_to(&mut out);
        (out, recent)
    }

    /// `frames` interleaved frames, each a copy of `frame`.
    fn frames(frame: &[Sample], frames: usize) -> Vec<Sample> {
        frame.repeat(frames)
    }

    #[expect(
        clippy::cast_precision_loss,
        reason = "test indices are far below 2^24"
    )]
    fn ramp(n: usize) -> Vec<Sample> {
        (0..n).map(|i| i as Sample).collect()
    }

    #[test]
    fn tap_mixes_stereo_to_mono() {
        let flags = Arc::new(SourceFlags::default());
        let buffer = SamplesBuffer::new(channels(2), rate(44_100), frames(&[1.0, 0.0], 1024));
        let src = Guarded::new(buffer, Arc::clone(&flags));
        assert_eq!(src.count(), 2048);
        let (samples, recent) = tap_of(&flags);
        assert_eq!(samples, vec![0.5; 1024]);
        assert_eq!(
            recent,
            Some(Recent {
                sample_rate: 44_100,
                written: 1024
            })
        );
    }

    #[test]
    fn tap_keeps_the_newest_samples_in_order() {
        let flags = Arc::new(SourceFlags::default());
        let n = FFT_SIZE + 3 * TAP_BATCH;
        let data = ramp(n);
        let buffer = SamplesBuffer::new(channels(1), rate(48_000), data.clone());
        let src = Guarded::new(buffer, Arc::clone(&flags));
        assert_eq!(src.count(), n);
        let (samples, recent) = tap_of(&flags);
        assert_eq!(samples, data.get(n - FFT_SIZE..).unwrap_or_default());
        assert_eq!(recent.map(|r| r.sample_rate), Some(48_000));
        assert_eq!(recent.map(|r| r.written), Some(n as u64));
    }

    #[test]
    fn tap_is_empty_until_a_batch_is_handed_over() {
        let flags = Arc::new(SourceFlags::default());
        assert_eq!(tap_of(&flags), (Vec::new(), None), "a new load's tap is empty");

        let buffer = SamplesBuffer::new(channels(1), rate(8_000), vec![0.25; TAP_BATCH - 1]);
        let src = Guarded::new(buffer, Arc::clone(&flags));
        assert_eq!(src.count(), TAP_BATCH - 1);
        assert_eq!(tap_of(&flags), (Vec::new(), None), "a partial batch is not flushed");
    }

    #[test]
    fn tap_never_waits_for_a_held_lock() {
        let flags = Arc::new(SourceFlags::default());
        let buffer = SamplesBuffer::new(channels(1), rate(44_100), vec![0.5; 4 * TAP_BATCH]);
        let mut src = Guarded::new(buffer, Arc::clone(&flags));
        {
            let _held = flags.tap.lock().unwrap_or_else(PoisonError::into_inner);
            // `try_lock` on the thread holding the lock fails instead of blocking.
            assert_eq!(src.by_ref().take(2 * TAP_BATCH).count(), 2 * TAP_BATCH);
        }
        assert_eq!(tap_of(&flags), (Vec::new(), None), "both batches were dropped");
        assert_eq!(src.count(), 2 * TAP_BATCH);
        let (samples, recent) = tap_of(&flags);
        assert_eq!(samples.len(), 2 * TAP_BATCH);
        assert_eq!(recent.map(|r| r.written), Some(2 * TAP_BATCH as u64));
    }

    #[test]
    fn tap_recovers_from_a_poisoned_lock() {
        let flags = Arc::new(SourceFlags::default());
        let poisoner = Arc::clone(&flags);
        let _panicked = std::thread::spawn(move || {
            let _held = poisoner.tap.lock();
            // Unwinds without running the panic hook, so nothing is printed.
            std::panic::resume_unwind(Box::new("poison the tap"));
        })
        .join();
        assert!(flags.tap.is_poisoned());
        let buffer = SamplesBuffer::new(channels(1), rate(44_100), vec![0.5; TAP_BATCH]);
        assert_eq!(Guarded::new(buffer, Arc::clone(&flags)).count(), TAP_BATCH);
        assert_eq!(tap_of(&flags).0, vec![0.5; TAP_BATCH]);
    }

    #[test]
    fn tap_is_taken_before_the_volume() {
        let flags = Arc::new(SourceFlags::default());
        let data: Vec<Sample> = ramp(TAP_BATCH).iter().map(|x| x / 1000.0).collect();
        let buffer = SamplesBuffer::new(channels(1), rate(44_100), data.clone());
        // rodio applies the volume in an amplify stage downstream of `Guarded`.
        let played: Vec<Sample> = Guarded::new(buffer, Arc::clone(&flags))
            .amplify(0.2)
            .collect();
        let expected: Vec<Sample> = data.iter().map(|x| x * 0.2).collect();
        assert_eq!(played, expected);
        assert_eq!(tap_of(&flags).0, data, "the tap ignores the volume");
    }

    #[test]
    fn tap_mixes_each_span_with_its_own_channel_count() {
        let flags = Arc::new(SourceFlags::default());
        let src = Spans::new(vec![
            Span::new(2, 44_100, frames(&[1.0, 0.0], 1024)),
            Span::new(1, 44_100, vec![0.25; 512]),
            Span::new(3, 44_100, frames(&[0.0, 0.0, 0.3], 512)),
        ]);
        assert_eq!(Guarded::new(src, Arc::clone(&flags)).count(), 2048 + 512 + 1536);
        let (samples, recent) = tap_of(&flags);
        let mut expected = vec![0.5; 1024];
        expected.extend([0.25; 512]);
        expected.extend([0.1; 512]);
        assert_eq!(samples.len(), expected.len());
        for (got, want) in samples.iter().zip(&expected) {
            assert!((got - want).abs() < 1e-6, "{got} != {want}");
        }
        assert_eq!(recent.map(|r| r.written), Some(2048));
    }

    #[test]
    fn tap_clears_on_a_sample_rate_change() {
        let flags = Arc::new(SourceFlags::default());
        let src = Spans::new(vec![
            // 512 samples are handed over; the other 488 are pending at the change.
            Span::new(1, 44_100, vec![1.0; 1000]),
            Span::new(1, 48_000, vec![0.5; 512]),
        ]);
        let mut src = Guarded::new(src, Arc::clone(&flags));
        assert_eq!(src.by_ref().take(1000).count(), 1000);
        assert_eq!(tap_of(&flags).0, vec![1.0; 512]);
        assert_eq!(src.count(), 512);
        let (samples, recent) = tap_of(&flags);
        assert_eq!(samples, vec![0.5; 512], "no 44.1 kHz sample is kept");
        assert_eq!(
            recent,
            Some(Recent {
                sample_rate: 48_000,
                written: 1024
            })
        );
    }

    #[test]
    fn tap_keeps_frames_aligned_when_spans_are_odd_or_unknown() {
        for reported in [None, Some(0), Some(5)] {
            // Three channels whose mean is 0.5: a frame read one sample off
            // would mix to 0.333 or 0.167 instead.
            let mut span = Span::new(3, 44_100, frames(&[0.0, 0.5, 1.0], 2048));
            span.reported = reported;
            let src = Spans::new(vec![span]);
            let reads = std::rc::Rc::clone(&src.reads);
            let flags = Arc::new(SourceFlags::default());
            assert_eq!(Guarded::new(src, Arc::clone(&flags)).count(), 3 * 2048);
            let (samples, recent) = tap_of(&flags);
            assert_eq!(samples, vec![0.5; 2048], "{reported:?}");
            assert_eq!(recent.map(|r| r.written), Some(2048), "{reported:?}");
            // A re-read needs the span to run out and a frame boundary.
            let max_reads = match reported {
                Some(5) => 3 * 2048 / 5 + 1,
                _ => 3 * 2048 / UNKNOWN_SPAN + 1,
            };
            assert!(reads.get() <= max_reads, "{reported:?}: {} reads", reads.get());
        }
    }

    #[test]
    fn tap_reads_a_new_format_only_on_a_frame_boundary() {
        // The first span claims 4 samples of 3 channels, so it ends one sample
        // into a frame, and the 2-channel span starts there. The frame is
        // completed from the new span's samples before the format is re-read;
        // re-reading mid-frame would drop that frame and mix (0.9 + 0.0) / 2.
        let mut first = Span::new(3, 44_100, vec![0.0, 0.5, 1.0, 0.3]);
        first.reported = Some(4);
        let mut second = vec![0.9, 0.0];
        second.extend(frames(&[1.0, 0.0], 511));
        let src = Spans::new(vec![first, Span::new(2, 44_100, second)]);
        let flags = Arc::new(SourceFlags::default());
        assert_eq!(Guarded::new(src, Arc::clone(&flags)).count(), 4 + 1024);
        let (samples, _) = tap_of(&flags);
        assert_eq!(samples.len(), TAP_BATCH);
        let head = samples.get(..3).unwrap_or_default();
        let want = [0.5, (0.3 + 0.9 + 0.0) / 3.0, 0.5];
        for (got, want) in head.iter().zip(want) {
            assert!((got - want).abs() < 1e-6, "{head:?}");
        }
        assert!(samples.iter().skip(2).all(|&x| (x - 0.5).abs() < 1e-6));
    }

    #[test]
    fn tap_counts_every_flushed_batch() {
        let flags = Arc::new(SourceFlags::default());
        let buffer = SamplesBuffer::new(channels(1), rate(44_100), vec![0.0; 3 * TAP_BATCH + 100]);
        let mut src = Guarded::new(buffer, Arc::clone(&flags));
        for batch in 1..=3_u64 {
            assert_eq!(src.by_ref().take(TAP_BATCH).count(), TAP_BATCH);
            let written = tap_of(&flags).1.map(|r| r.written);
            assert_eq!(written, Some(batch * TAP_BATCH as u64));
        }
        assert_eq!(src.count(), 100);
        assert_eq!(tap_of(&flags).1.map(|r| r.written), Some(3 * TAP_BATCH as u64));
    }

    #[test]
    fn seeking_restarts_the_tap_frame() {
        let flags = Arc::new(SourceFlags::default());
        let spans = Spans::new(vec![Span::new(2, 44_100, frames(&[1.0, 0.0], 2048))]);
        let mut src = Guarded::new(spans, Arc::clone(&flags));
        // Stop in the middle of a frame and of a batch, then seek.
        assert_eq!(src.by_ref().take(301).count(), 301);
        assert!(src.try_seek(Duration::ZERO).is_ok());
        assert_eq!(src.by_ref().take(2 * TAP_BATCH).count(), 2 * TAP_BATCH);
        assert_eq!(tap_of(&flags).0, vec![0.5; TAP_BATCH]);
    }

    #[test]
    fn default_recent_samples_reports_nothing() {
        let audio = crate::test_support::SilentAudio::default();
        let mut out = vec![1.0; 4];
        assert_eq!(audio.recent_samples(&mut out), None);
        assert_eq!(out, Vec::<f32>::new());
    }

    #[test]
    fn open_source_rejects_bad_files() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let flags = Arc::new(SourceFlags::default());

        let corrupt = tmp.path().join("corrupt.mp3");
        std::fs::write(&corrupt, b"this is not an mp3 file at all")?;
        let err = open_source(&corrupt, Duration::ZERO, &flags).err();
        assert!(err.is_some(), "corrupt file must not open");

        let missing = tmp.path().join("missing.mp3");
        assert!(open_source(&missing, Duration::ZERO, &flags).is_err());

        let dir = tmp.path().join("dir.mp3");
        std::fs::create_dir(&dir)?;
        let msg = open_source(&dir, Duration::ZERO, &flags)
            .err()
            .map(|e| format!("{e:#}"));
        assert_eq!(msg.as_deref(), Some("not a regular file"));
        Ok(())
    }

    #[test]
    fn open_source_decodes_and_seeks_valid_audio() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let song = tmp.path().join("song.WAV");
        std::fs::write(&song, crate::test_support::wav(16_000))?; // 2 s

        let flags = Arc::new(SourceFlags::default());
        let src = open_source(&song, Duration::ZERO, &flags)?;
        assert_eq!(src.total_duration(), Some(Duration::from_secs(2)));
        assert_eq!(src.count(), 16_000);
        assert!(flags.produced.load(Ordering::Acquire));

        let src = open_source(&song, Duration::from_secs(1), &flags)?;
        let left = src.count();
        assert!((7_000..=9_000).contains(&left), "{left} samples after seek");
        Ok(())
    }

    #[test]
    fn seek_failures_are_classified() {
        use rodio::decoder::symphonia::SeekError as Symphonia;
        let other = |e: std::io::Error| SeekError::Other(Arc::new(e));
        let eof = std::io::Error::from(std::io::ErrorKind::UnexpectedEof);
        assert!(is_past_end(&other(eof)));
        let range = std::io::Error::other(
            "seek error: requested seek timestamp is out-of-range for stream",
        );
        assert!(is_past_end(&other(range)));
        let denied = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
        assert!(!is_past_end(&other(denied)));
        assert!(!is_past_end(&SeekError::NotSupported {
            underlying_source: "x"
        }));
        assert!(!is_past_end(&SeekError::SymphoniaDecoder(
            Symphonia::RandomAccessNotSupported
        )));
        assert!(!is_past_end(&SeekError::Other(Arc::new(DecoderCrashed))));
    }

    #[test]
    fn seeking_past_the_end_of_a_real_file() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let song = tmp.path().join("song.wav");
        std::fs::write(&song, crate::test_support::wav(16_000))?; // 2 s
        let flags = Arc::new(SourceFlags::default());
        match open_source(&song, Duration::from_secs(10), &flags) {
            // Saturating at the end is fine: the track simply ends.
            Ok(src) => assert_eq!(src.count(), 0),
            Err(err) => {
                let failure = err.downcast_ref::<SeekFailed>();
                assert!(failure.is_some_and(|f| f.past_end), "{err:#}");
            }
        }
        Ok(())
    }

    /// Plays through the real output device. Needs sound hardware, so it is
    /// ignored by default: `cargo test -- --ignored --nocapture real_device`.
    #[test]
    #[ignore = "needs an audio output device"]
    fn real_device_plays_and_reports_bad_files() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let good = tmp.path().join("good.wav");
        std::fs::write(&good, crate::test_support::wav(4_000))?; // 0.5 s
        let corrupt = tmp.path().join("corrupt.mp3");
        std::fs::write(&corrupt, [0xFF_u8; 4096])?;

        let mut player = Player::open()?;
        assert_eq!(player.status(), Status::Idle);
        assert!(player.load(&corrupt, Duration::ZERO, false).is_err());
        assert_eq!(player.status(), Status::Idle);

        player.load(&good, Duration::ZERO, false)?;
        assert_eq!(player.status(), Status::Active);
        assert_eq!(player.duration(), Some(Duration::from_millis(500)));
        std::thread::sleep(Duration::from_millis(200));
        player.set_paused(true);
        // The pause is applied by the audio thread within a few milliseconds.
        std::thread::sleep(Duration::from_millis(50));
        let paused_at = player.position();
        assert!(!paused_at.is_zero());
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(player.position(), paused_at, "position holds while paused");
        player.set_paused(false);
        std::thread::sleep(Duration::from_millis(1_000));
        assert_eq!(player.status(), Status::Ended);
        player.stop();
        assert_eq!(player.status(), Status::Idle);
        assert_eq!(player.take_device_error(), None);

        // A corrupt file is reported and skipped, even with repeat one.
        let tracks = [&corrupt, &good]
            .map(|p| crate::library::read_track((*p).clone()))
            .to_vec();
        let mut playback = crate::playback::Playback::new(tracks, player);
        while playback.repeat() != crate::queue::Repeat::One {
            playback.cycle_repeat();
        }
        playback.play(0);
        assert_eq!(playback.current(), Some(1));
        assert_eq!(playback.state(), crate::playback::PlayState::Playing);
        let notice = playback.take_notice();
        assert!(
            notice
                .as_ref()
                .is_some_and(|n| n.text.contains("corrupt.mp3")),
            "{notice:?}"
        );
        Ok(())
    }

    /// The tap through the real output device: filled while playing, and
    /// empty again after a new load. Needs sound hardware:
    /// `cargo test -- --ignored real_device_tap`.
    #[test]
    #[ignore = "needs an audio output device"]
    fn real_device_tap_follows_loads() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let song = tmp.path().join("song.wav");
        std::fs::write(&song, crate::test_support::wav(16_000))?; // 2 s
        let mut player = Player::open()?;
        let mut out = Vec::new();
        assert_eq!(player.recent_samples(&mut out), None, "nothing loaded");

        player.load(&song, Duration::ZERO, false)?;
        std::thread::sleep(Duration::from_millis(500));
        let recent = player.recent_samples(&mut out);
        assert_eq!(recent.map(|r| r.sample_rate), Some(8_000));
        assert!(recent.is_some_and(|r| r.written > 0));
        assert!(!out.is_empty() && out.len() <= FFT_SIZE);

        // A paused source is not pulled, so the new tap stays empty.
        player.load(&song, Duration::from_secs(1), true)?;
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(player.recent_samples(&mut out), None, "fresh tap per load");
        assert_eq!(out, Vec::<f32>::new());
        player.stop();
        assert_eq!(player.recent_samples(&mut out), None);
        Ok(())
    }

    /// Plays a real file through `Playback`: `KASSET_SAMPLE=/path/song.mp3
    /// cargo test -- --ignored real_sample`. Does nothing when the variable is unset.
    #[test]
    #[ignore = "needs an audio output device and a sample file"]
    fn real_sample_plays_seeks_and_changes_volume() -> Result<()> {
        use crate::playback::{PlayState, Playback};
        let Some(sample) = std::env::var_os("KASSET_SAMPLE") else {
            return Ok(());
        };
        let track = crate::library::read_track(sample.into());
        let mut playback = Playback::new(vec![track], Player::open()?);
        playback.play(0);
        assert_eq!(playback.state(), PlayState::Playing);
        assert_eq!(playback.take_notice(), None);
        std::thread::sleep(Duration::from_millis(1_500));
        playback.tick();
        let before = playback.position();
        assert!(before >= Duration::from_millis(1_000), "{before:?}");

        playback.seek(true);
        let after = playback.position();
        assert_eq!(playback.take_notice(), None);
        assert!(
            after >= before + Duration::from_secs(4),
            "{before:?} -> {after:?}"
        );
        playback.volume_down();
        std::thread::sleep(Duration::from_millis(1_000));
        playback.tick();
        assert_eq!(playback.state(), PlayState::Playing);
        assert_eq!(playback.volume().percent(), 95);
        Ok(())
    }
}
