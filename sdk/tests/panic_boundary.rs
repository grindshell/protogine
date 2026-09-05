use protogine_plugin_api::{PG_ERROR, PG_PANIC, catch_status};
use std::{
    process::{Command, Stdio},
    sync::atomic::{AtomicUsize, Ordering},
    time::{Duration, Instant},
};

static PAYLOAD_DROPS: AtomicUsize = AtomicUsize::new(0);
static SECONDARY_DROPS: AtomicUsize = AtomicUsize::new(0);

struct Payload(u32);
impl Drop for Payload {
    fn drop(&mut self) {
        PAYLOAD_DROPS.fetch_add(1, Ordering::Relaxed);
        match self.0 {
            3 => panic!("payload destructor panicked"),
            4 => std::panic::panic_any(SecondaryPayload),
            _ => {}
        }
    }
}

struct SecondaryPayload;
impl Drop for SecondaryPayload {
    fn drop(&mut self) {
        SECONDARY_DROPS.fetch_add(1, Ordering::Relaxed);
        panic!("secondary payload must not be dropped");
    }
}

#[inline(never)]
extern "C" fn entry(case: u32) -> u32 {
    catch_status(|| match case {
        0 => PG_ERROR,
        1 => panic!("ordinary unwind panic"),
        _ => std::panic::panic_any(Payload(case)),
    })
}

#[test]
fn panic_probe() {
    let Ok(case) = std::env::var("PROTOGINE_SDK_PANIC_CASE") else {
        return;
    };
    let case: u32 = case.parse().unwrap();
    // This is a dedicated child process; silence only its expected panic hooks.
    std::panic::set_hook(Box::new(|_| {}));
    let status = entry(case);
    assert_eq!(status, if case == 0 { PG_ERROR } else { PG_PANIC });
    assert_eq!(
        PAYLOAD_DROPS.load(Ordering::Relaxed),
        usize::from(case >= 2)
    );
    assert_eq!(SECONDARY_DROPS.load(Ordering::Relaxed), 0);
}

#[test]
fn panic_payload_cleanup_cannot_escape_the_c_boundary() {
    for case in 0..=4 {
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "panic_probe", "--nocapture"])
            .env("PROTOGINE_SDK_PANIC_CASE", case.to_string())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while child.try_wait().unwrap().is_none() {
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("panic fixture {case} exceeded the independent 10-second watchdog");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "panic fixture {case}: {}\n{}\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }
}
