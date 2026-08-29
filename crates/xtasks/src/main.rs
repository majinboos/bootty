fn main() {
    if let Err(error) = xtasks::run() {
        eprintln!("{error:#}");
        let exit_code = error
            .downcast_ref::<xtasks::benchmark::power::CommandFailure>()
            .map(xtasks::benchmark::power::CommandFailure::exit_code)
            .or_else(|| {
                error
                    .downcast_ref::<xtasks::cancellation::Interrupted>()
                    .map(|_| 130)
            })
            .unwrap_or(1);
        std::process::exit(exit_code);
    }
}
