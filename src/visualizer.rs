//! Spectrum analysis for the visualizer: turns the newest decoded samples into
//! one bar level per frequency band.
//!
//! Pure logic with no audio or terminal dependencies. [`Spectrum::update`] takes
//! the samples, their sample rate, the bar count and the current time, so the
//! result is deterministic given its inputs (tests inject `now`).
//!
//! Each update removes the DC offset from the newest [`FFT_SIZE`] mono
//! samples, applies a Hann window, runs a forward FFT, and splits 40 Hz to
//! `min(16 kHz, rate / 2)` into logarithmically spaced bands. A band's value
//! is the peak bin magnitude it covers, scaled so a full-scale sine reads
//! about 0 dB; a band narrower than one bin uses the bin containing its centre
//! frequency. Levels map −60 dB to 0 dB onto 0.0 to 1.0.
//!
//! Invariants:
//! - every level is finite and in `0.0..=1.0`, whatever the input (non-finite
//!   samples count as silence, and samples are clamped to full scale);
//! - bars rise to their target at once and fall linearly, from 1.0 to 0.0 in
//!   [`FALL_TIME`];
//! - missing input (`None`, fewer than [`FFT_SIZE`] samples, or an unusable
//!   rate) means every target is 0, so the bars fall;
//! - a change in the bar count resets every level to 0, and the count is
//!   clamped to [`MAX_BARS`];
//! - after the first update for a given bar count and rate, `update` does not
//!   allocate (its buffers are reused and the band layout is cached);
//! - no band reads the DC bin: every band covers bins within
//!   `1..=FFT_SIZE / 2`, and the DC offset is removed before windowing.

use std::f32::consts::TAU;
use std::fmt;
use std::ops::Range;
use std::sync::Arc;
use std::time::{Duration, Instant};

use rustfft::num_complex::Complex;
use rustfft::{Fft, FftPlanner};

/// Number of samples analysed per frame. At 44.1 kHz that is about 46 ms of
/// audio and bins about 21.5 Hz wide. The audio tap keeps exactly this many.
pub const FFT_SIZE: usize = 2048;

/// Lower edge of the lowest band, in Hz.
const MIN_FREQ: f32 = 40.0;
/// Upper edge of the highest band, in Hz (capped at the Nyquist frequency).
const MAX_FREQ: f32 = 16_000.0;
/// Level shown as an empty bar; 0 dB is a full bar.
const FLOOR_DB: f32 = -60.0;
/// Time a bar takes to fall from full height to zero.
pub const FALL_TIME: Duration = Duration::from_secs(1);
/// Most bars [`Spectrum::update`] measures; more is clamped to this. Far more
/// than a terminal shows, and it bounds the buffers and keeps every count
/// exact as `f32`.
pub const MAX_BARS: usize = 1024;

/// Spectrum analyser and bar levels. See the module docs.
pub struct Spectrum {
    fft: Arc<dyn Fft<f32>>,
    /// Hann window, `FFT_SIZE` long.
    window: Vec<f32>,
    /// `2 / Σw`: scales `|X[k]|` so a full-scale sine reads 1.0 (0 dB).
    scale: f32,
    /// FFT input and output, `FFT_SIZE` long.
    buffer: Vec<Complex<f32>>,
    /// Scratch space the FFT asks for.
    scratch: Vec<Complex<f32>>,
    /// The FFT bins each band covers, for `bands_for`.
    bands: Vec<Range<usize>>,
    /// `(bars, rate)` that `bands` was computed for.
    bands_for: (usize, u32),
    /// This frame's target level per bar.
    targets: Vec<f32>,
    /// Displayed level per bar, `0.0..=1.0`.
    levels: Vec<f32>,
    /// Time of the last update, for the fall.
    last: Option<Instant>,
}

