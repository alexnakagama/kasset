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
