//! Time extraction per file and print the slowest: `cargo run --release -p kc-extract --example slowest -- files...`.
use std::time::Instant;

fn main() {
    let mut times: Vec<(f64, String, usize, u32)> = std::env::args()
        .skip(1)
        .filter_map(|p| {
            let src = std::fs::read_to_string(&p).ok()?;
            let t = Instant::now();
            let facts = kc_extract::extract(kc_extract::Lang::Swift, &src);
            Some((t.elapsed().as_secs_f64() * 1000.0, p, src.len(), facts.parse_errors))
        })
        .collect();
    times.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
    for (ms, path, bytes, errors) in times.iter().take(10) {
        println!("{ms:>9.1} ms  {bytes:>8} B  {errors:>4} errors  {path}");
    }
}
