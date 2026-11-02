use {{crate_name}}::app;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    app::run().await
}
