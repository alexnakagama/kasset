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

/// A playlist store over a library root that does not exist: listing finds
/// nothing and every write fails. For tests that do not involve playlists.
pub fn no_store() -> Box<dyn crate::playlist::PlaylistStore> {
    Box::new(crate::playlist::FsStore::new(std::path::Path::new(
        "/nonexistent/kasset-test-library",
    )))
}
