//! Some downloaded SFX are .mp4/.m4a files with two audio tracks where the first one is silent.
//! The normal decoder only plays the first track, so for those files we decode every track and
//! keep the one that actually has sound.

use std::collections::HashMap;
use std::fs::File;
use std::path::Path;

use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{CODEC_TYPE_NULL, Decoder, DecoderOptions};
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

pub struct Decoded {
    pub channels: u16,
    pub rate: u32,
    /// Interleaved samples.
    pub samples: Vec<f32>,
}

/// `Some` only for MP4-family files with more than one audio track: the loudest track, fully decoded.
pub fn loudest_track(path: &Path) -> Option<Decoded> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    if !matches!(ext.as_str(), "m4a" | "mp4" | "mov" | "m4v") {
        return None;
    }
    let mss = MediaSourceStream::new(Box::new(File::open(path).ok()?), Default::default());
    let mut hint = Hint::new();
    hint.with_extension(&ext);
    let probed = symphonia::default::get_probe()
        .format(&hint, mss, &FormatOptions::default(), &MetadataOptions::default())
        .ok()?;
    let mut format = probed.format;

    let mut tracks: HashMap<u32, (Box<dyn Decoder>, Decoded)> = HashMap::new();
    for t in format.tracks().iter().filter(|t| t.codec_params.codec != CODEC_TYPE_NULL) {
        if let Ok(dec) = symphonia::default::get_codecs().make(&t.codec_params, &DecoderOptions::default()) {
            tracks.insert(t.id, (dec, Decoded { channels: 0, rate: 0, samples: Vec::new() }));
        }
    }
    if tracks.len() < 2 {
        return None;
    }

    while let Ok(packet) = format.next_packet() {
        let Some((dec, out)) = tracks.get_mut(&packet.track_id()) else { continue };
        let Ok(buf) = dec.decode(&packet) else { continue };
        let spec = *buf.spec();
        let mut sb = SampleBuffer::<f32>::new(buf.capacity() as u64, spec);
        sb.copy_interleaved_ref(buf);
        out.channels = spec.channels.count() as u16;
        out.rate = spec.rate;
        out.samples.extend_from_slice(sb.samples());
    }

    let energy = |d: &Decoded| d.samples.iter().map(|s| (*s as f64) * (*s as f64)).sum::<f64>() / d.samples.len().max(1) as f64;
    tracks
        .into_values()
        .map(|(_, d)| d)
        .filter(|d| d.channels > 0 && d.rate > 0 && !d.samples.is_empty())
        .max_by(|a, b| energy(a).total_cmp(&energy(b)))
}
