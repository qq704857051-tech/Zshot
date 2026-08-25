fn main() {
    if let Err(e) = zshot::run() {
        eprintln!("{e}");
        std::process::exit(1);
    }
}
