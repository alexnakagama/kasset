//! Fixtures shared by unit tests.

/// A valid PCM WAV file (mono, 8 kHz, 16-bit) holding `samples` samples of silence.
pub fn wav(samples: u32) -> Vec<u8> {
    let data_len = samples * 2;
    let mut v = Vec::new();
    v.extend_from_slice(b"RIFF");
    v.extend_from_slice(&(36 + data_len).to_le_bytes());
    v.extend_from_slice(b"WAVEfmt ");
    v.extend_from_slice(&16u32.to_le_bytes());
    v.extend_from_slice(&1u16.to_le_bytes()); // PCM
    v.extend_from_slice(&1u16.to_le_bytes()); // mono
    v.extend_from_slice(&8000u32.to_le_bytes());
    v.extend_from_slice(&16000u32.to_le_bytes()); // byte rate
    v.extend_from_slice(&2u16.to_le_bytes()); // block align
    v.extend_from_slice(&16u16.to_le_bytes());
    v.extend_from_slice(b"data");
    v.extend_from_slice(&data_len.to_le_bytes());
    v.resize(v.len() + data_len as usize, 0);
    v
}

/// A tiny valid PCM WAV file (0.1 s of silence).
pub fn wav_bytes() -> Vec<u8> {
    wav(800)
}

/// A library track with the given text fields and a 3:20 length.
pub fn track(title: &str, artist: &str, album: &str) -> crate::library::Track {
    crate::library::Track {
        path: std::path::PathBuf::from(format!("/music/{artist}/{album}/{title}.mp3")),
        title: title.to_owned(),
        artist: artist.to_owned(),
        album: album.to_owned(),
        number: None,
        duration: Some(std::time::Duration::from_secs(200)),
    }
}

/// An audio output that plays nothing: loads succeed (unless `broken`) and
/// the position is whatever the test sets. Setting `ended` makes the loaded
/// track end (until the next load).
#[derive(Debug, Default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent switches each test sets directly"
)]
pub struct SilentAudio {
    pub loaded: bool,
    pub broken: bool,
    pub paused: bool,
    pub position: std::time::Duration,
    pub ended: bool,
}

impl crate::player::Audio for SilentAudio {
    fn load(
        &mut self,
        _path: &std::path::Path,
        start: std::time::Duration,
        paused: bool,
    ) -> anyhow::Result<()> {
        if self.broken {
            anyhow::bail!("not a playable audio file");
        }
        self.loaded = true;
        self.ended = false;
        self.paused = paused;
        self.position = start;
        Ok(())
    }
    fn set_paused(&mut self, paused: bool) {
        self.paused = paused;
    }
    fn stop(&mut self) {
        self.loaded = false;
        self.position = std::time::Duration::ZERO;
    }
    fn set_volume(&mut self, _volume: crate::player::Volume) {}
    fn position(&self) -> std::time::Duration {
        self.position
    }
    fn duration(&self) -> Option<std::time::Duration> {
        None
    }
    fn status(&self) -> crate::player::Status {
        if self.loaded && self.ended {
            crate::player::Status::Ended
        } else if self.loaded {
            crate::player::Status::Active
        } else {
            crate::player::Status::Idle
        }
    }
    fn take_device_error(&mut self) -> Option<String> {
        None
    }
}

/// An audio output that behaves like [`SilentAudio`] and, while a track is
/// loaded, reports a sine wave of `frequency` Hz and `amplitude` (1.0 is full
/// scale) as its recent samples. Each call to `recent_samples` while the
/// track plays advances `written` by `step` samples ([`ToneAudio::STEP`] by
/// default), unless
/// `advancing` is false (a stalled decoder or device). Each load starts a
/// new generation with `written` back at 0, like [`crate::player::Player`].
#[derive(Debug)]
pub struct ToneAudio {
    pub silent: SilentAudio,
    pub frequency: f32,
    pub amplitude: f32,
    pub sample_rate: u32,
    pub advancing: bool,
    /// Samples `written` advances by per call while playing.
    pub step: u64,
    written: std::cell::Cell<u64>,
    loads: u64,
}

impl ToneAudio {
    /// The default `step`.
    pub const STEP: u64 = 1024;

    /// A tone at `frequency` Hz and `amplitude`, sampled at 44.1 kHz.
    pub fn new(frequency: f32, amplitude: f32) -> Self {
        Self {
            silent: SilentAudio::default(),
            frequency,
            amplitude,
            sample_rate: 44_100,
            advancing: true,
            step: Self::STEP,
            written: std::cell::Cell::new(0),
            loads: 0,
        }
    }
}

