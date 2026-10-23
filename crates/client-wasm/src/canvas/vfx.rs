#[cfg(target_arch = "wasm32")]
use web_sys::CanvasRenderingContext2d;

#[derive(Clone, Debug)]
pub struct Particle {
    pub x: f64,
    pub y: f64,
    pub vx: f64,
    pub vy: f64,
    pub size: f64,
    pub color: &'static str,
    pub life: f64,
    pub max_life: f64,
}

#[derive(Default)]
pub struct VfxManager {
    pub particles: Vec<Particle>,
    pub trauma: f64,
}

impl VfxManager {
    pub fn has_active(&self) -> bool {
        !self.particles.is_empty() || self.trauma > 0.0
    }

    pub fn clear(&mut self) {
        self.particles.clear();
        self.trauma = 0.0;
    }

    pub fn add_trauma(&mut self, amount: f64) {
        self.trauma = (self.trauma + amount).clamp(0.0, 1.0);
    }

    pub fn get_shake_offset(&self, seed: f64) -> (f64, f64) {
        if self.trauma <= 0.01 {
            return (0.0, 0.0);
        }
        let shake = self.trauma * self.trauma;
        let max_offset = 6.0;
        let dx = max_offset * shake * (seed * 17.13).sin();
        let dy = max_offset * shake * (seed * 23.41).cos();
        (dx, dy)
    }

    pub fn spawn_explosion(&mut self, x: f64, y: f64, is_dark: bool) {
        self.add_trauma(0.65);
        let colors = if is_dark {
            ["#ff6b57", "#e5b338", "#ff9b8a", "#4ea884"]
        } else {
            ["#a33a2b", "#d8a52b", "#ff7a63", "#244a3b"]
        };
        for i in 0..20 {
            let angle = (i as f64) * (std::f64::consts::TAU / 20.0) + ((i * 13) as f64 * 0.1).sin();
            let speed = 25.0 + ((i * 7 % 30) as f64);
            let color = colors[i % colors.len()];
            let size = 2.0 + (i % 3) as f64 * 1.5;
            self.particles.push(Particle {
                x,
                y,
                vx: angle.cos() * speed,
                vy: angle.sin() * speed,
                size,
                color,
                life: 0.0,
                max_life: 0.45 + (i % 5) as f64 * 0.08,
            });
        }
    }

    pub fn update(&mut self, dt: f64) {
        self.trauma = (self.trauma - dt * 2.2).max(0.0);
        self.particles.retain_mut(|p| {
            p.life += dt;
            if p.life >= p.max_life {
                return false;
            }
            p.x += p.vx * dt;
            p.y += p.vy * dt;
            p.vy += 35.0 * dt;
            p.vx *= 0.95_f64.powf(dt * 60.0);
            true
        });
    }

    #[cfg(target_arch = "wasm32")]
    pub fn render(&self, context: &CanvasRenderingContext2d) {
        for p in &self.particles {
            let progress = (p.life / p.max_life).clamp(0.0, 1.0);
            let alpha = 1.0 - progress;
            context.save();
            context.set_global_alpha(alpha);
            context.set_fill_style_str(p.color);
            context.begin_path();
            let _ = context.arc(
                p.x,
                p.y,
                (p.size * (1.0 - progress * 0.5)).max(0.5),
                0.0,
                std::f64::consts::TAU,
            );
            context.fill();
            context.restore();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explosion_decays_to_idle_and_clear_removes_all_effects() {
        let mut vfx = VfxManager::default();
        assert!(!vfx.has_active());
        vfx.spawn_explosion(100.0, 200.0, false);
        assert!(vfx.has_active());
        assert!(
            vfx.particles
                .iter()
                .all(|particle| particle.size > 0.0 && particle.color.starts_with('#'))
        );
        assert_ne!(vfx.get_shake_offset(0.0), (0.0, 0.0));
        for _ in 0..120 {
            vfx.update(1.0 / 120.0);
        }
        assert!(!vfx.has_active());
        assert_eq!(vfx.get_shake_offset(0.0), (0.0, 0.0));
        vfx.spawn_explosion(100.0, 200.0, true);
        vfx.clear();
        assert!(!vfx.has_active());
        assert!(vfx.particles.is_empty());
        assert_eq!(vfx.get_shake_offset(0.0), (0.0, 0.0));
    }
}
