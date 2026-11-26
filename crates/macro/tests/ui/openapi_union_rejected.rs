use rume_macros::HasOpenApiSchema;

#[derive(HasOpenApiSchema)]
union BadUnion {
    a: u32,
    b: f32,
}

fn main() {}
