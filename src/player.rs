//! Audio output: decoding files and sending them to the default output device.
//!
//! [`Audio`] is the small interface the rest of the program drives; [`Player`]
//! implements it with `rodio`, and tests implement it with a fake so playback
//! logic can be checked without sound hardware.

use std::fs::File;
use std::io::BufReader;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use rodio::cpal::traits::HostTrait;
use rodio::source::SeekError;
use rodio::{ChannelCount, CpalError, Decoder, DeviceSinkBuilder, DeviceTrait, MixerDeviceSink};
use rodio::{Sample, SampleRate, Source};

use crate::text::sanitize;

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
}

/// Flags shared between a playing source (audio thread) and the [`Player`].
#[derive(Debug, Default)]
struct SourceFlags {
    /// At least one sample was decoded.
    produced: AtomicBool,
    /// The decoder panicked; the source ended early.
    panicked: AtomicBool,
}

/// A source wrapper that turns a decoder panic into the end of the track.
///
/// Decoders parse untrusted files on the audio thread; a panic there would kill
/// the output stream for the rest of the session. Instead the panic is caught,
/// recorded in [`SourceFlags::panicked`] and the source reports exhaustion.
struct Guarded<S> {
    inner: Option<S>,
    flags: Arc<SourceFlags>,
    produced: bool,
    channels: ChannelCount,
    sample_rate: SampleRate,
}

impl<S: Source> Guarded<S> {
    fn new(inner: S, flags: Arc<SourceFlags>) -> Self {
        Self {
            channels: inner.channels(),
            sample_rate: inner.sample_rate(),
            inner: Some(inner),
            flags,
            produced: false,
        }
    }

    fn poison(&mut self) {
        self.flags.panicked.store(true, Ordering::Release);
        // Dropping a decoder in an inconsistent state may panic again.
        let inner = self.inner.take();
        let _ignored = catch_unwind(AssertUnwindSafe(move || drop(inner)));
    }
}

impl<S: Source> Iterator for Guarded<S> {
    type Item = Sample;

    fn next(&mut self) -> Option<Sample> {
        let inner = self.inner.as_mut()?;
        // The decoder is dropped on panic and never touched again, so no broken
        // invariant can be observed. Requires `panic = "unwind"` (Cargo.toml).
        match catch_unwind(AssertUnwindSafe(|| inner.next())) {
            Ok(Some(sample)) => {
                if !self.produced {
                    self.produced = true;
                    self.flags.produced.store(true, Ordering::Release);
                }
                Some(sample)
            }
            Ok(None) => None,
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
        let Some(inner) = self.inner.as_mut() else {
            return Err(SeekError::Other(Arc::new(DecoderCrashed)));
        };
        catch_unwind(AssertUnwindSafe(|| inner.try_seek(pos))).unwrap_or_else(|_| {
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

    let decoder = catch_unwind(AssertUnwindSafe(move || builder.build()))
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
