use web_sys::{AudioContext, OscillatorType, Window};

pub struct SoundManager {
    context: Option<AudioContext>,
    muted: bool,
}

impl Default for SoundManager {
    fn default() -> Self {
        Self {
            context: None,
            muted: false,
        }
    }
}

impl SoundManager {
    pub fn new(window: &Window) -> Self {
        let muted = window
            .local_storage()
            .ok()
            .flatten()
            .and_then(|storage| storage.get_item("graphwar_muted").ok().flatten())
            .map(|val| val == "true")
            .unwrap_or(false);

        Self {
            context: None,
            muted,
        }
    }

    pub fn is_muted(&self) -> bool {
        self.muted
    }

    pub fn toggle_muted(&mut self, window: &Window) -> bool {
        self.muted = !self.muted;
        if let Ok(Some(storage)) = window.local_storage() {
            let _ = storage.set_item("graphwar_muted", if self.muted { "true" } else { "false" });
        }
        self.muted
    }

    fn ctx(&mut self) -> Option<&AudioContext> {
        if self.muted {
            return None;
        }
        if self.context.is_none() {
            self.context = AudioContext::new().ok();
        }
        if let Some(ref ctx) = self.context {
            let _ = ctx.resume();
        }
        self.context.as_ref()
    }

    pub fn play_shot(&mut self) {
        let Some(ctx) = self.ctx() else { return };
        let current_time = ctx.current_time();

        let Ok(osc) = ctx.create_oscillator() else {
            return;
        };
        let Ok(gain) = ctx.create_gain() else { return };

        osc.set_type(OscillatorType::Sawtooth);
        let freq = osc.frequency();
        let _ = freq.set_value_at_time(520.0, current_time);
        let _ = freq.exponential_ramp_to_value_at_time(110.0, current_time + 0.28);

        let gain_param = gain.gain();
        let _ = gain_param.set_value_at_time(0.20, current_time);
        let _ = gain_param.exponential_ramp_to_value_at_time(0.001, current_time + 0.28);

        let Ok(_) = osc.connect_with_audio_node(&gain) else {
            return;
        };
        let Ok(_) = gain.connect_with_audio_node(&ctx.destination()) else {
            return;
        };

        let _ = osc.start();
        let _ = osc.stop_with_when(current_time + 0.30);
    }

    pub fn play_explosion(&mut self) {
        let Some(ctx) = self.ctx() else { return };
        let current_time = ctx.current_time();

        let Ok(osc) = ctx.create_oscillator() else {
            return;
        };
        let Ok(gain) = ctx.create_gain() else { return };

        osc.set_type(OscillatorType::Square);
        let freq = osc.frequency();
        let _ = freq.set_value_at_time(140.0, current_time);
        let _ = freq.exponential_ramp_to_value_at_time(35.0, current_time + 0.42);

        let gain_param = gain.gain();
        let _ = gain_param.set_value_at_time(0.25, current_time);
        let _ = gain_param.exponential_ramp_to_value_at_time(0.001, current_time + 0.42);

        let Ok(_) = osc.connect_with_audio_node(&gain) else {
            return;
        };
        let Ok(_) = gain.connect_with_audio_node(&ctx.destination()) else {
            return;
        };

        let _ = osc.start();
        let _ = osc.stop_with_when(current_time + 0.45);
    }

    pub fn play_hit(&mut self) {
        let Some(ctx) = self.ctx() else { return };
        let current_time = ctx.current_time();

        let Ok(osc) = ctx.create_oscillator() else {
            return;
        };
        let Ok(gain) = ctx.create_gain() else { return };

        osc.set_type(OscillatorType::Sawtooth);
        let freq = osc.frequency();
        let _ = freq.set_value_at_time(750.0, current_time);
        let _ = freq.exponential_ramp_to_value_at_time(160.0, current_time + 0.22);

        let gain_param = gain.gain();
        let _ = gain_param.set_value_at_time(0.30, current_time);
        let _ = gain_param.exponential_ramp_to_value_at_time(0.001, current_time + 0.22);

        let Ok(_) = osc.connect_with_audio_node(&gain) else {
            return;
        };
        let Ok(_) = gain.connect_with_audio_node(&ctx.destination()) else {
            return;
        };

        let _ = osc.start();
        let _ = osc.stop_with_when(current_time + 0.25);
    }

    pub fn play_victory(&mut self) {
        let Some(ctx) = self.ctx() else { return };
        let current_time = ctx.current_time();

        let notes = [523.25, 659.25, 783.99, 1046.50]; // C5, E5, G5, C6
        for (i, &freq_val) in notes.iter().enumerate() {
            let start = current_time + (i as f64) * 0.12;
            let duration = 0.28;

            let Ok(osc) = ctx.create_oscillator() else {
                continue;
            };
            let Ok(gain) = ctx.create_gain() else {
                continue;
            };

            osc.set_type(OscillatorType::Triangle);
            let freq = osc.frequency();
            let _ = freq.set_value_at_time(freq_val, start);

            let gain_param = gain.gain();
            let _ = gain_param.set_value_at_time(0.22, start);
            let _ = gain_param.exponential_ramp_to_value_at_time(0.001, start + duration);

            let Ok(_) = osc.connect_with_audio_node(&gain) else {
                continue;
            };
            let Ok(_) = gain.connect_with_audio_node(&ctx.destination()) else {
                continue;
            };

            let _ = osc.start_with_when(start);
            let _ = osc.stop_with_when(start + duration + 0.05);
        }
    }
}
