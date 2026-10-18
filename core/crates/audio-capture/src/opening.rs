//! Waiting for a device thread to open its device, in two stages with two bounds.
//!
//! # Why two
//!
//! [`crate::MicrophoneSource`] opens its device on a thread of its own and waits for word back, so
//! that a missing device or a refused permission surfaces when the recording starts rather than as
//! silence once it has. That wait has to be bounded — a wedged audio driver must not hang the
//! request that started a meeting — but it was one bound over two very different steps:
//!
//! 1. **Finding the device.** The first Core Audio call a process makes connects it to the audio
//!    system and lists every device. On a Mac with a handful of virtual devices installed that
//!    measured 4.5–7.1 s, every time, in a fresh process.
//! 2. **Opening it.** Building and starting the stream, which is quick when it works.
//!
//! Five seconds over both meant the first recording in a fresh engine could fail with "did not
//! open in time" on a machine that was merely slow to list its devices — and so did the test
//! asking for a device that does not exist, which never opens anything at all. Each step now has
//! its own bound: a generous one for the listing, which is slow but finite, and the original five
//! seconds for the open, which is what a driver that has stopped responding looks like.

use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::Duration;

use crate::{CaptureError, Result};

/// How long the audio system may take to list its devices.
pub(crate) const FIND_TIMEOUT: Duration = Duration::from_secs(30);

/// How long a found device may take to start delivering.
pub(crate) const OPEN_TIMEOUT: Duration = Duration::from_secs(5);

/// What a device thread reports while opening.
#[derive(Debug)]
pub(crate) enum Opening<T> {
    /// The device exists and is about to be opened.
    Found,
    /// It is open and delivering.
    Opened(T),
    /// It could not be found or opened, and why.
    Failed(CaptureError),
}

/// Wait for a device thread's reports, applying each stage's bound.
pub(crate) fn await_open<T>(
    reports: &Receiver<Opening<T>>,
    find_timeout: Duration,
    open_timeout: Duration,
) -> Result<T> {
    let first = reports.recv_timeout(find_timeout).map_err(|e| match e {
        RecvTimeoutError::Timeout => {
            CaptureError::BadFormat("the audio system did not list its devices in time".into())
        }
        RecvTimeoutError::Disconnected => {
            CaptureError::BadFormat("the audio device thread stopped before reporting".into())
        }
    })?;

    match first {
        Opening::Opened(opened) => Ok(opened),
        Opening::Failed(error) => Err(error),
        Opening::Found => match reports.recv_timeout(open_timeout) {
            Ok(Opening::Opened(opened)) => Ok(opened),
            Ok(Opening::Failed(error)) => Err(error),
            Ok(Opening::Found) => Err(CaptureError::BadFormat(
                "the audio device thread reported finding its device twice".into(),
            )),
            Err(RecvTimeoutError::Timeout) => Err(CaptureError::BadFormat(
                "audio device did not open in time".into(),
            )),
            Err(RecvTimeoutError::Disconnected) => Err(CaptureError::BadFormat(
                "the audio device thread stopped while opening".into(),
            )),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::channel;
    use std::thread;

    const SHORT: Duration = Duration::from_millis(100);

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    /// The measured case: the listing takes longer than the open is allowed, and that is fine.
    #[test]
    fn a_slow_device_listing_is_not_mistaken_for_a_device_that_will_not_open() {
        let (tx, rx) = channel();
        thread::spawn(move || {
            thread::sleep(ms(300));
            let _ = tx.send(Opening::Found);
            thread::sleep(ms(20));
            let _ = tx.send(Opening::Opened("mic"));
        });

        assert_eq!(await_open(&rx, ms(2_000), SHORT).expect("opened"), "mic");
    }

    /// A missing device is reported as missing, however long it took to find out.
    #[test]
    fn a_device_that_does_not_exist_is_reported_as_such() {
        let (tx, rx) = channel::<Opening<()>>();
        thread::spawn(move || {
            thread::sleep(ms(300));
            let _ = tx.send(Opening::Failed(CaptureError::DeviceNotFound("Nope".into())));
        });

        assert!(matches!(
            await_open(&rx, ms(2_000), SHORT),
            Err(CaptureError::DeviceNotFound(name)) if name == "Nope"
        ));
    }

    /// The guard the timeout exists for: a device found and then never delivering.
    #[test]
    fn a_device_that_never_opens_still_times_out() {
        let (tx, rx) = channel::<Opening<()>>();
        thread::spawn(move || {
            let _ = tx.send(Opening::Found);
            thread::sleep(ms(1_000));
            drop(tx);
        });

        let error = await_open(&rx, ms(2_000), SHORT).expect_err("must not wait for ever");
        assert!(
            error.to_string().contains("did not open in time"),
            "{error}"
        );
    }

    /// And the listing has a bound of its own.
    #[test]
    fn an_audio_system_that_never_answers_times_out_too() {
        let (tx, rx) = channel::<Opening<()>>();
        let worker = thread::spawn(move || {
            thread::sleep(ms(500));
            drop(tx);
        });

        let error = await_open(&rx, SHORT, SHORT).expect_err("must not wait for ever");
        assert!(error.to_string().contains("list its devices"), "{error}");
        let _ = worker.join();
    }
}
