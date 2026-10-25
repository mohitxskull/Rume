mod handler;

pub fn run(kind: &str, name: Option<&str>) {
    match kind {
        "handler" => {
            let Some(name) = name else {
                eprintln!("error: `rume generate handler` requires a name");
                eprintln!("usage: rume generate handler <name>");
                std::process::exit(1);
            };
            handler::run(name);
        }
        _ => {
            eprintln!("error: unknown generator `{kind}`");
            eprintln!("available generators: handler");
            std::process::exit(1);
        }
    }
}