impl Spectrum {
    /// A spectrum with no bars yet; the FFT is planned once here.
    #[must_use]
    pub fn new() -> Self {
        let fft = FftPlanner::<f32>::new().plan_fft_forward(FFT_SIZE);
        let window: Vec<f32> = (0..FFT_SIZE)
            .map(|n| 0.5 - 0.5 * (TAU * to_f32(n) / to_f32(FFT_SIZE)).cos())
            .collect();
        let window_sum: f32 = window.iter().sum();
        let scratch = vec![Complex::default(); fft.get_inplace_scratch_len()];
        Self {
            fft,
            window,
            scale: 2.0 / window_sum,
            buffer: vec![Complex::default(); FFT_SIZE],
            scratch,
            bands: Vec::new(),
            bands_for: (0, 0),
            targets: Vec::new(),
            levels: Vec::new(),
            last: None,
        }
    }

    /// Measures `input` (the newest mono samples, oldest first, and their
    /// sample rate in Hz) into `bars` bands and moves the levels towards them:
    /// up at once, down by `dt / FALL_TIME` since the last update.
    ///
    /// `None`, fewer than [`FFT_SIZE`] samples, or a rate too low to cover any
    /// band give a target of 0 for every bar. Only the newest [`FFT_SIZE`]
    /// samples are used. `bars` is clamped to [`MAX_BARS`]; a count different
    /// from the last call resets every level to 0 first. The first update
    /// after [`Spectrum::new`] or [`Spectrum::reset`] applies no fall.
    pub fn update(&mut self, input: Option<(&[f32], u32)>, bars: usize, now: Instant) {
        let bars = bars.min(MAX_BARS);
        if bars != self.levels.len() {
            self.levels.clear();
            self.levels.resize(bars, 0.0);
        }
        self.targets.clear();
        self.targets.resize(bars, 0.0);
        if let Some((samples, rate)) = input
            && let Some(start) = samples.len().checked_sub(FFT_SIZE)
        {
            self.measure(&samples[start..], rate, bars);
        }

        let fall = self.last.map_or(0.0, |last| {
            now.saturating_duration_since(last).as_secs_f32() / FALL_TIME.as_secs_f32()
        });
        self.last = Some(now);
        for (level, &target) in self.levels.iter_mut().zip(&self.targets) {
            *level = target.max(*level - fall).max(0.0);
        }
    }

    /// Sets every level to 0 and forgets the last update time, for when the
    /// bars are not shown and must not reappear with a stale frame. The next
    /// update therefore applies no fall: it starts the clock again.
    pub fn reset(&mut self) {
        self.levels.fill(0.0);
        self.last = None;
    }

    /// The displayed level of each bar, lowest band first, each in `0.0..=1.0`.
    #[must_use]
    pub fn levels(&self) -> &[f32] {
        &self.levels
    }

    /// Whether every bar is at 0 (nothing left to animate).
    #[must_use]
    pub fn is_idle(&self) -> bool {
        self.levels.iter().all(|&level| level <= 0.0)
    }

    /// Fills `targets` from exactly `FFT_SIZE` samples at `rate` Hz.
    fn measure(&mut self, samples: &[f32], rate: u32, bars: usize) {
        // The window's mean (DC offset) is removed first: under the Hann window
        // it would leak into bins 0 and 1, which the lowest bands use at high
        // sample rates, and light them up with no audible content.
        let mean = samples.iter().copied().map(sanitize).sum::<f32>() / to_f32(FFT_SIZE);
        for ((slot, &sample), &weight) in self.buffer.iter_mut().zip(samples).zip(&self.window) {
            *slot = Complex::new((sanitize(sample) - mean) * weight, 0.0);
        }
        self.fft
            .process_with_scratch(&mut self.buffer, &mut self.scratch);

        if self.bands_for != (bars, rate) {
            self.bands = band_bins(bars, rate);
            self.bands_for = (bars, rate);
        }
        for (target, bins) in self.targets.iter_mut().zip(&self.bands) {
            let peak_squared = self
                .buffer
                .get(bins.clone())
                .unwrap_or_default()
                .iter()
                .map(Complex::norm_sqr)
                .fold(0.0, f32::max);
            *target = level_of(peak_squared.sqrt() * self.scale);
        }
    }
}

impl Default for Spectrum {
    fn default() -> Self {
        Self::new()
    }
}

// `rustfft::Fft` has no `Debug`, and `App` (which will own a `Spectrum`)
// derives it.
impl fmt::Debug for Spectrum {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Spectrum")
            .field("levels", &self.levels)
            .finish_non_exhaustive()
    }
}

