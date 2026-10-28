// FaceHAL/src/selftest.rs
//
// Device-side self test for the SenseTime algorithm binding.
//
// The stock HIDL module passes a 24-byte `st_image_t` descriptor to
// `st_auth_enroll` / `st_auth_unlock`, not a bare frame pointer (see
// re/notes/face-hal-re.md, section 5). This binary links the same
// libstfaceunlockppl.so and calls the same entry points with the same
// descriptor shape, so the descriptor layout can be proved on the device
// without a camera.
//
// It prints the return code of each call. A descriptor the library cannot
// parse shows up as a negative SenseTime error or a crash; a correct one
// gets as far as "no face in this image".

mod logging;
mod stface;

use stface::{EnrollOutcome, StFace, StImage};

/// A synthetic NV21 frame. The algorithm cannot find a face in it, so a
/// successful parse ends in a detection failure rather than success — which is
/// still proof that the descriptor was read as intended.
///
/// The stock HAL's geometry is 640x480 with a row stride of 640, and it feeds
/// the algorithm an NV21 buffer (the camera's Y plane followed by the
/// interleaved V and U planes).
fn nv21_frame(width: i32, height: i32) -> Vec<u8> {
    let w = width as usize;
    let h = height as usize;
    let mut v = vec![0u8; w * h + 2 * (w / 2) * (h / 2)];
    let mut x = 0u32;
    for row in v.chunks_mut(w) {
        for (i, px) in row.iter_mut().enumerate() {
            x = x.wrapping_mul(1103515245).wrapping_add(12345);
            *px = ((x >> 16) as u8) ^ (i as u8 & 0x3f);
        }
    }
    v
}

fn main() {
    logging::init();

    let width = 640i32;
    let height = 480i32;
    let stride = width;

    let mut algo = StFace::new();
    match algo.init() {
        Ok(()) => logging::info("selftest: algorithm init OK"),
        Err(e) => {
            logging::error(&format!("selftest: algorithm init failed ret={}", e));
            std::process::exit(1);
        }
    }

    // A real face frame read from disk proves the whole descriptor + NV21
    // path end to end; the synthetic noise below only proves parsing.
    // A directory of NV21 frames is fed in order, mimicking the live
    // enrollment sequence the stock HAL drives from the camera.
    if let Some(dir) = std::env::args().nth(1) {
        if std::path::Path::new(&dir).is_dir() {
            let mut frames: Vec<(String, Vec<u8>)> = std::fs::read_dir(&dir)
                .ok()
                .map(|rd| {
                    rd.filter_map(|e| e.ok())
                        .filter(|e| e.path().extension().map(|x| x == "nv21").unwrap_or(false))
                        .filter_map(|e| {
                            let p = e.path();
                            std::fs::read(&p).ok().map(|d| (p.display().to_string(), d))
                        })
                        .collect()
                })
                .unwrap_or_default();
            frames.sort();
            if !frames.is_empty() {
                let mut algo2 = StFace::new();
                let _ = algo2.init();
                if let Err(e) = algo2.prepare_enroll() {
                    logging::error(&format!("selftest: prepare_enroll failed ret={}", e));
                }
                // The same frame repeatedly: the library accumulates one sample
                // per call and its return value is the number of samples it
                // still needs.
                for i in 0..12 {
                    let (r, out) =
                        algo2.enroll_raw_result(&frames[0].1, width, height, stride);
                    logging::info(&format!(
                        "selftest: same-frame #{} ret={} out={}",
                        i, r, out
                    ));
                    if r == 0 {
                        break;
                    }
                }
                for (name, d) in &frames {
                    match algo2.enroll(d, width, height, stride) {
                        Ok(EnrollOutcome::Complete(id)) => {
                            logging::info(&format!("selftest: {} -> face_id={}", name, id));
                            break;
                        }
                        Ok(EnrollOutcome::Accepted) => {
                            logging::info(&format!("selftest: {} -> accepted", name));
                        }
                        Err(e) => {
                            logging::info(&format!("selftest: {} -> ret={}", name, e));
                        }
                    }
                }
                for (name, d) in &frames {
                    match algo2.unlock(d, width, height, stride) {
                        Ok(()) => {
                            logging::info(&format!("selftest: unlock {} -> OK", name));
                            break;
                        }
                        Err(e) => logging::info(&format!("selftest: unlock {} -> ret={}", name, e)),
                    }
                }
                match algo2.unlock(&frames[0].1, width, height, stride) {
                    Ok(()) => logging::info("selftest: unlock OK"),
                    Err(e) => logging::info(&format!("selftest: unlock ret={}", e)),
                }
                logging::info("selftest: done");
                return;
            }
        }
    }

    let data: Vec<u8> = match std::env::args().nth(1) {
        Some(path) => match std::fs::read(&path) {
            Ok(d) => {
                logging::info(&format!("selftest: read {} bytes from {}", d.len(), path));
                d
            }
            Err(e) => {
                logging::error(&format!("selftest: read {} failed: {}", path, e));
                nv21_frame(width, height)
            }
        },
        None => nv21_frame(width, height),
    };
    let img = StImage::new(data.as_ptr() as *mut u8, width, height, stride);
    logging::info(&format!(
        "selftest: descriptor data={:p} tag={} width={} height={} stride={}",
        img.data, img.tag, img.width, img.height, img.stride
    ));

    // The library accumulates enrollment features across calls, so a single
    // frame is not enough: keep feeding the same face until it accepts one.
    for i in 0..12 {
        match algo.enroll(&data, width, height, stride) {
            Ok(EnrollOutcome::Complete(id)) => {
                logging::info(&format!("selftest: enroll #{} returned face_id={}", i, id));
                break;
            }
            Ok(EnrollOutcome::Accepted) => {
                logging::info(&format!("selftest: enroll #{} accepted the frame", i));
            }
            Err(e) => logging::info(&format!("selftest: enroll #{} returned ret={}", i, e)),
        }
    }

    // A/B: the pre-fix code passed the raw pixel pointer where the library
    // expects a pointer to the descriptor, so the library reads pixel bytes as
    // descriptor fields. Run that variant in a child process so a crash is
    // observable without taking this one down.
    if std::env::var("SELFTEST_RAW").is_ok() {
        logging::info("selftest: RAW variant — passing the pixel pointer where the descriptor belongs");
        match algo.enroll_raw(&data) {
            Ok(id) => logging::info(&format!("selftest: RAW enroll returned face_id={}", id)),
            Err(e) => logging::info(&format!("selftest: RAW enroll returned ret={}", e)),
        }
        logging::info("selftest: RAW survived");
    }

    match algo.unlock(&data, width, height, stride) {
        Ok(()) => logging::info("selftest: unlock returned OK (unexpected on noise)"),
        Err(e) => logging::info(&format!("selftest: unlock returned ret={}", e)),
    }

    logging::info("selftest: done");
}
