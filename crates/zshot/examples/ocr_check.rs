//! Manual OCR check — the unit tests cannot ship 31 MB of models.
//!
//! ```text
//! set ZSHOT_MODELS=F:\own\Zshot\build\app\models\v6
//! cargo run --release --example ocr_check -- shot.png
//! ```

fn main() {
    let path = std::env::args().nth(1).expect("usage: ocr_check <image>");
    let img = image::open(&path).expect("open image").to_rgba8();
    let (w, h) = img.dimensions();

    // The overlay hands OCR a BGRA crop straight off a D3D readback, so it is
    // padded. Feeding a packed buffer here would not exercise that.
    let stride = w * 4 + 12;
    let mut bgra = vec![0u8; (stride * h) as usize];
    for (x, y, p) in img.enumerate_pixels() {
        let at = (y * stride + x * 4) as usize;
        bgra[at..at + 4].copy_from_slice(&[p[2], p[1], p[0], p[3]]);
    }

    let started = std::time::Instant::now();
    match zshot::ocr::recognize(&bgra, w, h, stride) {
        Ok(text) => println!("{:?}\n{text}", started.elapsed()),
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }

    // `--hold` keeps the process alive past the 60 s idle drop so an outside
    // sampler can see whether ORT actually hands the memory back.
    if std::env::args().any(|a| a == "--hold") {
        println!("holding, pid {}", std::process::id());
        std::thread::sleep(std::time::Duration::from_secs(100));
    }
}
