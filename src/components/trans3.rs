use std::sync::{Arc, RwLock};

use nalgebra::{Matrix4, Rotation3, Vector3};

/// A 3D transform: translation, rotation (axis-angle, radians) and scale.
/// The model matrix applies scale in the object's local space, then rotation,
/// then translation.
#[derive(Clone)]
pub struct Trans3 {
    pub position: Vector3<f32>,
    pub rotation: Vector3<f32>,
    pub scale: Vector3<f32>,
}

impl Trans3 {
    pub fn new(
        position: Vector3<f32>,
        rotation: Vector3<f32>,
        scale: Vector3<f32>,
    ) -> Arc<RwLock<Self>> {
        Arc::new(RwLock::new(Self {
            position,
            rotation,
            scale,
        }))
    }

    pub fn matrix(&self) -> Matrix4<f32> {
        Matrix4::new_translation(&self.position)
            * Matrix4::new_rotation(self.rotation)
            * Matrix4::new_nonuniform_scaling(&self.scale)
    }

    /// Orients the transform so its local -Z axis points from `position` toward
    /// `target`, matching the right-handed look-at convention used by the
    /// renderers (a camera with an identity rotation looks down -Z).
    ///
    /// `up` must not be collinear with the view direction.
    pub fn look_at(&mut self, target: Vector3<f32>, up: Vector3<f32>) {
        let f = (target - self.position).normalize();
        let s = f.cross(&up).normalize();
        let u = s.cross(&f);

        let rotation = Rotation3::from_basis_unchecked(&[s, u, -f]);

        self.rotation = if rotation.angle() > std::f32::consts::PI - 1.0e-2 {
            // Near a 180° flip the axis cannot be recovered from the matrix
            // (its skew-symmetric part vanishes), so derive it from the
            // rotated up vector instead: R·y = u ⇒ axis ∝ u + y.
            (u + Vector3::y()).try_normalize(1.0e-6).unwrap_or(s) * std::f32::consts::PI
        } else {
            rotation.scaled_axis()
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use nalgebra::Vector4;

    fn forward(trans: &Trans3) -> Vector3<f32> {
        (trans.matrix() * Vector4::new(0.0, 0.0, -1.0, 0.0)).xyz()
    }

    #[test]
    fn look_at_identity() {
        let trans = Trans3::new(
            Vector3::new(0.0, 0.0, 5.0),
            Vector3::zeros(),
            Vector3::new(1.0, 1.0, 1.0),
        );
        let mut trans = trans.write().unwrap();

        trans.look_at(Vector3::zeros(), Vector3::y());

        assert!(trans.rotation.norm() < 1.0e-6);
    }

    #[test]
    fn look_at_forward_points_at_target() {
        let cases = [
            (Vector3::new(5.0, 0.0, 0.0), Vector3::zeros()),
            (Vector3::new(0.0, 0.0, -5.0), Vector3::zeros()),
            (Vector3::new(-2.0, 3.0, 7.0), Vector3::new(4.0, -1.0, 2.0)),
        ];

        for (position, target) in cases {
            let trans = Trans3::new(position, Vector3::zeros(), Vector3::new(1.0, 1.0, 1.0));
            let mut trans = trans.write().unwrap();

            trans.look_at(target, Vector3::y());

            let expected = (target - position).normalize();

            assert!(
                (forward(&trans) - expected).norm() < 1.0e-5,
                "forward {:?} != {:?}",
                forward(&trans),
                expected,
            );
        }
    }

    #[test]
    fn look_at_up_is_up() {
        let trans = Trans3::new(
            Vector3::new(5.0, 0.0, 0.0),
            Vector3::zeros(),
            Vector3::new(1.0, 1.0, 1.0),
        );
        let mut trans = trans.write().unwrap();

        trans.look_at(Vector3::zeros(), Vector3::y());

        let up = (trans.matrix() * Vector4::new(0.0, 1.0, 0.0, 0.0)).xyz();

        assert!(up.dot(&Vector3::y()) > 0.99);
    }

    #[test]
    fn non_uniform_scale_rotates_rigidly() {
        // A non-uniformly scaled model must rotate as a rigid box: the scale
        // lives in the model's local space, so after a 90° yaw the local +Z
        // tip (scaled to length 4) still reaches 4 units along world +X.
        let trans = Trans3::new(
            Vector3::new(1.0, 2.0, 3.0),
            Vector3::new(0.0, std::f32::consts::FRAC_PI_2, 0.0),
            Vector3::new(2.0, 3.0, 4.0),
        );
        let trans = trans.read().unwrap();

        let z_tip = (trans.matrix() * Vector4::new(0.0, 0.0, 1.0, 1.0)).xyz();
        let x_tip = (trans.matrix() * Vector4::new(1.0, 0.0, 0.0, 1.0)).xyz();

        // Ry(90°): +Z -> +X, +X -> -Z, each still carrying its scaled length.
        assert!(
            (z_tip - Vector3::new(5.0, 2.0, 3.0)).norm() < 1.0e-5,
            "+Z tip landed at {z_tip:?}, expected (5, 2, 3)",
        );
        assert!(
            (x_tip - Vector3::new(1.0, 2.0, 1.0)).norm() < 1.0e-5,
            "+X tip landed at {x_tip:?}, expected (1, 2, 1)",
        );
    }
}
