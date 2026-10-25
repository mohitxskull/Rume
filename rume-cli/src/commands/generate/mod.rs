mod handler;

pub fn run(kind: Option<&str>, name: Option<&str>) {
    let Some(kind) = kind else {
        println!("Available generators:");
        println!("  handler <name>    Generate an HTTP handler and register its route");
        println!();
        println!("Usage:");
        println!("  rume generate handler users");
        return;
    };

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
