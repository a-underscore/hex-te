use crate::nalgebra::{Matrix4, Perspective3};

use std::sync::{Arc, RwLock};

/// A perspective camera. The projection is cached and rebuilt whenever the
/// aspect ratio or field of view changes; position and orientation come from
/// the entity's [`Trans3`](crate::components::Trans3).
#[derive(Clone)]
pub struct Camera3 {
    aspect: f32,
    fovy: f32,
    near: f32,
    far: f32,
    proj: Matrix4<f32>,
}

impl Camera3 {
    pub fn new(aspect: f32, fovy: f32, near: f32, far: f32) -> Arc<RwLock<Self>> {
        Arc::new(RwLock::new(Self {
            aspect,
            fovy,
            near,
            far,
            proj: Self::calculate_proj(aspect, fovy, near, far),
        }))
    }

    pub fn aspect(&self) -> f32 {
        self.aspect
    }

    pub fn set_aspect(&mut self, aspect: f32) {
        self.aspect = aspect;

        self.update_proj();
    }

    pub fn fovy(&self) -> f32 {
        self.fovy
    }

    pub fn set_fovy(&mut self, fovy: f32) {
        self.fovy = fovy;

        self.update_proj();
    }

    pub fn near(&self) -> f32 {
        self.near
    }

    pub fn set_near(&mut self, near: f32) {
        self.near = near;

        self.update_proj();
    }

    pub fn proj(&self) -> Matrix4<f32> {
        self.proj
    }

    fn update_proj(&mut self) {
        self.proj = Self::calculate_proj(self.aspect, self.fovy, self.near, self.far);
    }

    fn calculate_proj(aspect: f32, fovy: f32, near: f32, far: f32) -> Matrix4<f32> {
        super::wgpu_clip_correction() * Perspective3::new(aspect, fovy, near, far).to_homogeneous()
    }
}

#[cfg(test)]
mod tests {
    use super::Camera3;

    #[test]
    fn changing_the_aspect_rebuilds_the_projection() {
        let camera = Camera3::new(1.0, std::f32::consts::FRAC_PI_3, 0.1, 100.0);
        let mut camera = camera.write().unwrap();

        let before = camera.proj();

        camera.set_aspect(2.0);

        assert_eq!(camera.aspect(), 2.0);
        assert_ne!(before, camera.proj());
    }
}
