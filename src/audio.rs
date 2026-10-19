use std::fs::File;
use std::path::{Path, PathBuf};
use std::time::Duration;

use std::num::NonZero;

use rodio::buffer::SamplesBuffer;
use rodio::{Decoder, MixerDeviceSink, Player, Source};

/// Preview player. The output device is opened lazily so startup stays instant.
pub struct Audio {
    sink: Option<MixerDeviceSink>,
    player: Option<Player>,
    pub current: Option<PathBuf>,
    pub duration: Option<Duration>,
    volume: f32,
}

impl Audio {
    pub fn new(volume: f32) -> Self {
        Self { sink: None, player: None, current: None, duration: None, volume }
    }

    pub fn play(&mut self, path: &Path) -> Result<(), String> {
        self.stop();
        if self.sink.is_none() {
            let mut sink = rodio::DeviceSinkBuilder::open_default_sink().map_err(|e| e.to_string())?;
            sink.log_on_drop(false);
            self.sink = Some(sink);
        }
        let sink = self.sink.as_ref().unwrap();
        let player = Player::connect_new(sink.mixer());
        player.set_volume(self.volume);
        match crate::multitrack::loudest_track(path) {
            Some(d) => {
                let (Some(ch), Some(rate)) = (NonZero::new(d.channels), NonZero::new(d.rate)) else {
                    return Err("bad audio track".into());
                };
                let buffer = SamplesBuffer::new(ch, rate, d.samples);
                self.duration = buffer.total_duration();
                player.append(buffer);
            }
            None => {
                let file = File::open(path).map_err(|e| e.to_string())?;
                let decoder = Decoder::try_from(file).map_err(|e| e.to_string())?;
                self.duration = decoder.total_duration();
                player.append(decoder);
            }
        }
        self.player = Some(player);
        self.current = Some(path.to_path_buf());
        Ok(())
    }

    pub fn stop(&mut self) {
        if let Some(p) = self.player.take() {
            p.stop();
        }
        self.current = None;
        self.duration = None;
    }

    pub fn toggle_pause(&mut self) {
        if let Some(p) = &self.player {
            if p.is_paused() { p.play() } else { p.pause() }
        }
    }

    pub fn is_playing(&self) -> bool {
        self.player.as_ref().is_some_and(|p| !p.is_paused() && !p.empty())
    }

    pub fn finished(&self) -> bool {
        self.player.as_ref().is_some_and(|p| p.empty())
    }

    pub fn position(&self) -> Duration {
        self.player.as_ref().map(|p| p.get_pos()).unwrap_or_default()
    }

    pub fn seek(&self, pos: Duration) {
        if let Some(p) = &self.player {
            let _ = p.try_seek(pos);
        }
    }

    pub fn volume(&self) -> f32 {
        self.volume
    }

    pub fn set_volume(&mut self, v: f32) {
        self.volume = v;
        if let Some(p) = &self.player {
            p.set_volume(v);
        }
    }
}

/// Reads just enough of the file to learn its length.
pub fn probe_duration(path: &Path) -> Option<f32> {
    let file = File::open(path).ok()?;
    let decoder = Decoder::try_from(file).ok()?;
    decoder.total_duration().map(|d| d.as_secs_f32())
}
