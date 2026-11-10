//! Thread scheduling helpers.
//!
//! Registering the audio thread with the Multimedia Class Scheduler Service
//! under the "Pro Audio" task tells Windows to treat its deadlines as hard,
//! which is what keeps buffer-ready events from being serviced late.

use windows::Win32::Foundation::HANDLE;
use windows::Win32::Media::{timeBeginPeriod, timeEndPeriod};
use windows::Win32::System::Threading::{
    AvRevertMmThreadCharacteristics, AvSetMmThreadCharacteristicsW, GetCurrentThread,
    SetThreadPriority, THREAD_PRIORITY_TIME_CRITICAL,
};
use windows::core::w;

/// Holds the MMCSS registration for as long as the audio thread runs.
/// Dropping it reverts the thread to normal scheduling.
pub struct ProAudioThread {
    handle: Option<HANDLE>,
}

impl ProAudioThread {
    /// Registers the *calling* thread as Pro Audio and raises its priority.
    /// Failure is non-fatal: audio still works, just with more jitter.
    pub fn register() -> ProAudioThread {
        let mut task_index: u32 = 0;
        let handle = unsafe { AvSetMmThreadCharacteristicsW(w!("Pro Audio"), &mut task_index) };
        let handle = match handle {
            Ok(h) if !h.is_invalid() => Some(h),
            _ => None,
        };

        // Belt and braces: even without MMCSS, ask for time-critical priority.
        unsafe {
            let _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_TIME_CRITICAL);
        }

        ProAudioThread { handle }
    }

    /// True when MMCSS accepted the registration.
    pub fn is_registered(&self) -> bool {
        self.handle.is_some()
    }
}

impl Drop for ProAudioThread {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            unsafe {
                let _ = AvRevertMmThreadCharacteristics(handle);
            }
        }
    }
}

/// Raises the system timer resolution to 1 ms for the lifetime of the process.
///
/// The network threads sleep in sub-millisecond slices when their ring runs
/// dry; without this the default ~15.6 ms tick would turn those into audible
/// stalls.
pub struct TimerResolution {
    active: bool,
}

impl TimerResolution {
    pub fn acquire() -> TimerResolution {
        let ok = unsafe { timeBeginPeriod(1) } == 0;
        TimerResolution { active: ok }
    }
}

impl Drop for TimerResolution {
    fn drop(&mut self) {
        if self.active {
            unsafe {
                timeEndPeriod(1);
            }
        }
    }
}
