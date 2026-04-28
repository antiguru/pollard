//! Build small profiles for tests using fxprof-processed-profile, then
//! serialize+deserialize them through our raw types.

use fxprof_processed_profile::{
    CategoryHandle, CpuDelta, Frame, FrameFlags, FrameInfo, Profile as FxProfile,
    SamplingInterval, Timestamp,
};
use std::time::SystemTime;

/// Stack helper: each entry is (function_name, module_label).
pub struct SampleSpec<'a> {
    pub stack: &'a [(&'a str, &'a str)],
    pub count: u32,
}

/// Build a single-thread profile with the given samples.
pub fn build_simple_profile(name: &str, samples: &[SampleSpec<'_>]) -> String {
    let mut profile = FxProfile::new(
        name,
        SystemTime::now().into(),
        SamplingInterval::from_millis(1),
    );
    let process =
        profile.add_process("test-process", 1, Timestamp::from_millis_since_reference(0.0));
    let thread = profile.add_thread(
        process,
        1,
        Timestamp::from_millis_since_reference(0.0),
        true,
    );
    profile.set_thread_name(thread, "Main");

    let mut t = 0.0_f64;
    for sample in samples {
        let frame_infos: Vec<FrameInfo> = sample
            .stack
            .iter()
            .map(|(fn_name, _module)| {
                let s_handle = profile.intern_string(fn_name);
                FrameInfo {
                    frame: Frame::Label(s_handle),
                    category_pair: CategoryHandle::OTHER.into(),
                    flags: FrameFlags::empty(),
                }
            })
            .collect();
        let stack = profile.intern_stack_frames(thread, frame_infos.into_iter());
        for _ in 0..sample.count {
            profile.add_sample(
                thread,
                Timestamp::from_millis_since_reference(t),
                stack,
                CpuDelta::ZERO,
                1,
            );
            t += 1.0;
        }
    }

    serde_json::to_string(&profile).unwrap()
}
