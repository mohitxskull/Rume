mod handler;
mod middleware;
mod model;
mod service;

pub fn run(kind: Option<&str>, name: Option<&str>) {
    let Some(kind) = kind else {
        println!("Available generators:");
        println!("  handler <name>     Generate an HTTP handler and register its route");
        println!("  service <name>     Generate a service struct");
        println!("  model <name>       Generate a model struct");
        println!("  middleware <name>  Generate a middleware function");
        println!();
        println!("Usage:");
        println!("  rume generate handler users");
        println!("  rume generate service users");
        println!("  rume generate model user");
        println!("  rume generate middleware auth");
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
        "service" => {
            let Some(name) = name else {
                eprintln!("error: `rume generate service` requires a name");
                eprintln!("usage: rume generate service <name>");
                std::process::exit(1);
            };
            service::run(name);
        }
        "model" => {
            let Some(name) = name else {
                eprintln!("error: `rume generate model` requires a name");
                eprintln!("usage: rume generate model <name>");
                std::process::exit(1);
            };
            model::run(name);
        }
        "middleware" => {
            let Some(name) = name else {
                eprintln!("error: `rume generate middleware` requires a name");
                eprintln!("usage: rume generate middleware <name>");
                std::process::exit(1);
            };
            middleware::run(name);
        }
        _ => {
            eprintln!("error: unknown generator `{kind}`");
            eprintln!("available generators: handler, service, model, middleware");
            std::process::exit(1);
        }
    }
}