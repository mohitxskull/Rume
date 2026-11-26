//! Optional tonic gRPC integration with W3C TraceContext propagation and
//! `AppError` mapping for Rume.

#![forbid(unsafe_code)]

/// gRPC server utilities.
///
/// TODO: Milestone E — implement gRPC server builder, interceptors, and
/// W3C TraceContext propagation.
pub mod server {
    /// Placeholder for the gRPC server builder.
    pub fn todo_server() {
        todo!("Milestone E: gRPC server builder")
    }
}

/// gRPC client utilities.
///
/// TODO: Milestone E — implement gRPC client helpers with retry and tracing.
pub mod client {
    /// Placeholder for the gRPC client builder.
    pub fn todo_client() {
        todo!("Milestone E: gRPC client builder")
    }
}
