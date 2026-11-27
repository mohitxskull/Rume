use rume_macros::HasOpenApiSchema;

#[derive(HasOpenApiSchema)]
struct BadTuple(String, i32);

fn main() {}