/// The `bars + 1` band edges in Hz, log-spaced from [`MIN_FREQ`] to
/// `min(MAX_FREQ, rate / 2)`. Empty when `bars` is 0 or the rate is too low
/// for that range to exist.
fn band_edges(bars: usize, rate: u32) -> Vec<f32> {
    let top = MAX_FREQ.min(hz(rate) / 2.0);
    if bars == 0 || top <= MIN_FREQ {
        return Vec::new();
    }
    let ratio = top / MIN_FREQ;
    let mut edges: Vec<f32> = (0..bars)
        .map(|i| MIN_FREQ * ratio.powf(to_f32(i) / to_f32(bars)))
        .collect();
    // Exactly `top`, so rounding never pushes the last edge past Nyquist.
    edges.push(top);
    edges
}

/// The FFT bins each band covers: those whose centre frequency is in
/// `[lo, hi)`, or, for a band narrower than that, the one bin containing the
/// band's (geometric) centre. Every range lies within `1..=FFT_SIZE / 2`: bin
/// 0 is DC (any offset or rumble in the audio), always below [`MIN_FREQ`], so
/// at high rates (≥ ~160 kHz, where 40 Hz rounds to bin 0) the lowest bands
/// use bin 1 instead.
fn band_bins(bars: usize, rate: u32) -> Vec<Range<usize>> {
    let bin_hz = hz(rate) / to_f32(FFT_SIZE);
    band_edges(bars, rate)
        .windows(2)
        .map(|edge| {
            let (lo, hi) = (edge[0], edge[1]);
            let start = bin_index((lo / bin_hz).ceil()).max(1);
            let end = bin_index((hi / bin_hz).ceil());
            if start < end {
                start..end
            } else {
                let centre = bin_index(((lo * hi).sqrt() / bin_hz).round()).max(1);
                centre..centre + 1
            }
        })
        .collect()
}

/// A decoded sample made safe for the FFT: non-finite values count as silence
/// and the rest are clamped to full scale (±1.0), so the transform never sees
/// inf or overflows to NaN. Full scale is all a bar can show anyway.
fn sanitize(sample: f32) -> f32 {
    if sample.is_finite() {
        sample.clamp(-1.0, 1.0)
    } else {
        0.0
    }
}

/// Maps a linear magnitude (1.0 = full-scale sine) onto `0.0..=1.0` from
/// [`FLOOR_DB`] to 0 dB. Zero, negative and NaN magnitudes give 0.
fn level_of(magnitude: f32) -> f32 {
    if magnitude.is_nan() || magnitude <= 0.0 {
        return 0.0;
    }
    let db = 20.0 * magnitude.log10();
    ((db - FLOOR_DB) / -FLOOR_DB).clamp(0.0, 1.0)
}

/// `n` as `f32`. Callers pass counts no larger than [`FFT_SIZE`] or
/// [`MAX_BARS`], far below 2^24, so the conversion is exact.
#[allow(clippy::cast_precision_loss, reason = "values are < 2^24, see doc")]
fn to_f32(n: usize) -> f32 {
    n as f32
}

/// A sample rate as `f32`. Real rates (up to a few hundred kHz) are below
/// 2^24 and convert exactly; an absurd rate only loses precision, and the
/// bands are clamped to the FFT's range anyway.
#[allow(clippy::cast_precision_loss, reason = "real rates are < 2^24, see doc")]
fn hz(rate: u32) -> f32 {
    rate as f32
}

