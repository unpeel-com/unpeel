//! Compatibility diagnostic for the removed engine installer.
pub fn run(args: &[String]) -> i32 {
    let reason = unpeel_serve::computer::RETIRED_REASON;
    if args.iter().any(|arg| arg == "--json") {
        println!(
            "{}",
            serde_json::json!({"state": "removed", "error": reason})
        );
    } else {
        eprintln!("{reason}");
    }
    1
}
