//! Second-order filter design and a trapezoidal state-variable filter.
//!
//! Responses follow the RBJ Audio EQ Cookbook
//! (<https://www.w3.org/TR/audio-eq-cookbook/>), realised as Andrew Simper's
//! linear trapezoidal SVF ("SvfLinearTrapOptimised2"). Both use the bilinear
//! transform prewarped at the centre frequency, so the transfer functions are
//! identical; the SVF is preferred because it stays well behaved while its
//! coefficients are being modulated and has good low-frequency precision in f32.

use std::f64::consts::PI;

use crate::BandKind;

/// Analog prototype `H(s) = m0 + (m1·s + m2) / (s² + k·s + 1)`, with `s`
/// normalised so that `s = j·tan(πf/fs) / g`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Prototype {
    g: f64,
    k: f64,
    m0: f64,
    m1: f64,
    m2: f64,
}

impl Prototype {
    /// `freq_hz` must already be in (0, fs/2). `gain_db` is ignored for kinds
    /// without a gain parameter.
    pub(crate) fn design(
        kind: BandKind,
        sample_rate: f64,
        freq_hz: f64,
        q: f64,
        gain_db: f64,
    ) -> Self {
        let g = (PI * freq_hz / sample_rate).tan();
        let k = 1.0 / q;
        // Cookbook `A`: sqrt of the linear gain, so A² is the full shelf/peak gain.
        let a = 10f64.powf(gain_db / 40.0);
        let (g, k, m0, m1, m2) = match kind {
            BandKind::Peak => (g, k / a, 1.0, k * (a - 1.0 / a), 0.0),
            BandKind::LowShelf => (g / a.sqrt(), k, 1.0, k * (a - 1.0), a * a - 1.0),
            BandKind::HighShelf => (g * a.sqrt(), k, a * a, k * (1.0 - a) * a, 1.0 - a * a),
            BandKind::LowPass => (g, k, 0.0, 0.0, 1.0),
            BandKind::HighPass => (g, k, 1.0, -k, -1.0),
            BandKind::Notch => (g, k, 1.0, -k, 0.0),
        };
        Self { g, k, m0, m1, m2 }
    }

    /// Exact magnitude of the discretised filter at `freq_hz` (in (0, fs/2)).
    pub(crate) fn magnitude(&self, sample_rate: f64, freq_hz: f64) -> f64 {
        let w = (PI * freq_hz / sample_rate).tan() / self.g;
        // H = (m0·D + N) / D with D = s² + k·s + 1 and N = m1·s + m2 at s = jw.
        let (d_re, d_im) = (1.0 - w * w, self.k * w);
        let n_re = self.m0 * d_re + self.m2;
        let n_im = self.m0 * d_im + self.m1 * w;
        ((n_re * n_re + n_im * n_im) / (d_re * d_re + d_im * d_im)).sqrt()
    }

    pub(crate) fn coeffs(&self) -> SvfCoeffs {
        let a1 = 1.0 / (1.0 + self.g * (self.g + self.k));
        let a2 = self.g * a1;
        let a3 = self.g * a2;
        SvfCoeffs {
            a1: a1 as f32,
            a2: a2 as f32,
            a3: a3 as f32,
        }
    }

    /// Weights `[m0, m1, m2]` of input, band-pass and low-pass outputs.
    pub(crate) fn weights(&self) -> [f32; 3] {
        [self.m0 as f32, self.m1 as f32, self.m2 as f32]
    }
}

/// Recursive part of the SVF. Kept apart from the output weights
/// ([`Prototype::weights`]) so those can be interpolated per sample.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct SvfCoeffs {
    a1: f32,
    a2: f32,
    a3: f32,
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Svf {
    ic1: f32,
    ic2: f32,
}

impl Svf {
    /// Advances one sample and returns `m0·v0 + m1·v1 + m2·v2`.
    #[inline]
    pub(crate) fn process(&mut self, c: &SvfCoeffs, m: [f32; 3], v0: f32) -> f32 {
        let v3 = v0 - self.ic2;
        let v1 = c.a1 * self.ic1 + c.a2 * v3;
        let v2 = self.ic2 + c.a2 * self.ic1 + c.a3 * v3;
        self.ic1 = 2.0 * v1 - self.ic1;
        self.ic2 = 2.0 * v2 - self.ic2;
        m[0] * v0 + m[1] * v1 + m[2] * v2
    }

    /// Zeroes state that has decayed towards the denormal range (Rust does not
    /// enable flush-to-zero) and recovers from a poisoned (non-finite) state.
    pub(crate) fn sanitize(&mut self) {
        if !(self.ic1.is_finite() && self.ic2.is_finite()) {
            *self = Self::default();
            return;
        }
        for s in [&mut self.ic1, &mut self.ic2] {
            if s.abs() < 1e-20 {
                *s = 0.0;
            }
        }
    }
}
