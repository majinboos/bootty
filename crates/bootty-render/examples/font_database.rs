use std::time::Instant;

fn main() {
    let start = Instant::now();
    let database = bootty_render::font_database::load_system_font_database();
    println!(
        "faces={} elapsed_ms={:.3}",
        database.faces().count(),
        start.elapsed().as_secs_f64() * 1_000.0,
    );
}
