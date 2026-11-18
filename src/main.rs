// musl's allocator serializes threads, which made parallel directory walks
// several times slower in the static build; mimalloc brings them back to
// glibc speed.
#[cfg(target_env = "musl")]
#[global_allocator]
static ALLOC: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn main() -> std::process::ExitCode {
    stomp::cli::run()
}
