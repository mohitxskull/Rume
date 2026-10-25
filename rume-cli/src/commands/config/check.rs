pub fn run() {
    match rume::config::load_raw_pretty(std::path::Path::new(".")) {
        Ok(text) => println!("{text}"),
        Err(err) => {
            eprintln!("error: {err}");
            std::process::exit(1);
        }
    }
}