impl crate::player::Audio for ToneAudio {
    fn load(
        &mut self,
        path: &std::path::Path,
        start: std::time::Duration,
        paused: bool,
    ) -> anyhow::Result<()> {
        self.silent.load(path, start, paused)?;
        // A new load starts with an empty tap.
        self.written.set(0);
        self.loads += 1;
        Ok(())
    }
    fn set_paused(&mut self, paused: bool) {
        self.silent.set_paused(paused);
    }
    fn stop(&mut self) {
        self.silent.stop();
        self.written.set(0);
    }
    fn set_volume(&mut self, volume: crate::player::Volume) {
        self.silent.set_volume(volume);
    }
    fn position(&self) -> std::time::Duration {
        self.silent.position()
    }
    fn duration(&self) -> Option<std::time::Duration> {
        self.silent.duration()
    }
    fn status(&self) -> crate::player::Status {
        self.silent.status()
    }
    fn take_device_error(&mut self) -> Option<String> {
        self.silent.take_device_error()
    }
    #[expect(
        clippy::cast_precision_loss,
        reason = "sample indices and rates in tests are far below 2^24"
    )]
    fn recent_samples(&self, out: &mut Vec<f32>) -> Option<crate::player::Recent> {
        out.clear();
        if !self.silent.loaded {
            return None;
        }
        let playing = !self.silent.paused && !self.silent.ended;
        if playing && self.advancing {
            self.written.set(self.written.get() + self.step);
        }
        let written = self.written.get();
        if written == 0 {
            return None;
        }
        let len = written.min(crate::visualizer::FFT_SIZE as u64);
        let first = written - len;
        let rate = self.sample_rate as f32;
        out.extend((first..written).map(|i| {
            // Reduce the phase per sample so it stays precise as `i` grows.
            let cycles = (i as f32 * self.frequency / rate).fract();
            self.amplitude * (std::f32::consts::TAU * cycles).sin()
        }));
        Some(crate::player::Recent {
            sample_rate: self.sample_rate,
            written,
            generation: self.loads,
        })
    }
}

/// A playlist store over a library root that does not exist: listing finds
/// nothing and every write fails. For tests that do not involve playlists.
pub fn no_store() -> Box<dyn crate::playlist::PlaylistStore> {
    Box::new(crate::playlist::FsStore::new(std::path::Path::new(
        "/nonexistent/kasset-test-library",
    )))
}

/// Creates a named pipe at `path` with the `mkfifo` tool. Returns `false` if
/// the tool is unavailable, so the calling test can skip itself.
#[cfg(unix)]
pub fn make_fifo(path: &std::path::Path) -> bool {
    std::process::Command::new("mkfifo")
        .arg(path)
        .status()
        .is_ok_and(|s| s.success())
}

/// A minimal FLAC file: the `fLaC` marker and a lone STREAMINFO block (mono,
/// 44.1 kHz, 16-bit, no audio frames). Enough for lofty to read and write
/// Vorbis comments.
pub fn flac_bytes() -> Vec<u8> {
    let mut v = b"fLaC".to_vec();
    // Metadata block header: last block, type 0 (STREAMINFO), 34 bytes.
    v.extend_from_slice(&[0x80, 0x00, 0x00, 34]);
    v.extend_from_slice(&4096u16.to_be_bytes()); // min block size
    v.extend_from_slice(&4096u16.to_be_bytes()); // max block size
    v.extend_from_slice(&[0, 0, 0, 0, 0, 0]); // min / max frame size: unknown
    // 20 bits sample rate (44 100), 3 bits channels - 1 (0), 5 bits bits per
    // sample - 1 (15), 36 bits total samples (0).
    let packed: u64 = (44_100 << 44) | (15 << 36);
    v.extend_from_slice(&packed.to_be_bytes());
    v.extend_from_slice(&[0; 16]); // MD5 of the audio: unknown
    v
}

/// A minimal MP3 file: a few silent MPEG-1 Layer III frames (128 kbit/s,
/// 44.1 kHz, mono), enough for lofty to read and write `ID3v2` and APE tags.
pub fn mp3_bytes() -> Vec<u8> {
    // Frame length = 144 * 128 000 / 44 100 = 417 bytes (no padding).
    let mut frame = vec![0xFF, 0xFB, 0x90, 0xC4];
    frame.resize(417, 0);
    frame.repeat(4)
}
