use trace_iw4::Trace;

use crate::GroundTraceInput;
use crate::correct_solid::{CorrectSolidOutcome, correct_solid};

pub trait CollisionBackend {
    /// Wet feet/body/eyes (0..=3). Dry maps retain the native movement path.
    fn water_level(&self, _origin: [f32; 3], _view_height: f32) -> u8 {
        0
    }

    fn trace(&self, input: GroundTraceInput) -> Trace;

    fn correct_solid(
        &self,
        origin: [f32; 3],
        mins: [f32; 3],
        maxs: [f32; 3],
        tracemask: u32,
    ) -> Option<CorrectSolidOutcome> {
        correct_solid(origin, mins, maxs, tracemask, self)
    }

    fn penetrations(
        &self,
        _input: GroundTraceInput,
        _contacts: &mut crate::recovery::ContactBuffer,
    ) -> crate::recovery::Coverage {
        crate::recovery::Coverage::Unsupported
    }

    fn touch_entity(&self, _entity: i32) {}
}