/// A (rounded) bin position as an index, clamped to `0..=FFT_SIZE / 2` (the
/// one-sided spectrum). NaN maps to 0.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "clamped to 0..=FFT_SIZE / 2 first; callers round"
)]
fn bin_index(bin: f32) -> usize {
    bin.clamp(0.0, to_f32(FFT_SIZE / 2)) as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATES: [u32; 3] = [44_100, 48_000, 96_000];

    /// `FFT_SIZE` samples of a sine at `freq` Hz with peak `amplitude`.
    fn sine(freq: f32, amplitude: f32, rate: u32) -> Vec<f32> {
        (0..FFT_SIZE)
            .map(|n| amplitude * (TAU * freq * to_f32(n) / hz(rate)).sin())
            .collect()
    }

    /// The frequency of FFT bin `k` at `rate`.
    fn bin_freq(k: usize, rate: u32) -> f32 {
        to_f32(k) * hz(rate) / to_f32(FFT_SIZE)
    }

    /// Index of the (first) tallest bar.
    fn tallest(levels: &[f32]) -> usize {
        levels
            .iter()
            .enumerate()
            .fold(
                (0, f32::MIN),
                |best, (i, &l)| if l > best.1 { (i, l) } else { best },
            )
            .0
    }

    fn peak(levels: &[f32]) -> f32 {
        levels.iter().copied().fold(0.0, f32::max)
    }

    fn measured(samples: &[f32], rate: u32, bars: usize) -> Spectrum {
        let mut spectrum = Spectrum::new();
        spectrum.update(Some((samples, rate)), bars, Instant::now());
        spectrum
    }

    #[test]
    fn low_tone_peaks_in_the_left_third() {
        for rate in RATES {
            for bars in [16, 39, 64] {
                let spectrum = measured(&sine(100.0, 0.5, rate), rate, bars);
                let at = tallest(spectrum.levels());
                assert!(peak(spectrum.levels()) > 0.5, "rate {rate}, bars {bars}");
                assert!(at < bars / 3, "rate {rate}, bars {bars}: tallest at {at}");
            }
        }
    }

    #[test]
    fn high_tone_peaks_in_the_right_half() {
        for rate in RATES {
            for bars in [16, 39, 64] {
                let spectrum = measured(&sine(5_000.0, 0.5, rate), rate, bars);
                let at = tallest(spectrum.levels());
                assert!(peak(spectrum.levels()) > 0.5, "rate {rate}, bars {bars}");
                assert!(at >= bars / 2, "rate {rate}, bars {bars}: tallest at {at}");
            }
        }
    }

    #[test]
    fn full_scale_sine_on_a_bin_reads_full_height() {
        for rate in RATES {
            let spectrum = measured(&sine(bin_freq(100, rate), 1.0, rate), rate, 32);
            let top = peak(spectrum.levels());
            assert!((0.95..=1.0).contains(&top), "rate {rate}: {top}");
        }
    }

    #[test]
    fn sine_below_the_floor_reads_zero() {
        let amplitude = 10f32.powf(-70.0 / 20.0);
        for rate in RATES {
            let spectrum = measured(&sine(bin_freq(100, rate), amplitude, rate), rate, 32);
            assert!(spectrum.is_idle(), "rate {rate}: {:?}", spectrum.levels());
        }
    }

    #[test]
    fn debug_shows_the_levels() {
        let spectrum = measured(&sine(1_000.0, 1.0, 44_100), 44_100, 4);
        let shown = format!("{spectrum:?}");
        assert!(shown.starts_with("Spectrum { levels: ["), "{shown}");
        assert!(shown.ends_with(".. }"), "{shown}");
    }

    #[test]
    fn missing_or_short_input_gives_zero_targets() {
        let silence = vec![0.0; FFT_SIZE];
        let short = sine(1_000.0, 1.0, 44_100)[..FFT_SIZE - 1].to_vec();
        let inputs: [Option<(&[f32], u32)>; 3] =
            [None, Some((&silence, 44_100)), Some((&short, 44_100))];
        for input in inputs {
            let mut spectrum = Spectrum::new();
            spectrum.update(input, 24, Instant::now());
            assert_eq!(spectrum.levels().len(), 24);
            assert!(spectrum.is_idle(), "{:?}", spectrum.levels());
        }
    }

    #[test]
    fn only_the_newest_samples_are_measured() {
        let mut samples = sine(1_000.0, 1.0, 44_100);
        samples.extend(std::iter::repeat_n(0.0, FFT_SIZE));
        assert!(measured(&samples, 44_100, 24).is_idle());

        let mut samples = vec![0.0; FFT_SIZE];
        samples.extend(sine(1_000.0, 1.0, 44_100));
        assert!(peak(measured(&samples, 44_100, 24).levels()) > 0.9);
    }

    #[test]
    fn bars_rise_at_once_and_fall_over_one_second() {
        let rate = 44_100;
        let tone = sine(bin_freq(100, rate), 1.0, rate);
        let t0 = Instant::now();
        let mut spectrum = Spectrum::new();
        spectrum.update(None, 32, t0);
        assert!(spectrum.is_idle());

        // Rises to the full target in the same frame.
        spectrum.update(Some((&tone, rate)), 32, t0 + Duration::from_millis(10));
        let full = peak(spectrum.levels());
        assert!(full >= 0.95, "{full}");

        spectrum.update(None, 32, t0 + Duration::from_millis(510));
        let half = peak(spectrum.levels());
        assert!((half - (full - 0.5)).abs() < 0.01, "{full} → {half}");
        assert!(!spectrum.is_idle());

        spectrum.update(None, 32, t0 + Duration::from_millis(1010));
        assert!(spectrum.is_idle(), "{:?}", spectrum.levels());
    }

    #[test]
    fn a_clock_going_backwards_does_not_raise_the_bars() {
        let rate = 44_100;
        let earlier = Instant::now();
        let later = earlier + Duration::from_secs(5);
        let mut spectrum = Spectrum::new();
        spectrum.update(
            Some((&sine(bin_freq(100, rate), 1.0, rate), rate)),
            8,
            later,
        );
        let before = spectrum.levels().to_vec();
        // `now` before the last update counts as no time passing.
        spectrum.update(None, 8, earlier);
        assert_eq!(spectrum.levels(), before.as_slice());
    }

    #[test]
    fn band_edges_increase_and_stop_at_nyquist() {
        for (rate, top) in [(44_100, 16_000.0), (22_050, 11_025.0), (8_000, 4_000.0)] {
            let edges = band_edges(40, rate);
            assert_eq!(edges.len(), 41);
            assert!((edges[0] - MIN_FREQ).abs() < f32::EPSILON);
            assert!(edges.windows(2).all(|e| e[0] < e[1]), "{edges:?}");
            assert!(
                (edges[40] - top).abs() < f32::EPSILON,
                "rate {rate}: {edges:?}"
            );
        }
    }

    #[test]
    fn bands_cover_valid_increasing_bins() {
        for rate in [
            8_000,
            22_050,
            44_100,
            48_000,
            96_000,
            176_400,
            192_000,
            u32::MAX,
        ] {
            for bars in [1, 2, 32, 39, 100, 500, MAX_BARS] {
                let bins = band_bins(bars, rate);
                assert_eq!(bins.len(), bars);
                assert!(
                    bins.iter()
                        .all(|b| 1 <= b.start && b.start < b.end && b.end <= FFT_SIZE / 2 + 1),
                    "rate {rate}, bars {bars}: {bins:?}"
                );
                assert!(
                    bins.windows(2).all(|b| b[0].start <= b[1].start),
                    "{bins:?}"
                );
            }
        }
    }

    #[test]
    fn a_rate_too_low_for_any_band_gives_zero() {
        let samples = vec![0.5; FFT_SIZE];
        for rate in [0, 1, 80] {
            assert!(band_edges(8, rate).is_empty(), "rate {rate}");
            let spectrum = measured(&samples, rate, 8);
            assert_eq!(spectrum.levels().len(), 8);
            assert!(spectrum.is_idle(), "rate {rate}");
        }
    }

    #[test]
    fn a_bar_count_change_resets_the_levels() {
        let rate = 44_100;
        let now = Instant::now();
        let mut spectrum = measured(&sine(1_000.0, 1.0, rate), rate, 16);
        assert!(!spectrum.is_idle());
        spectrum.update(None, 20, now);
        assert_eq!(spectrum.levels().len(), 20);
        assert!(spectrum.is_idle());

        spectrum.update(None, 0, now);
        assert_eq!(spectrum.levels(), []);
        assert!(spectrum.is_idle());
    }

    #[test]
    fn reset_clears_the_levels() {
        let mut spectrum = measured(&sine(1_000.0, 1.0, 44_100), 44_100, 16);
        spectrum.reset();
        assert_eq!(spectrum.levels().len(), 16);
        assert!(spectrum.is_idle());
    }

    #[test]
    fn hostile_samples_keep_levels_finite_and_in_range() {
        let mut samples = sine(1_000.0, 1.0, 44_100);
        for (i, sample) in samples.iter_mut().enumerate() {
            *sample = match i % 4 {
                0 => f32::NAN,
                1 => f32::INFINITY,
                2 => f32::MAX,
                _ => -f32::MAX,
            };
        }
        let spectrum = measured(&samples, 44_100, 32);
        assert!(
            spectrum
                .levels()
                .iter()
                .all(|l| l.is_finite() && (0.0..=1.0).contains(l)),
            "{:?}",
            spectrum.levels()
        );
    }

    #[test]
    fn levels_follow_the_decibel_scale() {
        // −20 dB → 40/60 and −6.02 dB → ~0.9: pins the `2 / Σw` scale both ways.
        for rate in RATES {
            for (amplitude, expected) in [(0.1, 2.0 / 3.0), (0.5, 0.899_7)] {
                let tone = sine(bin_freq(100, rate), amplitude, rate);
                let top = peak(measured(&tone, rate, 32).levels());
                assert!(
                    (top - expected).abs() < 0.02,
                    "rate {rate}, amplitude {amplitude}: {top}"
                );
            }
        }
    }

    #[test]
    fn narrow_low_bands_share_the_bin_holding_their_centre() {
        // At 44.1 kHz bins are ~21.5 Hz wide; with 64 bars the first band is
        // 40–43.9 Hz, centred on ~41.9 Hz, which is in bin 2 (32.3–53.8 Hz).
        let bins = band_bins(64, 44_100);
        assert_eq!(bins[0], 2..3);
        assert_eq!(bins[1], 2..3);
        // Where bands are at least a bin wide they tile without gaps.
        let wide: Vec<_> = bins.iter().filter(|b| b.end - b.start > 1).collect();
        assert!(wide.len() > 10, "{wide:?}");
        assert!(wide.windows(2).all(|b| b[0].end == b[1].start), "{wide:?}");
        assert_eq!(
            bins.last().map(|b| b.end),
            Some(bin_index((16_000.0 / bin_freq(1, 44_100)).ceil()))
        );
    }

    #[test]
    fn the_lowest_bar_ignores_dc_at_high_rates() {
        // A DC offset alone (no audible content) must not light up bar 0.
        for rate in [176_400, 192_000] {
            let spectrum = measured(&vec![0.5; FFT_SIZE], rate, 32);
            assert!(
                spectrum.levels()[0] < 0.1,
                "rate {rate}: {:?}",
                spectrum.levels()
            );
        }
    }

    #[test]
    fn odd_rates_keep_levels_finite_and_in_range() {
        for rate in [81, 1_000, u32::MAX] {
            let spectrum = measured(&sine(30.0, 1.0, rate.max(100)), rate, 16);
            assert_eq!(spectrum.levels().len(), 16);
            assert!(
                spectrum
                    .levels()
                    .iter()
                    .all(|l| l.is_finite() && (0.0..=1.0).contains(l)),
                "rate {rate}: {:?}",
                spectrum.levels()
            );
        }
    }

    #[test]
    fn the_bar_count_is_clamped() {
        let mut spectrum = Spectrum::new();
        spectrum.update(None, usize::MAX, Instant::now());
        assert_eq!(spectrum.levels().len(), MAX_BARS);
    }

    #[test]
    fn repeated_updates_reuse_their_buffers() {
        let rate = 44_100;
        let tone = sine(1_000.0, 1.0, rate);
        let mut spectrum = Spectrum::new();
        spectrum.update(Some((&tone, rate)), 40, Instant::now());
        let before = (
            spectrum.bands.as_ptr(),
            spectrum.targets.as_ptr(),
            spectrum.levels.as_ptr(),
            spectrum.buffer.as_ptr(),
        );
        for _ in 0..3 {
            spectrum.update(Some((&tone, rate)), 40, Instant::now());
            spectrum.update(None, 40, Instant::now());
        }
        let after = (
            spectrum.bands.as_ptr(),
            spectrum.targets.as_ptr(),
            spectrum.levels.as_ptr(),
            spectrum.buffer.as_ptr(),
        );
        assert_eq!(before, after);
    }
}
