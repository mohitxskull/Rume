mod handler;
mod middleware;
mod model;
mod resource;
mod service;

pub fn run(kind: Option<&str>, name: Option<&str>) {
    let Some(kind) = kind else {
        println!("Available generators:");
        println!("  handler <name>     Generate an HTTP handler and register its route");
        println!("  service <name>     Generate a service struct");
        println!("  model <name>       Generate a model struct");
        println!("  middleware <name>  Generate a middleware function");
        println!("  resource <name>    Generate RESTful resource handlers and routes");
        println!();
        println!("Usage:");
        println!("  rume generate handler users");
        println!("  rume generate service users");
        println!("  rume generate model user");
        println!("  rume generate middleware auth");
        println!("  rume generate resource users");
        return;
    };

    let name = match name {
        Some(n) => n.to_string(),
        None => {
            let prompted = crate::prompts::ask(&format!("{} name", kind));
            if prompted.is_empty() {
                eprintln!("error: name is required");
                std::process::exit(1);
            }
            prompted
        }
    };

    match kind {
        "handler" => {
            handler::run(&name);
        }
        "service" => {
            service::run(&name);
        }
        "model" => {
            model::run(&name);
        }
        "middleware" => {
            middleware::run(&name);
        }
        "resource" => {
            resource::run(&name);
        }
        _ => {
            eprintln!("error: unknown generator `{kind}`");
            eprintln!("available generators: handler, service, model, middleware, resource");
            std::process::exit(1);
        }
    }
}
