use crate::{Hub, Ports};
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Floor {
    Http,
    AlarmClean,
    AlarmError,
}

#[derive(Debug, Default, serde::Serialize)]
pub struct AlarmReport {
    pub step_errors: Vec<(String, String)>,
    pub top_level_error: Option<String>,
}
#[derive(Debug, Default)]
pub struct StepOutcome {
    pub ok: u32,
    pub failed: u32,
    pub step_error: Option<String>,
}

impl<P: Ports> Hub<P> {
    pub async fn rearm(&self, _floor: Floor) {}
    pub async fn run_alarm(&self) -> AlarmReport {
        AlarmReport::default()
    }
}
