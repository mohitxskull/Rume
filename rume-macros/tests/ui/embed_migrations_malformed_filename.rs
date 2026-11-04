use rume_macros::embed_migrations;

embed_migrations!("tests/fixtures/malformed_migrations");

fn main() {}
